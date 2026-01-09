//! Booktoki 资源索引器 (Resource Indexer)
//!
//! 负责站点元数据的抽取与章节层级结构的递归发现。

use async_trait::async_trait;
use scraper::{ElementRef, Html};
use url::Url;

use crate::core::error::{Result, SpiderError};
use crate::core::model::{BookItem, Chapter, Metadata};
use crate::interfaces::site::TaskArgs;
use crate::network::client::SiteClient;
use crate::utils::to_absolute_url;

use super::SiteSelectors;

/// 站点特定索引器
pub struct BooktokiIndexer {
    /// 站点基准 URL (用于相对路径规范化)
    base: Url,
}

impl BooktokiIndexer {
    pub fn new(base: Url) -> Self {
        Self { base }
    }

    /// 执行相对路径至绝对 URL 的规范化 (URI Normalization)
    #[inline]
    fn normalize(&self, path: &str) -> String {
        to_absolute_url(&self.base, path)
    }

    /// 基于 CSS 选择器提取紧邻 Icon 节点的文本内容
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

    /// 执行元数据 HTML 静态分析 (Metadata Scoping)
    fn parse_metadata_html(&self, html: &str) -> Result<(Metadata, Option<TaskArgs>)> {
        let doc = Html::parse_document(html);
        let s = SiteSelectors::get();

        let detail = doc.select(&s.detail_desc).next().ok_or_else(|| {
            SpiderError::Parse("Target resource not found: Structural mismatch or invalid ID".into())
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

    /// 执行章节列表分步解析与分页探测 (Pagination Discovery)
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

        // 探测下一页 URL 节点
        let next_url = doc
            .select(&s.pagination_next)
            .next()
            .and_then(|a| a.value().attr("href"))
            .filter(|href| !href.is_empty() && !href.starts_with('#'))
            .map(|href| self.normalize(href));

        Ok((chapters, next_url))
    }

    /// 根据路由变体构建资源定位符
    fn build_url(&self, kind: &str, args: &TaskArgs) -> Result<String> {
        match kind {
            "metadata" | "chapters" => {
                let id = args
                    .get("id")
                    .ok_or_else(|| SpiderError::Parse("Missing dynamic parameter: id".into()))?;
                Ok(self.normalize(&format!("/novel/{}", id)))
            }
            _ => Err(SpiderError::Parse(format!("Unknown routing kind: {}", kind))),
        }
    }
}

impl BooktokiIndexer {
    /// 执行元数据抓取与解析任务
    pub async fn fetch_metadata(
        &self,
        args: &TaskArgs,
        client: &SiteClient,
    ) -> Result<(Metadata, Option<TaskArgs>)> {
        let url = self.build_url("metadata", args)?;
        let html = client.get_text(&url).await?;
        self.parse_metadata_html(&html)
    }

    /// 初始化章节列表获取任务
    pub async fn fetch_chapters(
        &self,
        args: &TaskArgs,
        client: &SiteClient,
    ) -> Result<(Vec<BookItem>, Option<String>)> {
        let url = self.build_url("chapters", args)?;
        self.fetch_chapters_by_url(&url, client).await
    }

    /// 基于特定 URL 执行增量章节抓取
    pub async fn fetch_chapters_by_url(
        &self,
        url: &str,
        client: &SiteClient,
    ) -> Result<(Vec<BookItem>, Option<String>)> {
        let html = client.get_text(url).await?;
        self.parse_chapters_html(&html)
    }
}
