use std::sync::Arc;
use reqwest::{Request, Response, StatusCode};
use reqwest_middleware::{Middleware, Next, Result};
use tracing::warn;

use crate::interfaces::policy::PolicyResult;
use crate::interfaces::NetworkPolicy;
use crate::network::context::ServiceContext;
use crate::core::error::SpiderError;

/// 跳过反爬逻辑标记
#[derive(Clone)]
pub struct SkipAntiBlock;

/// 会话注入中间件
/// 负责在每次请求前，动态将 Session 中的最新 Cookie/UA 注入 Header
pub struct SessionMiddleware;

#[async_trait::async_trait]
impl Middleware for SessionMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        if let Some(session) = extensions.get::<std::sync::Arc<crate::network::session::Session>>() {
            let headers = req.headers_mut();

            // 动态注入 UA
            let ua = session.get_ua();
            if !ua.is_empty()
                && let Ok(val) = reqwest::header::HeaderValue::from_str(&ua) {
                    headers.insert(reqwest::header::USER_AGENT, val);
                }

            // 动态注入 Cookie
            if let Some(cookie) = session.get_cookie()
                && !cookie.is_empty()
                    && let Ok(val) = reqwest::header::HeaderValue::from_str(&cookie) {
                        headers.insert(reqwest::header::COOKIE, val);
                    }

            // 动态注入其他 Headers
            let extra = session.get_headers();
            for (k, v) in extra.iter() {
                headers.insert(k.clone(), v.clone());
            }
        }
        next.run(req, extensions).await
    }
}

/// 反爬中间件
/// 负责：1. 乐观锁等待  2. 运行策略检查并按需抛出 RefreshRequired
pub struct AntiBlockMiddleware;

#[async_trait::async_trait]
impl Middleware for AntiBlockMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        if extensions.get::<SkipAntiBlock>().is_some() {
            return next.run(req, extensions).await;
        }

        let ctx = extensions
            .get::<ServiceContext>()
            .expect("ServiceContext missing")
            .clone();

        let policies = extensions
            .get::<Vec<Arc<dyn NetworkPolicy>>>()
            .cloned()
            .unwrap_or_default();

        let resp = next.run(req, extensions).await?;

        // 先运行站点策略链（Cloudflare、Captcha 等需要精确检测）
        let mut current_resp = resp;
        for policy in &policies {
            match policy.check(current_resp, &ctx).await {
                Ok(PolicyResult::Pass(r)) => {
                    current_resp = r;
                }
                Ok(PolicyResult::Retry { is_force, reason }) => {
                    if is_force {
                        // 强制重试：抛出中断信号，让顶层 HttpService 换个 Client 重新来过
                        return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                            SpiderError::RefreshRequired {
                                reason,
                                new_url: None,
                            }
                        )));
                    } else {
                        // 普通重试：暂时也可以抛出，让顶层统一处理
                        return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                            SpiderError::RefreshRequired {
                                reason: format!("Policy retry: {}", reason),
                                new_url: None,
                            }
                        )));
                    }
                }
                Ok(PolicyResult::Redirect(url)) => {
                    warn!("检测到重定向: {}", url);
                    return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                        SpiderError::RefreshRequired {
                            reason: "redirect".into(),
                            new_url: Some(url),
                        }
                    )));
                }
                Ok(PolicyResult::Fail(e)) => return Err(reqwest_middleware::Error::from(anyhow::Error::new(e))),
                Err(e) => return Err(reqwest_middleware::Error::from(anyhow::Error::new(e))),
            }
        }

        // 策略链都通过了，再检查是否是通用的 403/429（真正的 IP 封禁）
        let final_status = current_resp.status();
        if final_status == StatusCode::FORBIDDEN || final_status == StatusCode::TOO_MANY_REQUESTS {
            warn!("检测到 {}（策略未处理），正在切换代理并重置会话...", final_status);
            ctx.rotate_proxy().await;
            ctx.reset_browser().await;
            return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                SpiderError::RefreshRequired {
                    reason: format!("HTTP {}", final_status),
                    new_url: None,
                }
            )));
        }

        Ok(current_resp)
    }
}