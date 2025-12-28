use std::sync::Arc;

use crate::core::error::Result;
use crate::core::model::{BookItem, Metadata};
use crate::sites::{Site, SiteContext, TaskArgs};

pub struct SiteClient {
    pub site: Arc<dyn Site>,
    pub ctx: SiteContext,
}

impl SiteClient {
    pub fn new(site: Arc<dyn Site>, ctx: SiteContext) -> Self {
        Self { site, ctx }
    }

    /// 发起 HTTP GET 请求
    async fn request(&self, url: &str) -> Result<String> {
        self.ctx
            .http
            .get_text(
                url,
                self.ctx.session.clone(),
                Some(Arc::new(crate::sites::MiddlewareContext {
                    site: self.site.clone(),
                    ctx: self.ctx.clone(),
                })),
            )
            .await
    }

    /// 获取元数据
    pub async fn fetch_metadata(&self, args: &TaskArgs) -> Result<(Metadata, Option<TaskArgs>)> {
        let url = self.site.build_url("metadata", args)?;
        let html = self.request(&url).await?;
        self.site.parse_metadata(&html)
    }

    /// 获取章节列表 (基于参数)
    pub async fn fetch_chapters(&self, args: &TaskArgs) -> Result<(Vec<BookItem>, Option<String>)> {
        let url = self.site.build_url("chapters", args)?;
        self.fetch_chapters_by_url(&url).await
    }

    /// 获取章节列表 (基于 URL，用于分页)
    pub async fn fetch_chapters_by_url(
        &self,
        url: &str,
    ) -> Result<(Vec<BookItem>, Option<String>)> {
        let html = self.request(url).await?;
        self.site.parse_chapters(&html)
    }

    /// 获取内容
    pub async fn fetch_content(&self, url: &str) -> Result<(String, Option<String>)> {
        let html = self.request(url).await?;
        self.site.parse_content(&html)
    }

    /// 获取完整内容 (自动处理分页)
    pub async fn fetch_full_content(&self, url: &str) -> Result<String> {
        let mut full_text = String::new();
        let mut current_url = Some(url.to_string());

        while let Some(u) = current_url {
            let (content, next) = self.fetch_content(&u).await?;
            full_text.push_str(&content);
            current_url = next;
        }

        Ok(full_text)
    }

    /// 获取二进制内容 (如图片)
    pub async fn fetch_bytes(&self, url: &str) -> Result<bytes::Bytes> {
        self.ctx
            .http
            .get_bytes(
                url,
                self.ctx.session.clone(),
                Some(Arc::new(crate::sites::MiddlewareContext {
                    site: self.site.clone(),
                    ctx: self.ctx.clone(),
                })),
            )
            .await
    }
}
