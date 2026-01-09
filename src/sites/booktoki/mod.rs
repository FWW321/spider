//! Booktoki 站点模块
//!
//! 按照新架构拆分为多个子模块

mod fetcher;
mod indexer;
mod policy;
mod selectors;

use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use crate::core::config::SiteConfig;
use crate::core::error::Result;
use crate::interfaces::site::TaskArgs;
use crate::interfaces::{ContentFetcher, Indexer, NetworkPolicy, Site, SiteClient};
use crate::network::context::ServiceContext;
use crate::network::policies::{CloudflarePolicy, RedirectPolicy};

pub use self::fetcher::BooktokiFetcher;
pub use self::indexer::BooktokiIndexer;
pub use self::policy::CaptchaPolicy;
pub use self::selectors::SiteSelectors;

/// Booktoki 站点实现
pub struct Booktoki {
    config: SiteConfig,
    base: Url,
    indexer: BooktokiIndexer,
    fetcher: BooktokiFetcher,
    client: SiteClient,
}

impl Booktoki {
    /// 创建新的 Booktoki 站点实例
    pub fn new(config: SiteConfig, ctx: ServiceContext) -> Self {
        let base_url = config
            .base_url
            .as_deref()
            .unwrap_or("https://booktoki469.com");
        let base = Url::parse(base_url).expect("Invalid base URL");

        // 初始化策略链
        let policies: Vec<Arc<dyn NetworkPolicy>> = vec![
            Arc::new(CaptchaPolicy::new(base.clone())),
            Arc::new(CloudflarePolicy::new()),
            Arc::new(RedirectPolicy::new()),
        ];

        let client = SiteClient::new(ctx, policies);

        Self {
            indexer: BooktokiIndexer::new(base.clone()),
            fetcher: BooktokiFetcher::new(base.clone()),
            client,
            base,
            config,
        }
    }

    /// 规范化 URL
    #[inline]
    pub fn normalize(&self, path: &str) -> String {
        crate::utils::to_absolute_url(&self.base, path)
    }
}

#[async_trait]
impl Site for Booktoki {
    fn id(&self) -> &str {
        "booktoki"
    }

    fn config(&self) -> &SiteConfig {
        &self.config
    }

    fn base_url(&self) -> &str {
        self.base.as_str()
    }

    fn client(&self) -> &SiteClient {
        &self.client
    }

    fn indexer(&self) -> &dyn Indexer {
        &self.indexer
    }

    fn fetcher(&self) -> &dyn ContentFetcher {
        &self.fetcher
    }

    fn build_url(&self, kind: &str, args: &TaskArgs) -> Result<String> {
        use crate::core::error::SpiderError;

        match kind {
            "metadata" | "chapters" => {
                let id = args
                    .get("id")
                    .ok_or_else(|| SpiderError::Parse("Missing id".into()))?;
                Ok(self.normalize(&format!("/novel/{}", id)))
            }
            _ => Err(SpiderError::Parse(format!("Unknown kind: {}", kind))),
        }
    }

    async fn prepare(&self, _ctx: &ServiceContext) -> Result<()> {
        tracing::info!("Booktoki 正在预热...");

        // 使用站点客户端执行预热，自动处理反爬
        // match self.client.get_text(self.base_url()).await {
        //     Ok(_) => tracing::info!("Booktoki 预热成功"),
        //     Err(e) => tracing::warn!("Booktoki 预热失败: {}", e),
        // }

        Ok(())
    }
}