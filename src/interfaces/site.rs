//! 站点定义
//!
//! 定义了站点需要实现的核心接口，包括索引器、内容获取器等。

use std::collections::HashMap;

use async_trait::async_trait;

use crate::core::config::SiteConfig;
use crate::core::error::Result;
use crate::core::model::{BookItem, Metadata};
use crate::network::client::SiteClient;
use crate::network::context::ServiceContext;

/// 任务参数
pub type TaskArgs = HashMap<String, String>;

/// 索引器 Trait - 负责获取书籍元数据和章节列表
#[async_trait]
pub trait Indexer: Send + Sync {
    /// 获取书籍元数据
    async fn fetch_metadata(
        &self,
        args: &TaskArgs,
        client: &SiteClient,
    ) -> Result<(Metadata, Option<TaskArgs>)>;

    /// 获取章节列表
    async fn fetch_chapters(
        &self,
        args: &TaskArgs,
        client: &SiteClient,
    ) -> Result<(Vec<BookItem>, Option<String>)>;

    /// 根据 URL 获取章节列表（用于分页）
    async fn fetch_chapters_by_url(
        &self,
        url: &str,
        client: &SiteClient,
    ) -> Result<(Vec<BookItem>, Option<String>)>;
}

/// 内容获取器 Trait - 负责获取章节内容
#[async_trait]
pub trait ContentFetcher: Send + Sync {
    /// 获取章节内容
    async fn fetch_content(
        &self,
        url: &str,
        client: &SiteClient,
    ) -> Result<(String, Option<String>)>;

    /// 获取完整内容（自动处理分页）
    async fn fetch_full_content(&self, url: &str, client: &SiteClient) -> Result<String> {
        let mut full_text = String::new();
        let mut current_url = Some(url.to_string());

        while let Some(u) = current_url {
            let (content, next) = self.fetch_content(&u, client).await?;
            full_text.push_str(&content);
            current_url = next;
        }

        Ok(full_text)
    }
}

/// 站点定义 Trait
///
/// 每个站点需要实现此 Trait，提供：
/// - 站点标识和配置
/// - 客户端（封装了策略链）
/// - 索引器和内容获取器
#[async_trait]
pub trait Site: Send + Sync {
    /// 站点唯一标识
    fn id(&self) -> &str;

    /// 站点配置
    fn config(&self) -> &SiteConfig;

    /// 基础 URL
    fn base_url(&self) -> &str;

    /// 获取站点专用客户端
    fn client(&self) -> &SiteClient;

    /// 获取索引器
    fn indexer(&self) -> &dyn Indexer;

    /// 获取内容获取器
    fn fetcher(&self) -> &dyn ContentFetcher;

    /// 预准备钩子（可选）
    /// 在 Pipeline 初始化之前调用，用于预登录、获取初始 Cookie 等
    async fn prepare(&self, _ctx: &ServiceContext) -> Result<()> {
        Ok(())
    }

    /// 构建 URL
    fn build_url(&self, kind: &str, args: &TaskArgs) -> Result<String>;
}
