//! 代理管理器 (Proxy Manager)
//!
//! 负责代理服务的生命周期管理、节点轮换及流量转发规则的动态更新。
//! 使用 shoes 库在进程内直接运行代理核心，无需外部二进制依赖。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, Context as _};
use flume::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use shoes::address::{Address, NetLocation};
use shoes::client_proxy_selector::{ClientProxySelector, ReloadableProxySelector};
use shoes::config::{
    BindLocation, ClientConfig, ServerConfig,
    ServerProxyConfig, Transport, TcpConfig,
};
use shoes::option_util::NoneOrSome;
use shoes::resolver::{NativeResolver, Resolver};
use shoes::network::tcp::tcp_server::start_servers;
use shoes::ProxySwapper;

use crate::core::config::AppConfig;
use crate::utils::subscription::{ProxyNode, fetch_subscription_urls};

/// 代理控制指令
#[derive(Debug)]
pub enum ProxyMsg {
    /// 切换到下一个可用节点
    Rotate {
        reply: Option<oneshot::Sender<()>>,
    },
    /// 暂停/恢复代理服务 (预留)
    Pause,
    Resume,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProxyState {
    last_node_fingerprint: String,
}

pub struct ProxyManager {
    config: Arc<AppConfig>,
    rx: Receiver<ProxyMsg>,
    selector: Arc<ReloadableProxySelector>,
    resolver: Arc<dyn Resolver>,
    swapper: ProxySwapper,
    nodes: Vec<(String, ClientConfig)>,
    current_node_index: usize,
}

impl ProxyManager {
    /// 启动代理管理器 Actor
    pub async fn start(config: Arc<AppConfig>) -> (Sender<ProxyMsg>, JoinHandle<()>) {
        let (tx, rx) = flume::unbounded();
        let config_clone = config.clone();

        let handle = tokio::spawn(async move {
            let manager = Self::new(config_clone, rx);
            manager.run().await;
        });

        (tx, handle)
    }

    fn new(config: Arc<AppConfig>, rx: Receiver<ProxyMsg>) -> Self {
        let resolver = Arc::new(NativeResolver::new());
        // 初始为空选择器（使用直连，直到节点加载完成）
        use shoes::config::{ClientChain, ClientChainHop, ClientConfig, ConfigSelection};
        use shoes::utils::option::{NoneOrSome, OneOrSome};
        use shoes::network::tcp::chain_builder::build_client_chain_group;

        // 创建直连 chain group
        let chain_hop = ClientChainHop::Single(ConfigSelection::Config(ClientConfig::default()));
        let chain = ClientChain {
            hops: OneOrSome::One(chain_hop),
        };
        let chain_group = build_client_chain_group(NoneOrSome::Some(vec![chain]), resolver.clone());

        let selector = Arc::new(ReloadableProxySelector::new(
            ClientProxySelector::new_with_chain_group(chain_group)
        ));
        let swapper = ProxySwapper::new(selector.clone(), resolver.clone());

        Self {
            config,
            rx,
            selector,
            resolver,
            swapper,
            nodes: Vec::new(),
            current_node_index: 0,
        }
    }

    async fn run(mut self) {
        info!("Initializing internal proxy core (shoes)...");

        // 1. 获取订阅并解析节点
        match self.fetch_and_parse_nodes().await {
            Ok(nodes) => {
                info!("Loaded {} proxy nodes", nodes.len());
                self.nodes = nodes;
            }
            Err(e) => {
                error!("Failed to fetch subscription: {}", e);
            }
        }

        // 恢复上次的状态并跳过最后一个节点
        if !self.nodes.is_empty() {
            if let Some(state) = self.load_state() {
                // 查找上次使用的节点指纹对应的索引
                let last_index = self.nodes.iter().position(|(fp, _)| *fp == state.last_node_fingerprint);
                
                if let Some(i) = last_index {
                    // 如果找到了上次的节点，从下一个开始（跳过）
                    self.current_node_index = (i + 1) % self.nodes.len();
                    info!(
                        "Resuming from proxy node #{} (Last used fingerprint: {}, skipped)",
                        self.current_node_index, 
                        &state.last_node_fingerprint.chars().take(8).collect::<String>()
                    );
                } else {
                    // 如果找不到（节点列表变动），重置为0
                    self.current_node_index = 0;
                    info!(
                        "Last used node not found (Fingerprint: {}), resetting to node #0", 
                        &state.last_node_fingerprint.chars().take(8).collect::<String>()
                    );
                }
            } else {
                self.current_node_index = 0;
            }
            // 立即保存新的起始状态，防止启动即崩溃导致状态未更新
            self.save_state();
        }

        // 2. 启动本地监听服务
        if let Err(e) = self.start_local_server().await {
            error!("Failed to start local proxy server: {}", e);
            return;
        }

        // 3. 应用初始节点
        self.apply_node();

        // 4. 事件循环
        while let Ok(msg) = self.rx.recv_async().await {
            match msg {
                ProxyMsg::Rotate { reply } => {
                    self.rotate_node();
                    if let Some(tx) = reply {
                        let _ = tx.send(());
                    }
                }
                ProxyMsg::Pause => {
                    info!("Proxy service paused (not implemented)");
                }
                ProxyMsg::Resume => {
                    info!("Proxy service resumed (not implemented)");
                }
            }
        }
    }

    fn get_state_path(&self) -> PathBuf {
        PathBuf::from(&self.config.cache_path).join("proxy_state.json")
    }

    fn load_state(&self) -> Option<ProxyState> {
        let path = self.get_state_path();
        if path.exists() {
            if let Ok(file) = std::fs::File::open(&path) {
                if let Ok(state) = serde_json::from_reader(file) {
                    return Some(state);
                }
            }
        }
        None
    }

    fn save_state(&self) {
        if self.nodes.is_empty() {
            return;
        }

        let path = self.get_state_path();
        // 确保存储目录存在
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        
        if let Ok(file) = std::fs::File::create(&path) {
            // 保存当前节点的指纹
            let fingerprint = &self.nodes[self.current_node_index].0;
            let state = ProxyState {
                last_node_fingerprint: fingerprint.clone(),
            };
            let _ = serde_json::to_writer(file, &state);
        }
    }

    async fn fetch_and_parse_nodes(&self) -> Result<Vec<(String, ClientConfig)>> {
        let urls = &self.config.proxy.subscription_urls;
        if urls.is_empty() {
            return Ok(vec![]);
        }

        let cache_path = PathBuf::from(&self.config.cache_path);
        let proxy_nodes = fetch_subscription_urls(urls, &cache_path).await?;

        let client_configs: Vec<(String, ClientConfig)> = proxy_nodes
            .into_iter()
            .filter_map(|node| match node.to_shoes_config() {
                Ok(cfg) => Some((node.fingerprint(), cfg)),
                Err(e) => {
                    debug!("Skipping invalid node {}: {}", node.tag(), e);
                    None
                }
            })
            .collect();

        Ok(client_configs)
    }

    async fn start_local_server(&self) -> Result<()> {
        let port = self.config.proxy.proxy_port;
        // 绑定到 127.0.0.1
        let bind_addr = Address::Ipv4(Ipv4Addr::new(127, 0, 0, 1));
        let location = NetLocation::new(bind_addr, port);
        
        // 创建 Mixed (HTTP+SOCKS5) 服务端配置
        let server_config = ServerConfig {
            bind_location: BindLocation::Address(location.into()),
            protocol: ServerProxyConfig::Mixed {
                username: None,
                password: None,
                udp_enabled: true,
            },
            transport: Transport::Tcp,
            tcp_settings: Some(TcpConfig { no_delay: true }),
            quic_settings: None,
            rules: NoneOrSome::None,
        };

        // 启动服务，后台运行
        let _handles = start_servers(
            shoes::config::Config::Server(server_config),
            self.selector.clone(),
            self.resolver.clone(),
        ).await.map_err(|e| anyhow::anyhow!("Server start error: {}", e))?;

        info!("Local proxy listening on 127.0.0.1:{}", port);
        Ok(())
    }

    fn rotate_node(&mut self) {
        if self.nodes.is_empty() {
            warn!("No proxy nodes available to rotate");
            return;
        }

        self.current_node_index = (self.current_node_index + 1) % self.nodes.len();
        self.save_state(); // 保存新状态
        self.apply_node();
    }

    fn apply_node(&mut self) {
        if self.nodes.is_empty() {
            return;
        }

        let (_, node) = &self.nodes[self.current_node_index];
        let protocol_name = node.protocol.protocol_name();
        
        info!(
            "Rotating proxy to node #{} [Protocol: {}] - {}", 
            self.current_node_index, 
            protocol_name,
            node.address
        );

        // 使用 Swapper 一键切换
        self.swapper.swap_to_node(node);
    }
}
