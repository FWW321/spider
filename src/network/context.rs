//! 全局服务上下文 (Service Context)
//!
//! 核心副作用管理器，集成分布式状态协调、故障恢复流水线及乐观重试机制。

use std::sync::Arc;
use std::time::Duration;

use flume::Sender;
use reqwest::Method;
use reqwest_middleware::RequestBuilder;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::actors::proxy::ProxyMsg;
use crate::core::config::AppConfig;
use crate::core::coordinator::Coordinator;
use crate::core::error::{BlockReason, Result};
use crate::core::event::EventSender;
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
    /// 生命周期撤回令牌
    pub shutdown: CancellationToken,
    /// 事件上报总线
    pub events: Option<EventSender>,
}

impl ServiceContext {
    /// 单出口节点重试阈值
    const MAX_RETRIES: u32 = 10;
    /// 任务全局生存预算 (防止循环依赖导致的任务挂死)
    const MAX_GLOBAL_RETRIES: u32 = 50;
    /// 指数退避基准时延
    const BASE_DELAY: Duration = Duration::from_millis(500);
    /// 指数退避上限
    const MAX_DELAY: Duration = Duration::from_secs(5);

    /// 初始化服务上下文
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

    /// 注入事件发送器
    pub fn with_events(mut self, events: EventSender) -> Self {
        self.events = Some(events);
        self
    }

    /// 向总线分发系统事件
    pub fn emit(&self, event: crate::core::event::SpiderEvent) {
        if let Some(ref sender) = self.events {
            sender.emit(event);
        }
    }

    // =========================================================================
    // HTTP 请求方法
    // =========================================================================

    /// 构建注入会话特征的请求构造器
    pub fn request_builder(&self, method: Method, url: &str) -> RequestBuilder {
        self.request_builder_with_client(self.http.client(), method, url)
    }

    /// 基于特定客户端实例构建请求上下文
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

    /// 执行绕过拦截策略的健康探测
    pub async fn probe(&self, url: &str) -> Result<(u16, String)> {
        self.http.probe(url, self.clone()).await
    }

    /// 乐观任务执行器 (Optimistic Executor)
    ///
    /// 实现“试错-反馈-恢复”闭环逻辑，集成如下特性：
    /// - **Pre-flight Check**: 状态抢占检查，防止在阻塞期间浪费流量。
    /// - **Anti-Thundering Herd**: 引入抖动 (Jitter) 缓解恢复瞬间的共振压测。
    /// - **Auto Recovery**: 检测到特定 BlockReason 时自动触发代理轮换或浏览器挑战。
    /// - **Equal Jitter Backoff**: 指数退避策略，兼顾重试间隔与负载平滑。
    pub async fn run_optimistic<F, Fut, T>(
        &self,
        desc: impl std::fmt::Display,
        task: F,
    ) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        // 局部重试计数 (用于计算当前 IP 的退避时间，切 IP 后会重置)
        let mut attempts = 0;
        // 全局重试计数 (用于防止无限循环，永远不重置)
        let mut total_attempts = 0;

        loop {
            // 0. 优雅退出检查
            if self.shutdown.is_cancelled() {
                return Err(crate::core::error::SpiderError::Custom("Cancellation: System shutdown".into()));
            }

            // 1. 起飞前检查 (Pre-flight Check)
            // 如果系统 Blocked，在此挂起。防止无效请求风暴。
            // 返回值 waited 指示是否经历了阻塞。
            let waited = self.wait_if_blocked().await;

            // 2. 唤醒后微抖动 (Anti-Thundering Herd)
            // 只有当真正发生过等待（即系统从 Blocked 恢复为 Running）时，才应用随机抖动。
            // 正常运行时的第一次请求不会触发此逻辑。
            if waited {
                 tokio::time::sleep(Duration::from_millis(fastrand::u64(0..50))).await;
            }

            attempts += 1;
            total_attempts += 1;

            match task().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // 全局熔断检查
                    if total_attempts >= Self::MAX_GLOBAL_RETRIES {
                        warn!("Task [{}] exceeded global budget ({}), aborting.", desc, Self::MAX_GLOBAL_RETRIES);
                        return Err(e);
                    }

                    // 3. 阻断性错误处理
                    if let Some(reason) = e.is_blocking() {
                        info!("Task blocked [{}] ({}), initiating recovery...", desc, reason);

                        match reason {
                            BlockReason::IpBlocked | BlockReason::RateLimit => {
                                self.rotate_proxy().await;
                            }
                            _ => {}
                        }

                        // 环境重置后，清空局部计数器，以便新 IP 从短时间等待开始试探
                        attempts = 0;
                        continue;
                    }

                    // 4. 局部最大重试检查
                    if attempts >= Self::MAX_RETRIES {
                        return Err(e);
                    }

                    // 5. 指数退避 (可中断)
                    let wait_time = Self::calculate_backoff(attempts);
                    
                    warn!(
                        "Task failed [{}] ({}/{} | total {}): {}. Retrying in {:?}...",
                        desc, attempts, Self::MAX_RETRIES, total_attempts, e, wait_time
                    );

                    // 使用 select! 确保在休眠期间也能响应退出信号
                    tokio::select! {
                        _ = tokio::time::sleep(wait_time) => {},
                        _ = self.shutdown.cancelled() => {
                            return Err(crate::core::error::SpiderError::Custom("Cancellation: Interrupted".into()));
                        }
                    }
                }
            }
        }
    }

    /// 计算退避时间 (Equal Jitter 策略)
    fn calculate_backoff(attempts: u32) -> Duration {
        // 防止溢出，限制指数上限
        let exp = (attempts.saturating_sub(1)).min(30);
        let ceil = Self::BASE_DELAY
            .checked_mul(1 << exp)
            .unwrap_or(Self::MAX_DELAY)
            .min(Self::MAX_DELAY);
        
        // Equal Jitter implementation
        let half = ceil / 2;
        // fastrand::u64(0..v) 生成 [0, v)
        let jitter_nanos = fastrand::u64(0..half.as_nanos() as u64);
        
        half + Duration::from_nanos(jitter_nanos)
    }

    // =========================================================================
    // 恢复操作
    // =========================================================================

    /// 调度代理节点轮换
    /// 
    /// 采用协调器抢占模式，确保并发环境下仅执行一次切换动作。
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

    /// 物理执行代理切换及环境重置
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
                    debug!("Proxy rotation confirmed by actor");
                    // 切换 IP 后，必须清除旧会话 (Cookie/UA)，确保身份彻底刷新
                    self.session.clear();

                    // 重置 HTTP 客户端 (清除旧的连接池)
                    // 否则 Keep-Alive 的连接可能会连到旧的 IP (取决于 sing-box 行为)
                    if let Err(e) = self.http.recreate_client() {
                        warn!("Failed to recreate client: {}", e);
                    }
                }
                Err(_) => warn!("Proxy rotation timeout"),
            }
        }
    }

    /// 强行触发出口节点变更
    pub async fn force_rotate_proxy(&self) {
        self.do_rotate_proxy().await;
    }

    /// 原子化更新会话凭据
    pub fn update_cookies(&self, cookies: &str) {
        self.session.set_cookie(cookies.to_string());
        debug!("Session credentials updated");
    }

    /// 激活浏览器自动化绕过 Cloudflare 挑战
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

    /// 检查并同步等待系统恢复至运行态
    pub async fn wait_if_blocked(&self) -> bool {
        if !self.coordinator.is_running() {
            debug!("System blocked, awaiting recovery...");
            self.coordinator.wait_until_running().await;
            true
        } else {
            false
        }
    }

    /// 检查并等待系统恢复 (带超时)
    pub async fn wait_if_blocked_timeout(&self, timeout: Duration) -> bool {
        if !self.coordinator.is_running() {
            debug!("System blocked, awaiting recovery (timeout: {:?})...", timeout);
            self.coordinator.wait_until_running_timeout(timeout).await
        } else {
            true
        }
    }
}
