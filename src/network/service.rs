use std::sync::Arc;

use flume::Sender;
use reqwest::{Response, header};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use tracing::debug;

use crate::actors::proxy::ProxyMsg;
use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::network::middleware::{AntiBlockMiddleware, ProxyMiddleware, SessionMiddleware};
use crate::network::session::Session;
use crate::sites::MiddlewareContext;

#[derive(Clone)]
pub struct HttpService {
    pub client: ClientWithMiddleware,
}

impl HttpService {
    pub fn new(config: Arc<AppConfig>, proxy_actor: Sender<ProxyMsg>) -> Self {
        let retry_policy =
            ExponentialBackoff::builder().build_with_max_retries(config.spider.retry_count);
        let proxy_url = format!("http://127.0.0.1:{}", config.singbox.proxy_port);

        let client_builder = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy_url).expect("Invalid proxy URL"))
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none());

        let client = ClientBuilder::new(
            client_builder
                .build()
                .expect("Failed to build reqwest client"),
        )
        .with(AntiBlockMiddleware)
        .with(SessionMiddleware)
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .with(ProxyMiddleware { proxy_actor })
        .build();

        Self { client }
    }

    pub async fn get(
        &self,
        url: &str,
        session: Arc<Session>,
        mw_ctx: Option<Arc<MiddlewareContext>>,
    ) -> Result<Response> {
        debug!("GET {}", url);
        let mut req = self.client.get(url).with_extension(session);
        if let Some(ctx) = mw_ctx {
            req = req.with_extension(ctx);
        }
        req.send().await.map_err(SpiderError::Middleware)
    }

    pub async fn post(
        &self,
        url: &str,
        json: &serde_json::Value,
        session: Arc<Session>,
        mw_ctx: Option<Arc<MiddlewareContext>>,
    ) -> Result<Response> {
        let body = serde_json::to_vec(json).map_err(|e| SpiderError::Parse(e.to_string()))?;
        let mut req = self
            .client
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .with_extension(session);

        if let Some(ctx) = mw_ctx {
            req = req.with_extension(ctx);
        }

        req.send().await.map_err(SpiderError::Middleware)
    }

    pub async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
        session: Arc<Session>,
        mw_ctx: Option<Arc<MiddlewareContext>>,
    ) -> Result<Response> {
        let mut req = self.client.post(url).form(form).with_extension(session);
        if let Some(ctx) = mw_ctx {
            req = req.with_extension(ctx);
        }
        req.send().await.map_err(SpiderError::Middleware)
    }

    pub async fn get_bytes(
        &self,
        url: &str,
        session: Arc<Session>,
        mw_ctx: Option<Arc<MiddlewareContext>>,
    ) -> Result<bytes::Bytes> {
        let resp = self.get(url, session, mw_ctx).await?;
        resp.bytes().await.map_err(SpiderError::Network)
    }

    pub async fn get_text(
        &self,
        url: &str,
        session: Arc<Session>,
        mw_ctx: Option<Arc<MiddlewareContext>>,
    ) -> Result<String> {
        let resp = self.get(url, session, mw_ctx).await?;
        resp.text().await.map_err(SpiderError::Network)
    }

    /// 探测请求，返回状态码和响应体 (跳过反阻断检测)
    pub async fn probe(
        &self,
        url: &str,
        session: Arc<Session>,
        mw_ctx: Option<Arc<MiddlewareContext>>,
    ) -> Result<(u16, String)> {
        let mut req = self
            .client
            .get(url)
            .with_extension(session)
            .with_extension(crate::network::middleware::SkipAntiBlock);

        if let Some(ctx) = mw_ctx {
            req = req.with_extension(ctx);
        }

        let resp = req.send().await.map_err(SpiderError::Middleware)?;

        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(SpiderError::Network)?;
        Ok((status, text))
    }
}
