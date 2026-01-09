//! 代理调度 Actor (Proxy Scheduling Actor)
//!
//! 负责代理节点的状态监测、健康分、轮换策略及底层选择器同步。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use flume::{Receiver, Sender};
use serde_json;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::core::config::AppConfig;
use crate::utils::singbox::SingBoxController;
use crate::utils::subscription::{self, ProxyNode};

/// Actor Messages
pub enum ProxyMsg {
    /// 获取当前活跃节点
    GetNode {
        reply: Sender<Option<ProxyNode>>,
    },
    /// 汇报请求成功，用于性能建模 (目前仅占位)
    ReportSuccess {
        tag: String,
        latency: u128,
    },
    /// 汇报请求失败，触发节点降级与轮换
    ReportFailure {
        tag: String,
    },
    /// 强制触发出口 IP 轮换
    Rotate {
        reply: Option<tokio::sync::oneshot::Sender<()>>,
    },
}

/// Actor Implementation
pub struct ProxyManager {
    /// 消息接收端
    rx: Receiver<ProxyMsg>,
    /// 底层进程控制器
    controller: Arc<SingBoxController>,
    /// 节点池 (基于权重/健康分排序)
    nodes: Vec<ProxyNode>,
    /// 配置与状态持久化路径
    cache_path: PathBuf,
}

impl ProxyManager {
    /// 启动代理调度 Actor 并初始化底层进程
    pub async fn start(config: Arc<AppConfig>) -> (Sender<ProxyMsg>, JoinHandle<()>) {
        let (tx, rx) = flume::unbounded();

        let bin_path = Path::new(&config.singbox.bin_path);
        let cache_path = Path::new(&config.cache_path);

        // 订阅同步与预处理 (Subscription Ingestion)
        let mut nodes =
            subscription::fetch_subscription_urls(&config.singbox.subscription_urls, cache_path)
                .await
                .unwrap_or_else(|e| {
                    error!("Subscription fetch error: {}", e);
                    vec![]
                });

        debug!("Fetched {} proxy nodes", nodes.len());
        info!("Proxy scheduler initialized");

        // 加载持久化的节点优先级 (Priority Recovery)
        let order_file = cache_path.join("proxy_order.json");
        if order_file.exists() {
            if let Ok(json) = tokio::fs::read_to_string(&order_file).await {
                if let Ok(ordered_tags) = serde_json::from_str::<Vec<String>>(&json) {
                    debug!("Recovering node priorities ({} entries)", ordered_tags.len());

                    let mut reordered = Vec::with_capacity(nodes.len());
                    let mut node_map: std::collections::HashMap<String, ProxyNode> =
                        nodes.drain(..).map(|n| (n.tag().to_string(), n)).collect();

                    for tag in ordered_tags {
                        if let Some(node) = node_map.remove(&tag) {
                            reordered.push(node);
                        }
                    }
                    reordered.extend(node_map.into_values());
                    nodes = reordered;
                }
            }
        }

        let config_content = subscription::generate_singbox_config(
            &nodes,
            config.singbox.proxy_port,
            config.singbox.api_port,
            &config.singbox.api_secret,
            cache_path,
        )
        .unwrap_or_else(|e| {
            error!("Config generation error: {}", e);
            String::new()
        });

        let controller = Arc::new(
            SingBoxController::new(
                bin_path,
                cache_path,
                config.singbox.api_port,
                &config.singbox.api_secret,
            )
            .expect("Failed to initialize process controller"),
        );

        // Hot Start
        if !nodes.is_empty() {
            if let Err(e) = controller.write_config(&config_content).await {
                error!("Write config failed: {}", e);
            }
            if let Err(e) = controller.start().await {
                error!("Start artifact failed: {}", e);
            }
            if let Err(e) = controller.wait_for_api(10).await {
                error!("API readiness timeout: {}", e);
            }
        }

        let mut actor = ProxyManager {
            rx,
            controller,
            nodes,
            cache_path: cache_path.to_path_buf(),
        };

        if !actor.nodes.is_empty() {
            actor.rotate_and_save().await;
        }

        let handle = tokio::spawn(async move {
            actor.run().await;
        });

        (tx, handle)
    }

    /// Actor 消息循环 (Event Loop)
    async fn run(&mut self) {
        while let Ok(msg) = self.rx.recv_async().await {
            match msg {
                ProxyMsg::GetNode { reply } => {
                    let _ = reply.send(self.nodes.first().cloned());
                }
                ProxyMsg::ReportFailure { tag } => {
                    if self.nodes.is_empty() {
                        continue;
                    }

                    // 节点惩罚逻辑 (Deprioritization)
                    if let Some(current) = self.nodes.first() {
                        if current.tag() == tag {
                            warn!("Node [{}] failure detected, deprioritizing...", tag);
                            self.rotate_and_save().await;
                        } else {
                            debug!("Ignored stale failure report: {} (active: {})", tag, current.tag());
                        }
                    }
                }
                ProxyMsg::Rotate { reply } => {
                    warn!("Forced rotation triggered");
                    self.rotate_and_save().await;
                    if let Some(tx) = reply {
                        let _ = tx.send(());
                    }
                }
                ProxyMsg::ReportSuccess { .. } => {}
            }
        }
    }

    /// 执行节点切换并持久化状态
    async fn rotate_and_save(&mut self) {
        if self.nodes.len() < 2 {
            return;
        }

        // 循环左移，将故障节点移至队尾 (Round-robin with Deprioritization)
        self.nodes.rotate_left(1);

        if let Some(new_head) = self.nodes.first() {
            info!("Rotating to new exit node...");
            debug!("Selected node: {}", new_head.tag());
            if let Err(e) = self
                .controller
                .switch_selector("proxy_selector", new_head.tag())
                .await
            {
                error!("Selector switch failed: {}", e);
            }
        }

        self.save_order().await;
    }

    /// 持久化节点排序，确保跨进程生存期一致性
    async fn save_order(&self) {
        let tags: Vec<String> = self.nodes.iter().map(|n| n.tag().to_string()).collect();
        let path = self.cache_path.join("proxy_order.json");

        if let Ok(json) = serde_json::to_string_pretty(&tags) {
            if let Err(e) = tokio::fs::write(&path, json).await {
                warn!("Save proxy state error: {}", e);
            } else {
                debug!("Proxy state persisted");
            }
        }
    }
}
