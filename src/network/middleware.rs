use std::sync::Arc;

use flume::Sender;
use http::{Extensions, HeaderValue, Method};
use reqwest::{Request, Response, StatusCode, Url, header};
use reqwest_middleware::{Error, Middleware, Next, Result};
use tracing::{debug, info, warn};

// 假设这些是在 crate 中定义的，保持原样引用
use crate::actors::proxy::ProxyMsg;
use crate::core::error::SpiderError;
use crate::network::session::Session;
use crate::sites::MiddlewareContext;

/// 标记结构体，用于在请求扩展中指示跳过阻断检测逻辑
#[derive(Clone)]
pub struct SkipAntiBlock;

/// 保存原始请求URL，用于重定向后的重试
#[derive(Clone)]
pub struct OriginalUrl(pub String);

#[derive(Clone, Default)]
struct RedirectCount(u8);

#[derive(Clone, Default)]
struct RecoveryAttempts(u8);

const MAX_REDIRECTS: u8 = 10;
const MAX_RECOVERY_ATTEMPTS: u8 = 3;

// --- ProxyMiddleware ---

pub struct ProxyMiddleware {
    pub proxy_actor: Sender<ProxyMsg>,
}

#[async_trait::async_trait]
impl Middleware for ProxyMiddleware {
    async fn handle(&self, req: Request, ext: &mut Extensions, next: Next<'_>) -> Result<Response> {
        let res = next.run(req, ext).await;

        if let Err(e) = &res {
            if matches!(e, Error::Reqwest(e) if e.is_timeout() || e.is_connect()) {
                let _ = self.proxy_actor.send(ProxyMsg::Rotate { reply: None });
            }
        }
        res
    }
}

// --- SessionMiddleware ---

pub struct SessionMiddleware;

#[async_trait::async_trait]
impl Middleware for SessionMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        ext: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        if let Some(session) = ext.get::<Arc<Session>>() {
            let ua = session.ua.read();
            if !ua.is_empty() {
                if let Ok(val) = HeaderValue::from_str(&ua) {
                    req.headers_mut().insert(header::USER_AGENT, val);
                }
            }

            let cookie = session.cookie.read();
            if let Some(c) = &*cookie {
                debug!("注入 Session Cookie (长度: {})", c.len());
                if let Ok(val) = HeaderValue::from_str(c) {
                    req.headers_mut().insert(header::COOKIE, val);
                }
            }

            let extra = session.extra_headers.read();
            for (k, v) in extra.iter() {
                req.headers_mut().insert(k.clone(), v.clone());
            }
        }
        next.run(req, ext).await
    }
}

// --- AntiBlockMiddleware ---

pub struct AntiBlockMiddleware;

impl AntiBlockMiddleware {
    fn resolve_redirect_url(base_url: &Url, res: &Response) -> Option<Url> {
        let loc = res.headers().get(header::LOCATION)?;

        let loc_str = String::from_utf8_lossy(loc.as_bytes());

        base_url.join(&loc_str).ok()
    }

    fn should_check_block(status: StatusCode, res: &Response) -> bool {
        if status.as_u16() >= 400 && status != StatusCode::NOT_FOUND {
            return true;
        }
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map_or(false, |s| s.contains("text/html"))
    }
}

#[async_trait::async_trait]
impl Middleware for AntiBlockMiddleware {
    async fn handle(&self, req: Request, ext: &mut Extensions, next: Next<'_>) -> Result<Response> {
        let skip_recover = ext.get::<SkipAntiBlock>().is_some();
        if ext.get::<OriginalUrl>().is_none() {
            ext.insert(OriginalUrl(req.url().to_string()));
        }

        let current_referer = req.url().to_string();
        let res = next.clone().run(req, ext).await?;
        let status = res.status();
        let current_url = res.url().clone();

        debug!("响应状态: {} ({})", status.as_u16(), current_url);

        // 仅在明确是重定向状态码时尝试跟随 Location
        if status.is_redirection() {
            if let Some(target_url) = Self::resolve_redirect_url(&current_url, &res) {
                let redirect_count = ext.get::<RedirectCount>().map(|r| r.0).unwrap_or(0);

                if redirect_count >= MAX_REDIRECTS {
                    warn!("达到最大重定向次数 ({})", MAX_REDIRECTS);
                    return Ok(res);
                }

                debug!("跟随重定向: {} -> {}", status, target_url);
                ext.insert(RedirectCount(redirect_count + 1));

                let mut new_req = Request::new(Method::GET, target_url);
                new_req.headers_mut().insert(
                    header::REFERER,
                    HeaderValue::from_str(&current_referer).unwrap(),
                );

                return Box::pin(self.handle(new_req, ext, next)).await;
            }
        }

        if skip_recover || !Self::should_check_block(status, &res) {
            return Ok(res);
        }

        let Some(mw_ctx) = ext.get::<Arc<MiddlewareContext>>() else {
            return Ok(res);
        };

        let headers = res.headers().clone();
        let version = res.version();

        let bytes = res.bytes().await.map_err(reqwest_middleware::Error::from)?;
        let html = String::from_utf8_lossy(&bytes);

        if let Err(SpiderError::SoftBlock(reason)) =
            mw_ctx
                .site
                .check_block(current_url.as_str(), &html, status.as_u16())
        {
            let original_url_str = ext
                .get::<OriginalUrl>()
                .map(|u| u.0.clone())
                .unwrap_or_else(|| current_url.to_string());
            let attempts = ext.get::<RecoveryAttempts>().map(|r| r.0).unwrap_or(0);

            if attempts >= MAX_RECOVERY_ATTEMPTS {
                warn!(
                    "达到最大阻断恢复重试次数 ({}): {}",
                    MAX_RECOVERY_ATTEMPTS, original_url_str
                );
                return Err(reqwest_middleware::Error::from(anyhow::anyhow!(
                    SpiderError::SoftBlock(format!("{}:max_retries_exceeded", reason))
                )));
            }

            warn!(
                "检测到访问阻断，正在重试 ({}/{})",
                attempts + 1,
                MAX_RECOVERY_ATTEMPTS
            );
            debug!("阻断详情: {} -> {}", original_url_str, reason);

            let recovered = mw_ctx
                .site
                .recover(&original_url_str, &reason, &mw_ctx.ctx)
                .await
                .map_err(|e| reqwest_middleware::Error::from(anyhow::anyhow!(e)))?;

            if recovered {
                info!("访问阻断已自动解除，继续执行任务...");
                debug!("重试目标: {}", original_url_str);

                ext.insert(RedirectCount(0));
                ext.insert(RecoveryAttempts(attempts + 1));

                let new_req = Request::new(Method::GET, original_url_str.parse().unwrap());
                return Box::pin(self.handle(new_req, ext, next)).await;
            }
        }

        let mut builder = http::Response::builder().status(status).version(version);

        if let Some(h_map) = builder.headers_mut() {
            *h_map = headers;
        }

        let response = builder.body(bytes).unwrap();

        Ok(Response::from(response))
    }
}
