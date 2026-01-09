//! Booktoki 内容抓取器 (Content Fetcher)
//!
//! 负责章节正文的提取、垃圾信息过滤及多页链接探测。

use async_trait::async_trait;
use scraper::{Html, Selector};
use url::Url;

use super::SiteSelectors;
use crate::core::error::{Result, SpiderError};
use crate::network::client::SiteClient;

/// 站点特定正文获取器
pub struct BooktokiFetcher {
    /// 站点基准 URL
    base: Url,
}

impl BooktokiFetcher {
    pub fn new(base: Url) -> Self {
        Self { base }
    }
}

impl BooktokiFetcher {
    /// 执行内容抓取与语义化清理
    pub async fn fetch_content(
        &self,
        url: &str,
        client: &SiteClient,
    ) -> Result<(String, Option<String>)> {
        let html = client.get_text(url).await?;
        let doc = Html::parse_document(&html);
        let s = SiteSelectors::get();

        // Container Positioning
        let node = doc
            .select(&s.novel_content)
            .next()
            .ok_or_else(|| SpiderError::Parse("Novel content container not found".into()))?;

        // Spam Filtering
        let content = node
            .select(&s.paragraph)
            .filter_map(|p| {
                let mut byte_len = 0;
                let mut all_alphanum = true;
                let mut has_content = false;

                // Zero-allocation Scanning
                'check: for slice in p.text() {
                    for c in slice.chars() {
                        if !c.is_whitespace() {
                            has_content = true;
                            byte_len += c.len_utf8();
                            if !c.is_alphanumeric() {
                                all_alphanum = false;
                                break 'check; 
                            }
                        }
                    }
                }

                // Spam 特征：长度超过阈值且全部由字母/数字组成
                let is_spam = has_content && byte_len > 40 && all_alphanum;

                if is_spam {
                    None
                } else {
                    Some(p.html().trim().to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // 探测续页或子页面链接
        let next_url = doc
            .select(&s.btn_next)
            .next()
            .and_then(|el| el.value().attr("href"))
            .map(|href| crate::utils::to_absolute_url(&self.base, href));

        Ok((content, next_url))
    }
}
