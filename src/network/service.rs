use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, COOKIE, USER_AGENT};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::interfaces::NetworkPolicy;
use crate::network::context::ServiceContext;
use crate::network::middleware::{AntiBlockMiddleware, SessionMiddleware, SkipPolicy};
use crate::network::session::Session;

#[derive(Clone)]
pub struct HttpService {
    client: Arc<RwLock<ClientWithMiddleware>>,
    #[allow(dead_code)]
    config: Arc<AppConfig>,
    #[allow(dead_code)]
    session: Arc<Session>,
}

impl HttpService {
    pub fn new(config: Arc<AppConfig>, session: Arc<Session>) -> Self {
        let client = Self::try_build_internal_client(&config, &session)
            .expect("CRITICAL: Failed to initialize network client");
        Self {
            client: Arc::new(RwLock::new(client)),
            config,
            session,
        }
    }

    /// 重建内部客户端 (Hot Swap)
    /// 
    /// 当代理切换或需要刷新连接池时调用。
    /// 这会创建一个新的 Client（带有新的连接池），旧的 Client 会在所有引用它的任务结束后自动释放。
    pub fn recreate_client(&self) -> Result<()> {
        let new_client = Self::try_build_internal_client(&self.config, &self.session)?;
        let mut writer = self.client.write().expect("HttpService lock poisoned");
        *writer = new_client;
        Ok(())
    }

    /// 构建底层的 HTTP 客户端
    fn try_build_internal_client(config: &AppConfig, session: &Session) -> Result<ClientWithMiddleware> {
        let proxy_url = format!("http://127.0.0.1:{}", config.singbox.proxy_port);
        let mut headers = HeaderMap::new();
        
        // 基础 Header 注入
        let base_headers = [
            (USER_AGENT, session.get_ua()),
            (COOKIE, session.get_cookie().unwrap_or_default()),
        ];

        headers.extend(
            base_headers.into_iter()
                .filter(|(_, v)| !v.is_empty())
                .filter_map(|(k, v)| {
                    HeaderValue::from_str(&v).ok().map(|val| (k, val))
                })
        );

        // 批量注入 Session Headers
        headers.extend(
            session.get_headers().iter()
                .map(|(k, v)| (k.clone(), v.clone()))
        );

        let client_builder = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy_url).map_err(SpiderError::Network)?)
            .default_headers(headers)
            .pool_max_idle_per_host(32)
            .tcp_nodelay(true)     // 禁用 Nagle 算法，降低小包延迟
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30));

        let client = client_builder.build().map_err(SpiderError::Network)?;

        Ok(ClientBuilder::new(client)
            .with(SessionMiddleware)
            .with(AntiBlockMiddleware)
            .build())
    }

    /// 获取当前可用的客户端副本
    pub fn client(&self) -> ClientWithMiddleware {
        self.client.read().expect("HttpService lock poisoned").clone()
    }

    /// 核心执行逻辑：乐观且极简
    pub async fn execute(
        &self,
        ctx: &ServiceContext,
        method: reqwest::Method,
        url: &str,
        policies: Vec<Arc<dyn NetworkPolicy>>,
    ) -> Result<reqwest::Response> {
        let client = self.client();

        let rb = ctx.request_builder_with_client(client, method, url)
            .with_extension(policies);

        rb.send().await.map_err(SpiderError::Middleware)
    }

    /// 探测方法 (用于健康检查)
    pub async fn probe(&self, url: &str, ctx: ServiceContext) -> Result<(u16, String)> {
        let resp = self.client()
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