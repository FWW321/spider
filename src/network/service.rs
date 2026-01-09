use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, COOKIE, USER_AGENT};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::interfaces::NetworkPolicy;
use crate::network::context::ServiceContext;
use crate::network::middleware::{AntiBlockMiddleware, SessionMiddleware, SkipAntiBlock};
use crate::network::session::Session;

#[derive(Clone)]
pub struct HttpService {
    client: ClientWithMiddleware,
    #[allow(dead_code)]
    config: Arc<AppConfig>,
    #[allow(dead_code)]
    session: Arc<Session>,
}

impl HttpService {
    pub fn new(config: Arc<AppConfig>, session: Arc<Session>) -> Self {
        let client = Self::build_internal_client(&config, &session);
        Self {
            client,
            config,
            session,
        }
    }

    /// 构建底层的 HTTP 客户端
    fn build_internal_client(config: &AppConfig, session: &Session) -> ClientWithMiddleware {
        let proxy_url = format!("http://127.0.0.1:{}", config.singbox.proxy_port);
        let mut headers = HeaderMap::new();
        
        // 基础 Header 注入
        // 注意：SessionMiddleware 会动态覆盖这些值，这里保留作为兜底
        let ua = session.get_ua();
        if let Ok(val) = HeaderValue::from_str(&ua) {
            headers.insert(USER_AGENT, val);
        }

        if let Some(cookies) = session.get_cookie()
            && !cookies.is_empty()
                && let Ok(val) = HeaderValue::from_str(&cookies) {
                    headers.insert(COOKIE, val);
                }

        let session_headers = session.get_headers();
        for (k, v) in session_headers.iter() {
            headers.insert(k.clone(), v.clone());
        }

        let client_builder = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy_url).expect("Invalid proxy URL"))
            .default_headers(headers)
            .pool_max_idle_per_host(0) // 禁用连接池，强制每次请求建立新连接，确保代理切换生效
            .tcp_nodelay(true)     // 禁用 Nagle 算法，降低小包延迟
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30));

        ClientBuilder::new(client_builder.build().unwrap())
            .with(SessionMiddleware)
            .with(AntiBlockMiddleware)
            .build()
    }

    /// 获取当前可用的客户端副本
    pub fn client(&self) -> ClientWithMiddleware {
        self.client.clone()
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
            .with_extension(SkipAntiBlock)
            .send()
            .await
            .map_err(SpiderError::Middleware)?;

        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(SpiderError::Network)?;
        Ok((status, text))
    }
}