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
        // 优化：避免在过滤阶段进行 String 分配
        let content = node
            .select(&s.paragraph)
            .filter_map(|p| {
                let mut byte_len = 0;
                let mut all_alphanum = true;
                let mut has_content = false;

                // Zero-allocation spam check
                'check: for slice in p.text() {
                    for c in slice.chars() {
                        if !c.is_whitespace() {
                            has_content = true;
                            byte_len += c.len_utf8();
                            if !c.is_alphanumeric() {
                                all_alphanum = false;
                                break 'check; // 发现非字母数字字符，肯定不是 Spam
                            }
                        }
                    }
                }

                // Spam 定义：长度 > 40 且全是字母/数字
                let is_spam = has_content && byte_len > 40 && all_alphanum;

                if is_spam {
                    None
                } else {
                    Some(p.html().trim().to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // 3. 解析下一页链接
        let next_url = doc
            .select(&s.btn_next)
            .next()
            .and_then(|el| el.value().attr("href"))
            .map(|href| crate::utils::to_absolute_url(&self.base, href));

        Ok((content, next_url))
    }
}
