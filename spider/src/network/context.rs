//! 服务上下文 (Service Context)
//!
//! 集成状态协调、故障恢复和乐观重试机制的核心副作用管理器。

use std::sync::Arc;
use std::time::Duration;

use flume::Sender;
use rand::Rng;
use reqwest::Method;
use reqwest_middleware::RequestBuilder;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::actors::proxy::ProxyMsg;
use crate::core::config::AppConfig;
use crate::core::coordinator::Coordinator;
use crate::core::error::{BlockReason, Result, SpiderError};
use crate::core::event::{EventSender, SpiderEvent};
use crate::network::browser::BrowserService;
use crate::network::service::HttpService;
use crate::network::session::Session;

/// 运行时服务上下文
///
/// 封装网络栈、会话持久化、代理枢纽及浏览器自动化接口，提供高可用的任务执行环境。
#[derive(Clone)]
pub struct ServiceContext {
    /// 核心 HTTP 引擎
    pub http: Arc<HttpService>,
    /// 同步会话状态
    pub session: Arc<Session>,
    /// 代理调度信道
    pub proxy: Sender<ProxyMsg>,
    /// 浏览器自动化服务
    pub browser: Arc<BrowserService>,
    /// 静态应用配置
    pub config: Arc<AppConfig>,
    /// 状态协调器
    pub coordinator: Coordinator,
    /// 生命周期取消令牌
    pub shutdown: CancellationToken,
    /// 事件上报总线
    pub events: Option<EventSender>,
}

impl ServiceContext {
    const MAX_RETRIES: u32 = 10;
    const MAX_GLOBAL_RETRIES: u32 = 50;
    const BASE_DELAY: Duration = Duration::from_millis(500);
    const MAX_DELAY: Duration = Duration::from_secs(5);
    const PROXY_ROTATION_TIMEOUT: Duration = Duration::from_secs(30);
    const RECOVERY_JITTER_MS: u64 = 50;

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

    pub fn with_events(mut self, events: EventSender) -> Self {
        self.events = Some(events);
        self
    }

    pub fn emit(&self, event: SpiderEvent) {
        if let Some(ref sender) = self.events {
            sender.emit(event);
        }
    }


    pub fn request_builder(&self, method: Method, url: &str) -> RequestBuilder {
        self.request_builder_with_client(self.http.client(), method, url)
    }

    pub fn request_builder_with_client(
        &self,
        client: reqwest_middleware::ClientWithMiddleware,
        method: Method,
        url: &str,
    ) -> RequestBuilder {
        client
            .request(method, url)
            .with_extension(self.session.clone())
            .with_extension(self.clone())
    }

    pub async fn probe(&self, url: &str) -> Result<(u16, String)> {
        self.http.probe(url, self.clone()).await
    }

    /// 乐观任务执行器
    ///
    /// 实现"试错-反馈-恢复"闭环逻辑，集成如下特性：
    /// - **Pre-flight Check**: 状态抢占检查，防止在阻塞期间浪费流量
    /// - **Anti-Thundering Herd**: 引入抖动 (Jitter) 缓解恢复瞬间的共振压测
    /// - **Auto Recovery**: 检测到特定 BlockReason 时自动触发代理轮换或浏览器挑战
    /// - **Equal Jitter Backoff**: 指数退避策略，兼顾重试间隔与负载平滑
    pub async fn run_optimistic<F, Fut, T>(
        &self,
        desc: impl std::fmt::Display,
        task: F,
    ) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut attempts = 0;
        let mut total_attempts = 0;

        loop {
            self.check_shutdown()?;

            if self.wait_if_blocked().await {
                self.apply_jitter().await;
            }

            attempts += 1;
            total_attempts += 1;

            match task().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let should_retry = self.handle_error(&desc, &e, &mut attempts, total_attempts).await?;
                    if should_retry {
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    fn check_shutdown(&self) -> Result<()> {
        if self.shutdown.is_cancelled() {
            return Err(SpiderError::Custom("Cancellation: System shutdown".into()));
        }
        Ok(())
    }

    async fn apply_jitter(&self) {
        let jitter = Duration::from_millis(rand::rng().random_range(0..Self::RECOVERY_JITTER_MS));
        tokio::time::sleep(jitter).await;
    }

    async fn handle_error(
        &self,
        desc: &impl std::fmt::Display,
        e: &SpiderError,
        attempts: &mut u32,
        total_attempts: u32,
    ) -> Result<bool> {
        if total_attempts >= Self::MAX_GLOBAL_RETRIES {
            warn!(
                "Task [{}] exceeded global budget ({}), aborting.",
                desc, Self::MAX_GLOBAL_RETRIES
            );
            return Ok(false);
        }

        if let Some(reason) = e.is_blocking() {
            self.handle_blocking_error(desc, reason).await;
            *attempts = 0;
            return Ok(true);
        }

        if *attempts >= Self::MAX_RETRIES {
            return Ok(false);
        }

        self.backoff_and_retry(desc, e, *attempts, total_attempts).await
    }

    async fn handle_blocking_error(&self, desc: &impl std::fmt::Display, reason: BlockReason) {
        info!("Task blocked [{}] ({}), initiating recovery...", desc, reason);

        match reason {
            BlockReason::IpBlocked | BlockReason::RateLimit => {
                self.rotate_proxy().await;
            }
            _ => {}
        }
    }

    async fn backoff_and_retry(
        &self,
        desc: &impl std::fmt::Display,
        e: &SpiderError,
        attempts: u32,
        total_attempts: u32,
    ) -> Result<bool> {
        let wait_time = Self::calculate_backoff(attempts);

        warn!(
            "Task failed [{}] ({}/{} | total {}): {}. Retrying in {:?}...",
            desc, attempts, Self::MAX_RETRIES, total_attempts, e, wait_time
        );

        tokio::select! {
            _ = tokio::time::sleep(wait_time) => {},
            _ = self.shutdown.cancelled() => {
                return Err(SpiderError::Custom("Cancellation: Interrupted".into()));
            }
        }

        Ok(true)
    }


    fn calculate_backoff(attempts: u32) -> Duration {
        let exp = attempts.saturating_sub(1).min(30);
        let ceil = Self::BASE_DELAY
            .checked_mul(1 << exp)
            .unwrap_or(Self::MAX_DELAY)
            .min(Self::MAX_DELAY);

        Self::equal_jitter(ceil)
    }

    fn equal_jitter(ceil: Duration) -> Duration {
        let half = ceil / 2;
        let jitter_nanos = rand::rng().random_range(0..=half.as_nanos().max(0) as u64);
        half + Duration::from_nanos(jitter_nanos)
    }

    pub async fn rotate_proxy(&self) {
        if let Some(_guard) = self
            .coordinator
            .try_acquire_fix(BlockReason::IpBlocked)
            .await
        {
            self.do_rotate_proxy().await;
        }
    }

    async fn do_rotate_proxy(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();

        if self.proxy.send(ProxyMsg::Rotate { reply: Some(tx) }).is_ok() {
            match tokio::time::timeout(Self::PROXY_ROTATION_TIMEOUT, rx).await {
                Ok(_) => self.on_proxy_rotation_success(),
                Err(_) => warn!("Proxy rotation timeout"),
            }
        }
    }

    fn on_proxy_rotation_success(&self) {
        debug!("Proxy rotation confirmed by actor");
        self.session.clear();

        if let Err(e) = self.http.recreate_client() {
            warn!("Failed to recreate client: {}", e);
        }
    }

    pub async fn force_rotate_proxy(&self) {
        self.do_rotate_proxy().await;
    }

    pub fn update_cookies(&self, cookies: &str) {
        self.session.set_cookie(cookies.to_string());
        debug!("Session credentials updated");
    }

    pub async fn bypass_cloudflare(&self, url: &str) -> Result<()> {
        if let Some(_guard) = self
            .coordinator
            .try_acquire_fix(BlockReason::Cloudflare)
            .await
        {
            info!("Initiating browser-based Cloudflare bypass...");
            self.browser.bypass(url, self).await?;
        }

        Ok(())
    }

    pub async fn wait_if_blocked(&self) -> bool {
        if !self.coordinator.is_running() {
            debug!("System blocked, awaiting recovery...");
            self.coordinator.wait_until_running().await;
            true
        } else {
            false
        }
    }

    pub async fn wait_if_blocked_timeout(&self, timeout: Duration) -> bool {
        if !self.coordinator.is_running() {
            debug!("System blocked, awaiting recovery (timeout: {:?})...", timeout);
            self.coordinator.wait_until_running_timeout(timeout).await
        } else {
            true
        }
    }
}