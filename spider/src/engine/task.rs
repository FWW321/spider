//! 任务原子单元 (Atomic Task Units)
//!
//! 定义抓取流程中可独立执行的最小操作单元，并处理其生命周期与状态转换。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracing::{debug, info, warn};

use crate::core::error::{Result, SpiderError};
use crate::core::event::SpiderEvent;
use crate::core::model::{BookItem, Chapter};
use crate::utils::{file_exists, generate_filename, save_file};

use super::context::RuntimeContext;

/// 任务执行反馈 (Execution Result)
pub enum TaskResult {
    /// 任务正常终止
    Completed,
    /// 任务执行成功，并生成后续衍生任务 (例如从章节 HTML 提取出图片下载任务)
    Spawn(Vec<Task>),
    /// 任务被静默跳过 (例如命中物理文件缓存)
    Skipped,
}

/// 可并发执行的下载任务变体
#[derive(Debug, Clone)]
pub enum Task {
    /// 书籍封面下载
    Cover {
        url: String,
        path: PathBuf,
    },
    /// 章节正文抓取
    Chapter {
        chapter: Chapter,
        path: PathBuf,
    },
    /// 媒体资源下载
    Image {
        url: String,
        path: PathBuf,
        source: String,
    },
}

impl std::fmt::Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Task::Cover { url, .. } => write!(f, "Cover Download ({})", url),
            Task::Chapter { chapter, .. } => {
                write!(f, "Chapter Fetch {} - {}", chapter.index, chapter.title)
            }
            Task::Image { source, url, .. } => write!(f, "Image Download [{}] ({})", source, url),
        }
    }
}

impl Task {
    /// 驱动任务执行，并应用并发节流 (Concurrency Throttling)
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

    async fn handle_cover(url: String, path: PathBuf, ctx: &RuntimeContext) -> Result<TaskResult> {
        if file_exists(&path).await {
            return Ok(TaskResult::Skipped);
        }

        let bytes = ctx
            .core
            .run_optimistic("Cover Download", || {
                let url = url.clone();
                let site = ctx.site.clone();
                async move { site.client().get_bytes(&url).await }
            })
            .await?;

        save_file(&path, &bytes).await?;

        ctx.emit(SpiderEvent::CoverDownloaded);
        info!("Cover artifact saved");

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
            .run_optimistic(format!("Image Download: {}", url), || {
                let url = url.clone();
                let site = ctx.site.clone();
                async move { site.client().get_bytes(&url).await }
            })
            .await
        {
            Ok(bytes) => {
                save_file(&path, &bytes).await?;
                info!("Image saved [{}] : {}", source, url);
            }
            Err(e) => {
                warn!("Image download failed [{}]: {}", source, e);
            }
        }
        Ok(TaskResult::Completed)
    }

    async fn handle_chapter(
        chapter: Chapter,
        path: PathBuf,
        ctx: &RuntimeContext,
    ) -> Result<TaskResult> {
        // 物理缓存命检 (Cache Hit Check)
        if let Ok(html) = tokio::fs::read_to_string(&path).await
            && !html.is_empty()
        {
            debug!("Cache hit for chapter: {}", chapter.title);

            let completed = ctx.completed_chapters.fetch_add(1, Ordering::SeqCst) + 1;
            ctx.emit(SpiderEvent::ChapterProgress {
                current: completed,
                total: ctx.total_chapters,
                title: chapter.title.clone(),
            });

            // 仍需重构图片任务以确保资源清单完整性
            let (_, images) = ctx.site.process_images(&html);
            return Ok(TaskResult::Spawn(Self::create_image_tasks(
                images,
                &ctx.images_dir,
                &chapter.title,
            )));
        }

        let raw_html = ctx
            .core
            .run_optimistic(format!("Chapter Fetch: {}", chapter.title), || {
                let site_ctx = ctx.make_site_context();
                let item = BookItem::Chapter(chapter.clone());
                let site = ctx.site.clone();
                async move { site.fetch_content(&site_ctx, &item).await }
            })
            .await?;

        // 资源重写与图像清单提取 (Resource Manifest Extraction)
        let (processed_content, image_urls) = ctx.site.process_images(&raw_html);

        save_file(&path, processed_content.as_bytes()).await?;

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
            "Progress [/{}/{}] {}",
            chapter.index, ctx.total_chapters, chapter.title
        );

        Ok(TaskResult::Spawn(Self::create_image_tasks(
            image_urls,
            &ctx.images_dir,
            &chapter.title,
        )))
    }

    /// 根据 URL 列表构建衍生下载任务
    fn create_image_tasks(images: Vec<String>, images_dir: &Path, source: &str) -> Vec<Task> {
        images
            .into_iter()
            .map(|url| {
                let filename = generate_filename(&url);
                Task::Image {
                    url,
                    path: images_dir.join(filename),
                    source: format!("Chapter: {}", source),
                }
            })
            .collect()
    }
}
