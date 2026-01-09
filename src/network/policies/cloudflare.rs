use async_trait::async_trait;
use reqwest::Response;
use tracing::info;

use crate::core::error::{Result, SpiderError};
use crate::interfaces::policy::{NetworkPolicy, PolicyResult};
use crate::network::context::ServiceContext;
use crate::network::{ResponseExt, ResponseMetadata};

/// Cloudflare 检查策略
#[derive(Debug, Default)]
pub struct CloudflarePolicy;

impl CloudflarePolicy {
    pub fn new() -> Self {
        Self
    }

    fn contains_cf_fingerprint(&self, html: &str) -> bool {
        html.contains("<title>Just a moment...</title>")
    }
}

#[async_trait]
impl NetworkPolicy for CloudflarePolicy {
    fn name(&self) -> &str {
        "cloudflare"
    }

    async fn check(&self, resp: Response, ctx: &ServiceContext) -> Result<PolicyResult> {
        let url_str = resp.original_url().to_string();

        // 检查 HTML 内容是否包含 CF 特征
        let metadata = ResponseMetadata::from_response(&resp);
        let bytes = resp.bytes().await.map_err(SpiderError::Network)?;
        let html = String::from_utf8_lossy(&bytes);

        if self.contains_cf_fingerprint(&html) {
            info!("检测到 Cloudflare 挑战，正在尝试绕过...");
            ctx.bypass_cloudflare(&url_str).await?;
            return Ok(PolicyResult::Retry {
                is_force: true,
                reason: "cloudflare_bypassed".into()
            });
        }

        // 重建 Response 继续
        Ok(PolicyResult::Pass(metadata.rebuild(bytes)))
    }
}