use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::header::{COOKIE, HeaderMap, HeaderValue, USER_AGENT};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::interfaces::NetworkPolicy;
use crate::network::context::ServiceContext;
use crate::network::middleware::{AntiBlockMiddleware, SessionMiddleware, SkipPolicy};
use crate::network::session::Session;

/// 异步 HTTP 服务层 (Service Layer)
///
/// 封装底层 `reqwest` 客户端，集成中间件管线并管理网络会话状态。
#[derive(Clone)]
pub struct HttpService {
    /// 支持热重载的中间件客户端
    client: Arc<RwLock<ClientWithMiddleware>>,
    #[allow(dead_code)]
    config: Arc<AppConfig>,
    #[allow(dead_code)]
    session: Arc<Session>,
}

impl HttpService {
    /// 初始化 HTTP 服务实例
    pub fn new(config: Arc<AppConfig>, session: Arc<Session>) -> Self {
        let client = Self::try_build_internal_client(&config, &session)
            .expect("CRITICAL: Failed to initialize network client");
        Self {
            client: Arc::new(RwLock::new(client)),
            config,
            session,
        }
    }

    /// 客户端热重载 (Hot Swapping)
    ///
    /// 在代理轮换或连接池失效时重建内部客户端实例。
    /// 利用 Arc 引用计数确保旧客户端在待处理任务完成后平滑释放。
    pub fn recreate_client(&self) -> Result<()> {
        let new_client = Self::try_build_internal_client(&self.config, &self.session)?;
        let mut writer = self.client.write().expect("HttpService lock poisoned");
        *writer = new_client;
        Ok(())
    }

    /// 内部客户端工厂函数 (Client Factory)
    ///
    /// 配置连接池、代理重定向、超时策略以及注入初始会话凭据。
    fn try_build_internal_client(
        config: &AppConfig,
        session: &Session,
    ) -> Result<ClientWithMiddleware> {
        let proxy_url = format!("http://127.0.0.1:{}", config.singbox.proxy_port);
        let mut headers = HeaderMap::new();

        // 基础凭据注入
        let base_headers = [
            (USER_AGENT, session.get_ua()),
            (COOKIE, session.get_cookie().unwrap_or_default()),
        ];

        headers.extend(
            base_headers
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .filter_map(|(k, v)| HeaderValue::from_str(&v).ok().map(|val| (k, val))),
        );

        // 批量同步 Session Headers
        headers.extend(
            session
                .get_headers()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );

        let client_builder = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy_url).map_err(SpiderError::Network)?)
            .default_headers(headers)
            .pool_max_idle_per_host(32)
            .tcp_nodelay(true) // 禁用 Nagle 算法以降低交互式请求延迟
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30));

        let client = client_builder.build().map_err(SpiderError::Network)?;

        Ok(ClientBuilder::new(client)
            .with(SessionMiddleware)
            .with(AntiBlockMiddleware)
            .build())
    }

    /// 获取当前活跃的客户端实例 (Thread-safe)
    pub fn client(&self) -> ClientWithMiddleware {
        self.client
            .read()
            .expect("HttpService lock poisoned")
            .clone()
    }

    /// 请求分发逻辑 (Request Dispatching)
    ///
    /// 利用中间件链处理重试、反爬检测及会话同步。
    pub async fn execute(
        &self,
        ctx: &ServiceContext,
        method: reqwest::Method,
        url: &str,
        policies: Vec<Arc<dyn NetworkPolicy>>,
    ) -> Result<reqwest::Response> {
        let client = self.client();

        let rb = ctx
            .request_builder_with_client(client, method, url)
            .with_extension(policies);

        rb.send().await.map_err(SpiderError::Middleware)
    }

    /// 连通性探测 (Connectivity Probe)
    ///
    /// 绕过所有拦截策略，用于执行基础设施层面的健康检查。
    pub async fn probe(&self, url: &str, ctx: ServiceContext) -> Result<(u16, String)> {
        let resp = self
            .client()
            .get(url)
            .with_extension(ctx.session.clone())
            .with_extension(ctx)
            .with_extension(SkipPolicy::All)
            .send()
            .await
            .map_err(SpiderError::Middleware)?;

        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(SpiderError::Network)?;
        Ok((status, text))
    }
}
