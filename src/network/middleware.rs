use std::sync::Arc;
use reqwest::{Request, Response, StatusCode};
use reqwest_middleware::{Middleware, Next, Result};
use tracing::warn;

use crate::interfaces::policy::PolicyResult;
use crate::interfaces::NetworkPolicy;
use crate::network::context::ServiceContext;
use crate::core::error::SpiderError;

/// 策略跳过标记
///
/// 用于控制请求是否应该绕过某些或所有网络策略
#[derive(Clone)]
pub enum SkipPolicy {
    /// 跳过所有策略 (通常用于探测、健康检查)
    All,
    /// 跳过特定名称的策略 (防止递归调用)
    One(String),
}

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
        let ctx = extensions
            .get::<ServiceContext>()
            .expect("ServiceContext missing")
            .clone();

        // 1. 执行请求
        let resp = next.run(req, extensions).await?;

        // 2. 策略检查
        let mut current_resp = resp;
        let policies = extensions
            .get::<Vec<Arc<dyn NetworkPolicy>>>()
            .cloned()
            .unwrap_or_default();

        let skip_policy = extensions.get::<SkipPolicy>();

        for policy in &policies {
            // 检查是否需要跳过当前策略
            match skip_policy {
                Some(SkipPolicy::All) => break,
                Some(SkipPolicy::One(name)) if name == policy.name() => continue,
                _ => {}
            }

            match policy.check(current_resp, &ctx).await {
                Ok(PolicyResult::Pass(r)) => {
                    current_resp = r;
                }
                Ok(PolicyResult::Retry { is_force, reason }) => {
                    if is_force {
                        return Err(reqwest_middleware::Error::from(anyhow::Error::new(
                            SpiderError::RefreshRequired {
                                reason,
                                new_url: None,
                            }
                        )));
                    } else {
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

        // 3. 基础设施层封禁检查
        // 除非策略链已经拦截处理了，否则这里作为最后的防线处理 IP 封禁
        let final_status = current_resp.status();
        if final_status == StatusCode::FORBIDDEN || final_status == StatusCode::TOO_MANY_REQUESTS {
            warn!("检测到 {}（策略未处理），上报 RefreshRequired...", final_status);
            // 纯粹的监测哨：发现封禁只管报错，具体的恢复（切IP、重试）交给上层引擎调度
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