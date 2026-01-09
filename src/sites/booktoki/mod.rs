//! Booktoki 站点模块

mod fetcher;
mod indexer;
mod policy;
mod selectors;

use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use crate::core::config::SiteConfig;
use crate::core::error::Result;
use crate::core::model::{BookItem, Metadata};
use crate::interfaces::site::{Context, TaskArgs};
use crate::interfaces::{NetworkPolicy, Site, SiteClient};
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
    fn id(&self) -> &'static str {
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

    async fn fetch_metadata(&self, ctx: &Context) -> Result<(Metadata, Option<TaskArgs>)> {
        self.indexer.fetch_metadata(&ctx.args, self.client()).await
    }

    async fn fetch_chapter_list(&self, ctx: &Context) -> Result<Vec<BookItem>> {
        let (mut items, mut next) = self.indexer.fetch_chapters(&ctx.args, self.client()).await?;

        // 处理分页
        while let Some(url) = next {
            tracing::debug!("Fetching next chapter page: {}", url);
            let (more_items, next_url) = self
                .indexer
                .fetch_chapters_by_url(&url, self.client())
                .await?;
            items.extend(more_items);
            next = next_url;
        }

        Ok(items)
    }

    async fn fetch_content(&self, _ctx: &Context, item: &BookItem) -> Result<String> {
        let start_url = match item {
            BookItem::Chapter(c) => &c.url,
            _ => return Ok(String::new()),
        };

        let mut full_text = String::new();
        let mut current_url = Some(start_url.to_string());

        // 处理内容分页
        while let Some(u) = current_url {
            let (content, next) = self.fetcher.fetch_content(&u, self.client()).await?;
            full_text.push_str(&content);
            current_url = next;
        }

        Ok(full_text)
    }

    async fn prepare(&self, _ctx: &Context) -> Result<()> {
        tracing::info!("Booktoki 正在预热...");
        Ok(())
    }
}
