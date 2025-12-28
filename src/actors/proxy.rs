use std::path::{Path, PathBuf};
use std::sync::Arc;

use flume::{Receiver, Sender};
use serde_json;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::core::config::AppConfig;
use crate::utils::singbox::SingBoxController;
use crate::utils::subscription::{self, ProxyNode};

pub enum ProxyMsg {
    GetNode { reply: Sender<Option<ProxyNode>> },
    ReportSuccess { tag: String, latency: u128 },
    ReportFailure { tag: String },
    Rotate { reply: Option<tokio::sync::oneshot::Sender<()>> },
}

pub struct ProxyManager {
    rx: Receiver<ProxyMsg>,
    controller: Arc<SingBoxController>,
    nodes: Vec<ProxyNode>,
    cache_path: PathBuf,
}

impl ProxyManager {
    pub async fn start(config: Arc<AppConfig>) -> (Sender<ProxyMsg>, JoinHandle<()>) {
        let (tx, rx) = flume::unbounded();

        let bin_path = Path::new(&config.singbox.bin_path);
        let cache_path = Path::new(&config.cache_path);

        let mut nodes =
            subscription::fetch_subscription_urls(&config.singbox.subscription_urls, cache_path)
                .await
                .unwrap_or_else(|e| {
                    error!("获取订阅失败: {}", e);
                    vec![]
                });
        
        debug!("成功获取 {} 个代理节点", nodes.len());
        info!("代理服务初始化完成");

        let order_file = cache_path.join("proxy_order.json");
        if order_file.exists() {
            if let Ok(json) = std::fs::read_to_string(&order_file) {
                if let Ok(ordered_tags) = serde_json::from_str::<Vec<String>>(&json) {
                    debug!("加载缓存的代理排序 ({} 条记录)", ordered_tags.len());

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
            error!("生成配置文件失败: {}", e);
            String::new()
        });

        let controller = Arc::new(
            SingBoxController::new(
                bin_path,
                cache_path,
                config.singbox.api_port,
                &config.singbox.api_secret,
            )
            .expect("无法初始化 SingBoxController"),
        );

        if !nodes.is_empty() {
            if let Err(e) = controller.write_config(&config_content).await {
                error!("写入配置文件失败: {}", e);
            }
            if let Err(e) = controller.start().await {
                error!("启动 sing-box 失败: {}", e);
            }
            if let Err(e) = controller.wait_for_api(10).await {
                error!("sing-box API 响应超时: {}", e);
            }
        }

        let mut actor = ProxyManager {
            rx,
            controller,
            nodes,
            cache_path: cache_path.to_path_buf(),
        };

        if !actor.nodes.is_empty() {
            // 启动时自动切换到下一个节点
            actor.rotate_and_save().await;
        }

        let handle = tokio::spawn(async move {
            actor.run().await;
        });

        (tx, handle)
    }

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

                    if let Some(current) = self.nodes.first() {
                        if current.tag() == tag {
                            warn!("代理节点 {} 失效，正在切换并调低优先级...", tag);
                            self.rotate_and_save().await;
                        } else {
                            debug!("忽略过期的失效报告: {} (当前节点: {})", tag, current.tag());
                        }
                    }
                }
                ProxyMsg::Rotate { reply } => {
                    warn!("收到强制切换代理请求...");
                    self.rotate_and_save().await;
                    if let Some(tx) = reply {
                        let _ = tx.send(());
                    }
                }
                ProxyMsg::ReportSuccess { .. } => {}
            }
        }
    }

    async fn rotate_and_save(&mut self) {
        if self.nodes.len() < 2 {
            return;
        }

        self.nodes.rotate_left(1);

        if let Some(new_head) = self.nodes.first() {
            info!("正在自动切换代理节点...");
            debug!("切换至节点: {}", new_head.tag());
            if let Err(e) = self
                .controller
                .switch_selector("proxy_selector", new_head.tag())
                .await
            {
                error!("切换代理失败: {}", e);
            }
        }

        self.save_order().await;
    }

    async fn save_order(&self) {
        let tags: Vec<String> = self.nodes.iter().map(|n| n.tag().to_string()).collect();
        let path = self.cache_path.join("proxy_order.json");

        if let Ok(json) = serde_json::to_string_pretty(&tags) {
            if let Err(e) = tokio::fs::write(&path, json).await {
                warn!("保存代理排序失败: {}", e);
            } else {
                debug!("代理排序已保存");
            }
        }
    }
}