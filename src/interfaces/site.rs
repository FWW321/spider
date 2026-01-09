//! 站点接口定义
//!
//! 定义了所有站点必须实现的统一接口

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use crate::core::config::SiteConfig;
use crate::core::error::Result;
use crate::core::model::{BookItem, Metadata};
use crate::network::client::SiteClient;
use crate::network::context::ServiceContext;

/// 任务参数
pub type TaskArgs = HashMap<String, String>;

/// 任务上下文 (Task Context)
#[derive(Clone)]
pub struct Context {
    /// 任务唯一ID (通常是书籍ID，或者搜索关键词)
    pub task_id: String,
    /// 任务参数
    pub args: TaskArgs,
    /// 全局服务上下文 (提供事件、重试机制等)
    pub core: ServiceContext,
}

impl Context {
    pub fn new(task_id: String, args: TaskArgs, core: ServiceContext) -> Self {
        Self {
            task_id,
            args,
            core,
        }
    }
}

/// 站点定义 Trait
#[async_trait]
pub trait Site: Send + Sync {
    /// 站点唯一标识
    fn id(&self) -> &'static str;

    /// 站点配置
    fn config(&self) -> &SiteConfig;

    /// 基础 URL
    fn base_url(&self) -> &str;

    /// 获取站点专用客户端 (包含特定的策略链)
    fn client(&self) -> &SiteClient;

    /// 获取元数据
    ///
    /// 返回元数据和可选的新参数(用于参数发现，如 slug -> id)
    async fn fetch_metadata(&self, ctx: &Context) -> Result<(Metadata, Option<TaskArgs>)>;

    /// 获取章节列表
    ///
    /// 必须返回完整的章节列表 (如果站点分页，需在此方法内处理完分页逻辑)
    async fn fetch_chapter_list(&self, ctx: &Context) -> Result<Vec<BookItem>>;

    /// 获取章节内容
    ///
    /// 返回完整的章节正文 (如果章节内部分页，需在此方法内处理合并)
    async fn fetch_content(&self, ctx: &Context, chapter_item: &BookItem) -> Result<String>;

    /// 站点预热 (可选)
    /// 用于登录、获取初始 Cookie 或 CSRF Token
    async fn prepare(&self, _ctx: &Context) -> Result<()> {
        Ok(())
    }

    /// 图片处理与提取 (可选，提供默认实现)
    ///
    /// 输入 HTML，输出 (处理后的HTML, 提取的图片URL列表)
    /// 默认实现会将 img src 替换为相对路径 "../Images/xxx"
    fn process_images(&self, html: &str) -> (String, Vec<String>) {
        use crate::utils::{generate_filename, to_absolute_url};
        use scraper::{Html, Selector};

        let document = Html::parse_document(html);
        let selector = match Selector::parse("img") {
            Ok(s) => s,
            Err(_) => return (html.to_string(), vec![]),
        };

        let mut images = Vec::new();
        let mut new_html = html.to_string();
        let base = Url::parse(self.base_url()).ok();

        for element in document.select(&selector) {
            let Some(src) = element.value().attr("src") else {
                continue;
            };
            if src.trim().is_empty() {
                continue;
            }

            // 1. 检查是否已处理 (避免重复处理)
            if let Some(original_url) = element.value().attr("data-original-url") {
                images.push(original_url.to_string());
                continue;
            }

            // 2. 转绝对路径
            let absolute_url = base
                .as_ref()
                .map(|b| to_absolute_url(b, src))
                .unwrap_or_else(|| src.to_string());

            images.push(absolute_url.clone());

            // 3. 生成本地路径
            let filename = generate_filename(&absolute_url);
            let local_path = format!("../Images/{}", filename);
            let old_tag = element.html();

            // 4. 替换标签 (保留原始URL)
            let new_tag = old_tag
                .replacen(
                    &format!("src=\"{}\"", src),
                    &format!(
                        "src=\"{}\" data-original-url=\"{}\"",
                        local_path,
                        absolute_url
                    ),
                    1,
                )
                .replacen(
                    &format!("src='{}'", src),
                    &format!("src='{}' data-original-url='{}'", local_path, absolute_url),
                    1,
                );

            new_html = new_html.replace(&old_tag, &new_tag);
        }

        (new_html, images)
    }
}