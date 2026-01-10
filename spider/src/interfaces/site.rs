//! 站点抽象接口 (Site Abstraction)
//!
//! 定义爬虫引擎与具体站点实现之间的交互协议。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use crate::core::config::SiteConfig;
use crate::core::error::Result;
use crate::core::model::{BookItem, Metadata};
use crate::network::client::SiteClient;
use crate::network::context::ServiceContext;

/// 任务参数映射表 (Task Parameters)
pub type TaskArgs = HashMap<String, String>;

/// 任务执行上下文 (Task Context)
#[derive(Clone)]
pub struct Context {
    /// 任务唯一标识符 (通常为书籍 ID)
    pub task_id: String,
    /// 动态任务参数
    pub args: TaskArgs,
    /// 全局服务句柄
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

/// 站点行为抽象 Trait
#[async_trait]
pub trait Site: Send + Sync {
    /// 站点唯一标识符
    fn id(&self) -> &'static str;

    /// 站点特定配置快照
    fn config(&self) -> &SiteConfig;

    /// 站点根域名 URL
    fn base_url(&self) -> &str;

    /// 获取针对该站点优化的网络客户端
    fn client(&self) -> &SiteClient;

    /// 获取书籍元数据 (Metadata Discovery)
    ///
    /// 返回元数据及可选的新发现参数（用于处理 ID 映射或重定向）。
    async fn fetch_metadata(&self, ctx: &Context) -> Result<(Metadata, Option<TaskArgs>)>;

    /// 获取资源列表 (Resource Indexing)
    ///
    /// 获取完整的章节或卷列表，需在此阶段处理分页合并。
    async fn fetch_chapter_list(&self, ctx: &Context) -> Result<Vec<BookItem>>;

    /// 获取资源正文内容 (Content Fetching)
    ///
    /// 提取章节文本，并处理内部分页的合并逻辑。
    async fn fetch_content(&self, ctx: &Context, chapter_item: &BookItem) -> Result<String>;

    /// 站点环境预热 (Environment Warm-up)
    ///
    /// 用于登录授权、初始化 Session 或提取防重放令牌 (CSRF Token)。
    async fn prepare(&self, _ctx: &Context) -> Result<()> {
        Ok(())
    }

    /// 图像流水线处理 (Image Pipeline)
    ///
    /// 执行 HTML 静态分析，提取图片 URL 并将其重写为本地 EPUB 相对路径。
    fn process_images(&self, html: &str) -> (String, Vec<String>) {
        use crate::utils::{generate_filename, to_absolute_url};
        use lol_html::{element, HtmlRewriter, Settings};
        use std::cell::RefCell;

        let images = RefCell::new(Vec::new());
        let mut output = Vec::new();
        let base_url = Url::parse(self.base_url()).ok();

        let mut rewriter = HtmlRewriter::new(
            Settings {
                element_content_handlers: vec![element!("img", |el| {
                    if let Some(src) = el.get_attribute("src") {
                        if !src.trim().is_empty() {
                            let absolute_url =
                                if let Some(original_url) = el.get_attribute("data-original-url") {
                                    original_url
                                } else {
                                    let abs = base_url
                                        .as_ref()
                                        .map(|b| to_absolute_url(b, &src))
                                        .unwrap_or_else(|| src.clone());

                                    let filename = generate_filename(&abs);
                                    let local_path = format!("../Images/{}", filename);

                                    let _ = el.set_attribute("src", &local_path);
                                    let _ = el.set_attribute("data-original-url", &abs);
                                    abs
                                };

                            images.borrow_mut().push(absolute_url);
                        }
                    }
                    Ok(())
                })],
                ..Settings::default()
            },
            |c: &[u8]| output.extend_from_slice(c),
        );

        if rewriter.write(html.as_bytes()).is_err() || rewriter.end().is_err() {
            return (html.to_string(), vec![]);
        }

        let new_html = String::from_utf8(output).unwrap_or_else(|_| html.to_string());
        (new_html, images.into_inner())
    }
}
