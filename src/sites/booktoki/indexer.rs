//! Booktoki 索引器
//!
//! 负责获取书籍元数据和章节列表

use async_trait::async_trait;
use scraper::{ElementRef, Html};
use url::Url;

use crate::core::error::{Result, SpiderError};
use crate::core::model::{BookItem, Chapter, Metadata};
use crate::interfaces::site::TaskArgs;
use crate::network::client::SiteClient;
use crate::utils::to_absolute_url;

use super::SiteSelectors;

/// Booktoki 索引器
pub struct BooktokiIndexer {
    base: Url,
}

impl BooktokiIndexer {
    /// 创建新的索引器
    pub fn new(base: Url) -> Self {
        Self { base }
    }

    /// 规范化 URL
    #[inline]
    fn normalize(&self, path: &str) -> String {
        to_absolute_url(&self.base, path)
    }

    /// 提取图标后的文本
    fn extract_icon_text(
        &self,
        parent: &ElementRef,
        selector: &scraper::Selector,
    ) -> Option<String> {
        parent
            .select(selector)
            .next()
            .and_then(|el| el.next_sibling())
            .and_then(|n| n.value().as_text())
            .map(|t| t.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 解析元数据
    fn parse_metadata_html(&self, html: &str) -> Result<(Metadata, Option<TaskArgs>)> {
        let doc = Html::parse_document(html);
        let s = SiteSelectors::get();

        let detail = doc.select(&s.detail_desc).next().ok_or_else(|| {
            // 如果在重试后仍然找不到，说明是解析错误或死链
            SpiderError::Parse("Detail element not found (Structure changed or invalid ID)".into())
        })?;

        let mut contents = detail.select(&s.view_content);

        let title = contents
            .next()
            .and_then(|el| el.select(&s.bold).next())
            .map(|b| b.text().collect::<String>())
            .unwrap_or_else(|| "Unknown Title".into());

        let (mut author, mut tags, mut publisher) = (None, Vec::new(), None);
        if let Some(info) = contents.next() {
            author = self.extract_icon_text(&info, &s.icon_user);

            if let Some(tag_str) = self.extract_icon_text(&info, &s.icon_tags) {
                tags = tag_str.split(',').map(|s| s.trim().to_string()).collect();
            }

            if let Some(pub_str) = self.extract_icon_text(&info, &s.icon_building) {
                publisher = Some(format!("booktoki: {}", pub_str));
            }
        }

        let summary = contents
            .next()
            .map(|el| el.text().collect::<Vec<_>>().join("\n").trim().to_string());

        let cover_url = detail
            .select(&s.view_img)
            .next()
            .and_then(|img| img.value().attr("src"))
            .map(|url| self.normalize(url));

        Ok((
            Metadata {
                title,
                author,
                language: "ko".into(),
                summary,
                cover_url,
                tags,
                publisher,
            },
            None,
        ))
    }

    /// 解析章节列表
    fn parse_chapters_html(&self, html: &str) -> Result<(Vec<BookItem>, Option<String>)> {
        if html.is_empty() {
            return Ok((vec![], None));
        }

        let doc = Html::parse_document(html);
        let s = SiteSelectors::get();

        if doc.select(&s.wr_none).next().is_some() {
            return Ok((vec![], None));
        }

        let chapters = doc
            .select(&s.list_item)
            .filter_map(|item| {
                let link = item.select(&s.wr_subject_link).next()?;
                let url = link.value().attr("href")?.to_string();
                if url.is_empty() {
                    return None;
                }

                let id = item
                    .select(&s.wr_num)
                    .next()
                    .map(|el| el.text().collect::<String>().trim().to_string())
                    .unwrap_or_else(|| "0".to_string());

                let title = link
                    .children()
                    .filter_map(|node| node.value().as_text())
                    .map(|t| t.trim())
                    .collect::<String>()
                    .trim()
                    .to_string();

                Some(BookItem::Chapter(Chapter {
                    index: id.parse().unwrap_or(0),
                    id,
                    title,
                    url: self.normalize(&url),
                }))
            })
            .collect();

        let next_url = doc
            .select(&s.pagination_next)
            .next()
            .and_then(|a| a.value().attr("href"))
            .filter(|href| !href.is_empty() && !href.starts_with('#'))
            .map(|href| self.normalize(href));

        Ok((chapters, next_url))
    }

    /// 构建 URL
    fn build_url(&self, kind: &str, args: &TaskArgs) -> Result<String> {
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
}

impl BooktokiIndexer {
    pub async fn fetch_metadata(
        &self,
        args: &TaskArgs,
        client: &SiteClient,
    ) -> Result<(Metadata, Option<TaskArgs>)> {
        let url = self.build_url("metadata", args)?;
        // 使用 Client，自动处理重试
        let html = client.get_text(&url).await?;
        self.parse_metadata_html(&html)
    }

    pub async fn fetch_chapters(
        &self,
        args: &TaskArgs,
        client: &SiteClient,
    ) -> Result<(Vec<BookItem>, Option<String>)> {
        let url = self.build_url("chapters", args)?;
        self.fetch_chapters_by_url(&url, client).await
    }

    pub async fn fetch_chapters_by_url(
        &self,
        url: &str,
        client: &SiteClient,
    ) -> Result<(Vec<BookItem>, Option<String>)> {
        let html = client.get_text(url).await?;
        self.parse_chapters_html(&html)
    }
}
