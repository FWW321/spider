//! Booktoki 内容获取器
//!
//! 负责获取和解析章节的具体正文内容

use async_trait::async_trait;
use scraper::{Html, Selector};
use url::Url;

use crate::core::error::{Result, SpiderError};
use crate::network::client::SiteClient;
use super::SiteSelectors;

/// Booktoki 内容获取器
pub struct BooktokiFetcher {
    base: Url,
}

impl BooktokiFetcher {
    pub fn new(base: Url) -> Self {
        Self { base }
    }
}

impl BooktokiFetcher {
    pub async fn fetch_content(
        &self,
        url: &str,
        client: &SiteClient,
    ) -> Result<(String, Option<String>)> {
        let html = client.get_text(url).await?;
        let doc = Html::parse_document(&html);
        let s = SiteSelectors::get();

        // 1. 获取正文容器
        let node = doc.select(&s.novel_content).next().ok_or_else(|| {
            SpiderError::Parse("Novel content container not found".into())
        })?;

        // 2. 遍历 <p> 标签并过滤 Spam
        // 如果没有 <p> 标签，说明该章内容为空
        let content = node
            .select(&s.paragraph)
            .filter_map(|p| {
                let text = p.text().collect::<String>();
                let trimmed = text.trim();

                // Spam 过滤器：过滤乱码干扰行
                let is_spam = trimmed.len() > 40
                    && !trimmed.is_empty()
                    && trimmed.chars().all(|c| c.is_alphanumeric());

                if is_spam {
                    None
                } else {
                    Some(p.html().trim().to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // 3. 解析下一页链接
        let next_selector = Selector::parse("a.btn-next")
            .map_err(|_| SpiderError::Parse("Invalid next selector".into()))?;
        let next_url = doc
            .select(&next_selector)
            .next()
            .and_then(|el| el.value().attr("href"))
            .map(|href| crate::utils::to_absolute_url(&self.base, href));

        Ok((content, next_url))
    }
}
