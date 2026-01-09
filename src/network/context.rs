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

    /// 乐观执行器 (带自动恢复)
    /// 
    /// 封装"试错-等待-重试"逻辑。
    /// 如果遇到 IP 封禁或 Cloudflare 阻断，会自动触发恢复流程（切 IP / 等待）并无限重试，
    /// 直到任务成功或遇到不可恢复的错误。
    pub async fn run_optimistic<F, Fut, T>(&self, desc: impl std::fmt::Display, task: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut attempts = 0;
        let max_retries = 10;
        // 指数退避参数
        let base_delay = Duration::from_millis(500);
        let max_delay = Duration::from_secs(5);

        loop {
            attempts += 1;
            
            match task().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // 1. 检查是否为阻断性错误 (IP 封禁 / Cloudflare)
                    if let Some(reason) = e.is_blocking() {
                        info!("任务遭遇阻断 [{}] ({})，正在执行自动恢复...", desc, reason);
                        
                        // 针对性恢复策略
                        if reason.contains("HTTP 403") || reason.contains("HTTP 429") {
                            self.rotate_proxy().await;
                        }
                        
                        self.wait_if_blocked().await;
                        attempts = 0; // 环境已重置，重置尝试计数
                        continue;
                    }

                    // 2. 达到最大重试次数，放弃
                    if attempts >= max_retries {
                        return Err(e);
                    }

                    // 3. 计算指数退避时间 (Exponential Backoff with Jitter)
                    // delay = min(max_delay, base * 2^(attempts-1))
                    let backoff = base_delay.checked_mul(1 << (attempts - 1)).unwrap_or(max_delay).min(max_delay);
                    // 添加 10% 的随机抖动，防止共振 (Simple pseudo-random using system time)
                    let nanos = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .subsec_nanos();
                    let random_factor = (nanos % 100) as f32 / 1000.0; // 0.000 to 0.099
                    let jitter = backoff.mul_f32(random_factor); 
                    let wait_time = backoff + jitter;

                    self.wait_if_blocked().await;

                    warn!(
                        "任务失败 [{}] (第 {}/{} 次): {}。将在 {:?} 后重试...",
                        desc, attempts, max_retries, e, wait_time
                    );
                    tokio::time::sleep(wait_time).await;
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
                    // 切换 IP 后，必须清除旧会话 (Cookie/UA)，确保身份彻底刷新
                    self.session.clear();
                    
                    // 重置 HTTP 客户端 (清除旧的连接池)
                    // 否则 Keep-Alive 的连接可能会连到旧的 IP (取决于 sing-box 行为)
                    if let Err(e) = self.http.recreate_client() {
                        warn!("重置 HTTP 客户端失败: {}", e);
                    }
                }
                Err(_) => warn!("代理切换响应超时"),
            }
        }
    }

    /// 强制切换代理（不经过协调器）
    pub async fn force_rotate_proxy(&self) {
        self.do_rotate_proxy().await;
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