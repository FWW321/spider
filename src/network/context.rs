//! 服务上下文 (ServiceContext)
//!
//! 统一管理所有副作用操作，包含 HTTP 服务、会话、代理、浏览器等。

use std::sync::Arc;
use std::time::Duration;

use flume::Sender;
use reqwest::Method;
use reqwest_middleware::RequestBuilder;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::actors::proxy::ProxyMsg;
use crate::core::config::AppConfig;
use crate::core::coordinator::{BlockReason, Coordinator};
use crate::core::error::Result;
use crate::core::event::EventSender;
use crate::network::browser::BrowserService;
use crate::network::service::HttpService;
use crate::network::session::Session;

/// 服务上下文
///
/// 封装了所有网络请求和系统恢复相关的操作
#[derive(Clone)]
pub struct ServiceContext {
    /// HTTP 服务
    pub http: Arc<HttpService>,
    /// 会话管理（Cookie、UA 等）
    pub session: Arc<Session>,
    /// 代理管理 Actor 通信
    pub proxy: Sender<ProxyMsg>,
    /// 浏览器服务
    pub browser: Arc<BrowserService>,
    /// 应用配置
    pub config: Arc<AppConfig>,
    /// 全局状态协调器
    pub coordinator: Coordinator,
    /// 优雅退出令牌
    pub shutdown: CancellationToken,
    /// 事件发送器（可选）
    pub events: Option<EventSender>,
}

impl ServiceContext {
    /// 创建新的服务上下文
    pub fn new(
        http: Arc<HttpService>,
        session: Arc<Session>,
        proxy: Sender<ProxyMsg>,
        browser: Arc<BrowserService>,
        config: Arc<AppConfig>,
    ) -> Self {
        Self {
            http,
            session,
            proxy,
            browser,
            config,
            coordinator: Coordinator::new(),
            shutdown: CancellationToken::new(),
            events: None,
        }
    }

    /// 设置事件发送器
    pub fn with_events(mut self, events: EventSender) -> Self {
        self.events = Some(events);
        self
    }

    /// 发送事件
    pub fn emit(&self, event: crate::core::event::SpiderEvent) {
        if let Some(ref sender) = self.events {
            sender.emit(event);
        }
    }

    // =========================================================================
    // HTTP 请求方法
    // =========================================================================

    /// 构造一个带有基础上下文但没有站点策略的请求构造器
    pub fn request_builder(&self, method: Method, url: &str) -> RequestBuilder {
        self.request_builder_with_client(self.http.client(), method, url)
    }

    /// 使用指定的客户端构造请求
    /// 
    /// 允许调用方传入特定的客户端实例（例如从 HttpService 获取的副本）
    pub fn request_builder_with_client(&self, client: reqwest_middleware::ClientWithMiddleware, method: Method, url: &str) -> RequestBuilder {
        client
            .request(method, url)
            .with_extension(self.session.clone())
            .with_extension(self.clone())
    }

    /// 探测请求，返回状态码和响应体
    /// 注意：此方法绕过反爬策略
    pub async fn probe(&self, url: &str) -> Result<(u16, String)> {
        self.http.probe(url, self.clone()).await
    }

    /// 乐观执行器
    /// 
    /// 封装"试错-等待-重试"的逻辑。
    pub async fn run_optimistic<F, Fut, T, E>(&self, desc: impl std::fmt::Display, task: F) -> std::result::Result<T, E>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, E>>,
        E: std::fmt::Display + Send + Sync + 'static, // 简化错误约束
    {
        let mut attempts = 0;
        let max_attempts = 10;

        loop {
            attempts += 1;
            
            match task().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if attempts >= max_attempts {
                        return Err(e);
                    }

                    // 核心逻辑：失败后检查系统状态。
                    self.wait_if_blocked().await;

                    let wait = std::time::Duration::from_millis(500 * attempts as u64);
                    warn!(
                        "任务失败 [{}] (第 {}/{} 次): {}。将在 {:?} 后重试...",
                        desc, attempts, max_attempts, e, wait
                    );
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    // =========================================================================
    // 恢复操作
    // =========================================================================

    /// 切换代理
    ///
    /// 通过协调器确保只有一个任务执行切换
    pub async fn rotate_proxy(&self) {
        // 尝试获取修复权限。如果已有其他人（或当前线程自己持有了不同原因的锁）正在修复，
        // 且状态不是 Running，try_acquire_fix 内部会根据情况等待。
        if let Some(_guard) = self
            .coordinator
            .try_acquire_fix(BlockReason::IpBlocked)
            .await
        {
            self.do_rotate_proxy().await;
        }
    }

    /// 实际执行代理切换
    async fn do_rotate_proxy(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .proxy
            .send(ProxyMsg::Rotate { reply: Some(tx) })
            .is_ok()
        {
            // 设定一个合理的超时，防止 ProxyActor 挂死导致全线崩溃
            match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
                Ok(_) => {
                    debug!("代理节点已完成物理切换");
                }
                Err(_) => warn!("代理切换响应超时"),
            }
        }
    }

    /// 强制切换代理（不经过协调器）
    pub async fn force_rotate_proxy(&self) {
        self.do_rotate_proxy().await;
    }

    /// 重置浏览器会话
    pub async fn reset_browser(&self) {
        self.session.clear();
        debug!("浏览器会话已重置");
    }

    /// 更新 Cookie
    pub fn update_cookies(&self, cookies: &str) {
        self.session.set_cookie(cookies.to_string());
        debug!("Cookie 已更新");
    }

    /// 绕过 Cloudflare
    pub async fn bypass_cloudflare(&self, url: &str) -> Result<()> {
        if let Some(_guard) = self
            .coordinator
            .try_acquire_fix(BlockReason::Cloudflare)
            .await
        {
            info!("正在通过浏览器绕过 Cloudflare...");
            self.browser.bypass(url, self).await?;
        }

        Ok(())
    }

    /// 等待系统恢复运行
    pub async fn wait_if_blocked(&self) {
        if !self.coordinator.is_running() {
            debug!("系统阻塞中，等待恢复...");
            self.coordinator.wait_until_running().await;
        }
    }

    /// 等待系统恢复运行（带超时）
    pub async fn wait_if_blocked_timeout(&self, timeout: Duration) -> bool {
        if !self.coordinator.is_running() {
            debug!("系统阻塞中，等待恢复（超时: {:?}）...", timeout);
            self.coordinator.wait_until_running_timeout(timeout).await
        } else {
            true
        }
    }
}