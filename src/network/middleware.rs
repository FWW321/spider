//! 网络中间件管线 (Network Middleware Pipeline)
//!
//! 提供请求拦截、会话动态注入及基于策略的反爬检测逻辑。

use reqwest::{Request, Response, StatusCode};
use reqwest_middleware::{Middleware, Next, Result};
use std::sync::Arc;
use tracing::warn;

use crate::core::error::{BlockReason, SpiderError};
use crate::interfaces::NetworkPolicy;
use crate::interfaces::policy::PolicyResult;
use crate::network::context::ServiceContext;

/// 策略执行控制标记 (Policy Control Flag)
#[derive(Clone)]
pub enum SkipPolicy {
    /// 绕过所有验证策略 (常用于健康探测)
    All,
    /// 跳过指定的命名策略 (防止循环重定向)
    One(String),
}

/// 会话凭据注入中间件 (Credential Injection)
/// 
/// 在请求分发前，将 `Session` 中的最新指纹 (UA)、凭据 (Cookie) 及自定义头部同步至 `Request`。
pub struct SessionMiddleware;

#[async_trait::async_trait]
impl Middleware for SessionMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        if let Some(session) = extensions.get::<std::sync::Arc<crate::network::session::Session>>()
        {
            let headers = req.headers_mut();

            // 注入动态指纹
            let ua = session.get_ua();
            if !ua.is_empty()
                && let Ok(val) = reqwest::header::HeaderValue::from_str(&ua)
            {
                headers.insert(reqwest::header::USER_AGENT, val);
            }

            // 注入持久凭据
            if let Some(cookie) = session.get_cookie()
                && !cookie.is_empty()
                && let Ok(val) = reqwest::header::HeaderValue::from_str(&cookie)
            {
                headers.insert(reqwest::header::COOKIE, val);
            }

            // 同步额外上下文头部
            let extra = session.get_headers();
            for (k, v) in extra.iter() {
                headers.insert(k.clone(), v.clone());
            }
        }
        next.run(req, extensions).await
    }
}

/// 反爬拦截与自动化恢复中间件 (Anti-Block Interceptor)
/// 
/// 负责响应状态监测并执行挂载的网络策略，当检测到阻断时上报 `RefreshRequired` 以触发外部恢复流水线。
pub struct AntiBlockMiddleware;

#[async_trait::async_trait]
impl Middleware for AntiBlockMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        let ctx = extensions
            .get::<ServiceContext>()
            .expect("ServiceContext must be present in request extensions")
            .clone();

        // 执行下游管线请求
        let resp = next.run(req, extensions).await?;

        // 策略链校验 (Policy Chain Validation)
        let mut current_resp = resp;
        let policies = extensions
            .get::<Vec<Arc<dyn NetworkPolicy>>>()
            .cloned()
            .unwrap_or_default();

        let skip_policy = extensions.get::<SkipPolicy>();

        for policy in &policies {
            match skip_policy {
                Some(SkipPolicy::All) => break,
                Some(SkipPolicy::One(name)) if name == policy.name() => continue,
                _ => {}
            }

            match policy.check(current_resp, &ctx).await {
                Ok(PolicyResult::Pass(r)) => {
                    current_resp = r;
                }
                Ok(PolicyResult::Retry { is_force: _, reason }) => {
                    return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                        SpiderError::RefreshRequired {
                            reason,
                            new_url: None,
                        },
                    )));
                }
                Ok(PolicyResult::Redirect(url)) => {
                    warn!("Inbound redirect detected by policy: {}", url);
                    return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                        SpiderError::RefreshRequired {
                            reason: BlockReason::Custom("redirect".into()),
                            new_url: Some(url),
                        },
                    )));
                }
                Ok(PolicyResult::Fail(e)) => {
                    return Err(reqwest_middleware::Error::from(anyhow::Error::new(e)));
                }
                Err(e) => return Err(reqwest_middleware::Error::from(anyhow::Error::new(e))),
            }
        }

        // 基础设施层级阻断补丁 (Fallback Block Detection)
        let final_status = current_resp.status();
        if final_status == StatusCode::FORBIDDEN || final_status == StatusCode::TOO_MANY_REQUESTS {
            warn!(
                "Detection bypass alert: {} unhandled by policies, signaling refresh...",
                final_status
            );
            return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                SpiderError::RefreshRequired {
                    reason: BlockReason::from(final_status),
                    new_url: None,
                },
            )));
        }

        Ok(current_resp)
    }
}
