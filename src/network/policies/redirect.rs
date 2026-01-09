use async_trait::async_trait;
use reqwest::Response;
use tracing::debug;

use crate::core::error::Result;
use crate::interfaces::policy::{NetworkPolicy, PolicyResult};
use crate::network::context::ServiceContext;
use crate::network::ResponseExt;

/// 重定向处理策略
#[derive(Debug, Default)]
pub struct RedirectPolicy;

impl RedirectPolicy {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NetworkPolicy for RedirectPolicy {
    fn name(&self) -> &str {
        "redirect"
    }

    async fn check(&self, resp: Response, _ctx: &ServiceContext) -> Result<PolicyResult> {
        let status = resp.status();
        
        if status.is_redirection()
            && let Some(location) = resp.headers().get(reqwest::header::LOCATION) {
                let location_str = String::from_utf8_lossy(location.as_bytes()).to_string();
                let original_url = resp.original_url();
                
                let base = url::Url::parse(original_url).map_err(|e| {
                    crate::core::error::SpiderError::Custom(format!("Invalid URL: {}", e))
                })?;
                
                let target_url = base.join(&location_str).map_err(|e| {
                    crate::core::error::SpiderError::Custom(format!("Invalid redirect: {}", e))
                })?.to_string();

                debug!("重定向: {} -> {}", original_url, target_url);
                return Ok(PolicyResult::Redirect(target_url));
            }

        Ok(PolicyResult::Pass(resp))
    }
}
