//! 任务单元定义

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracing::{debug, info, warn};

use crate::core::error::{Result, SpiderError};
use crate::core::event::SpiderEvent;
use crate::core::model::{BookItem, Chapter};
use crate::utils::{file_exists, generate_filename, save_file};

use super::context::RuntimeContext;

/// 任务执行结果
pub enum TaskResult {
    /// 任务完成，无后续动作
    Completed,
    /// 任务完成，产生新任务
    Spawn(Vec<Task>),
    /// 任务被跳过 (如文件已存在)
    Skipped,
}

/// 具体的下载任务
#[derive(Debug, Clone)]
pub enum Task {
    Cover {
        url: String,
        path: PathBuf,
    },
    Chapter {
        chapter: Chapter,
        path: PathBuf,
    },
    Image {
        url: String,
        path: PathBuf,
        source: String,
    },
}

impl std::fmt::Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Task::Cover { url, .. } => write!(f, "下载封面 ({})", url),
            Task::Chapter { chapter, .. } => write!(f, "下载章节 {} - {}", chapter.index, chapter.title),
            Task::Image { source, url, .. } => write!(f, "下载图片 [{}] ({})", source, url),
        }
    }
}

impl Task {
    /// 执行任务
    pub async fn run(self, ctx: Arc<RuntimeContext>) -> Result<TaskResult> {
        let _permit = ctx
            .semaphore
            .acquire()
            .await
            .map_err(|e| SpiderError::Custom(e.to_string()))?;

        match self {
            Task::Cover { url, path } => Self::handle_cover(url, path, &ctx).await,
            Task::Image { url, path, source } => Self::handle_image(url, path, source, &ctx).await,
            Task::Chapter { chapter, path } => Self::handle_chapter(chapter, path, &ctx).await,
        }
    }

    async fn handle_cover(
        url: String,
        path: PathBuf,
        ctx: &RuntimeContext,
    ) -> Result<TaskResult> {
        if file_exists(&path).await {
            return Ok(TaskResult::Skipped);
        }

        let bytes = ctx
            .core
            .run_optimistic("下载封面", || {
                let url = url.clone();
                let site = ctx.site.clone();
                async move { site.client().get_bytes(&url).await }
            })
            .await?;

        save_file(&path, &bytes).await?;

        ctx.emit(SpiderEvent::CoverDownloaded);
        info!("封面下载完成");

        Ok(TaskResult::Completed)
    }

    async fn handle_image(
        url: String,
        path: PathBuf,
        source: String,
        ctx: &RuntimeContext,
    ) -> Result<TaskResult> {
        if file_exists(&path).await {
            return Ok(TaskResult::Skipped);
        }

        match ctx
            .core
            .run_optimistic(format!("下载图片: {}", url), || {
                let url = url.clone();
                let site = ctx.site.clone();
                async move { site.client().get_bytes(&url).await }
            })
            .await
        {
            Ok(bytes) => {
                save_file(&path, &bytes).await?;
                info!("图片已保存 [{}] : {}", source, url);
            }
            Err(e) => {
                warn!("图片下载失败 [{}]: {}", source, e);
            }
        }
        Ok(TaskResult::Completed)
    }

    async fn handle_chapter(
        chapter: Chapter,
        path: PathBuf,
        ctx: &RuntimeContext,
    ) -> Result<TaskResult> {
        // 检查缓存
        if let Ok(html) = tokio::fs::read_to_string(&path).await
            && !html.is_empty()
        {
            debug!("跳过已存在章节: {}", chapter.title);

            // 更新进度
            let completed = ctx.completed_chapters.fetch_add(1, Ordering::SeqCst) + 1;
            ctx.emit(SpiderEvent::ChapterProgress {
                current: completed,
                total: ctx.total_chapters,
                title: chapter.title.clone(),
            });

            // 即便是缓存的 HTML，也需要解析图片链接以确保图片被下载
            let (_, images) = ctx.site.process_images(&html);
            return Ok(TaskResult::Spawn(Self::create_image_tasks(images, &ctx.images_dir, &chapter.title)));
        }

        // 获取内容
        let raw_html = ctx
            .core
            .run_optimistic(format!("下载章节: {}", chapter.title), || {
                let site_ctx = ctx.make_site_context();
                let item = BookItem::Chapter(chapter.clone());
                let site = ctx.site.clone();
                async move { site.fetch_content(&site_ctx, &item).await }
            })
            .await?;

        // 处理内容 (图片提取 + 链接替换)
        let (processed_content, image_urls) = ctx.site.process_images(&raw_html);

        save_file(&path, processed_content.as_bytes()).await?;

        // 更新进度
        let completed = ctx.completed_chapters.fetch_add(1, Ordering::SeqCst) + 1;
        ctx.emit(SpiderEvent::ChapterProgress {
            current: completed,
            total: ctx.total_chapters,
            title: chapter.title.clone(),
        });
        ctx.emit(SpiderEvent::ChapterCompleted {
            index: chapter.index as usize,
            title: chapter.title.clone(),
        });

        info!(
            "[/{}/{}] {}",
            chapter.index, ctx.total_chapters, chapter.title
        );

        Ok(TaskResult::Spawn(Self::create_image_tasks(image_urls, &ctx.images_dir, &chapter.title)))
    }

    fn create_image_tasks(images: Vec<String>, images_dir: &Path, source: &str) -> Vec<Task> {
        images
            .into_iter()
            .map(|url| {
                let filename = generate_filename(&url);
                Task::Image {
                    url,
                    path: images_dir.join(filename),
                    source: format!("章节: {}", source),
                }
            })
            .collect()
    }
}
