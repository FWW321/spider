//! 爬虫调度引擎 (Orchestration Engine)
//!
//! 负责协调任务的完整生命周期：初始化 (Preparation) -> 发现 (Discovery) -> 执行 (Execution) -> 后处理 (Post-processing)。

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::core::event::SpiderEvent;
use crate::core::model::{Book, BookItem, Chapter};
use crate::interfaces::Site;
use crate::interfaces::site::{Context, TaskArgs};
use crate::network::context::ServiceContext;

use super::context::RuntimeContext;
use super::task::{Task, TaskResult};

/// 核心调度引擎
pub struct ScrapeEngine {
    /// 目标站点抽象
    site: Arc<dyn Site>,
    /// 网络与全局状态上下文
    core: ServiceContext,
    /// 全局配置
    config: Arc<AppConfig>,
}

impl ScrapeEngine {
    /// 创建引擎实例
    pub fn new(site: Arc<dyn Site>, core: ServiceContext, config: Arc<AppConfig>) -> Self {
        Self { site, core, config }
    }

    /// 执行完整的抓取任务流
    pub async fn run(&self, mut args: TaskArgs) -> Result<()> {
        let task_id = self.get_id(&args);

        // 1. 站点环境初始化 (Site Warm-up)
        self.prepare_site(&task_id, &args).await;

        // 2. 资源发现阶段 (Discovery Phase)
        let book = match self.discover_book(&task_id, &mut args).await {
            Ok(b) => b,
            Err(e) => {
                self.fail_task(e.to_string());
                return Err(e);
            }
        };

        // 3. 并发抓取循环 (Concurrent Execution)
        if let Err(e) = self.execute_loop(&book, &args, task_id).await {
            self.fail_task(e.to_string());
            return Err(e);
        }

        // 4. 文档生成与清理 (Post-processing)
        self.generate_epub(book).await?;
        self.finish_task();

        Ok(())
    }

    /// 站点环境预热
    async fn prepare_site(&self, task_id: &str, args: &TaskArgs) {
        let site_ctx = Context::new(task_id.to_string(), args.clone(), self.core.clone());
        if let Err(e) = self.site.prepare(&site_ctx).await {
            warn!("Site preparation warning: {}", e);
        }
    }

    /// 执行元数据与目录结构的发现
    async fn discover_book(&self, task_id: &str, args: &mut TaskArgs) -> Result<Book> {
        debug!("Fetching metadata...");
        let (metadata, discovered_args) = self
            .core
            .run_optimistic("Metadata Discovery", || {
                let site_ctx = Context::new(task_id.to_string(), args.clone(), self.core.clone());
                let site = self.site.clone();
                async move { site.fetch_metadata(&site_ctx).await }
            })
            .await?;

        if let Some(new_args) = discovered_args {
            args.extend(new_args);
        }

        debug!("Fetching chapter list...");
        let site_ctx = Context::new(task_id.to_string(), args.clone(), self.core.clone());

        let mut items = self
            .core
            .run_optimistic("Chapter Discovery", || {
                let ctx = site_ctx.clone();
                let site = self.site.clone();
                async move { site.fetch_chapter_list(&ctx).await }
            })
            .await?;

        items.sort_by_key(|item| item.index());

        let book = Book::new(
            self.site.id().to_string(),
            task_id.to_string(),
            metadata,
            items,
            PathBuf::from(&self.config.cache_path),
        );

        self.core.emit(SpiderEvent::TaskStarted {
            site_id: self.site.id().to_string(),
            book_id: book.id.clone(),
            title: book.metadata.title.clone(),
        });

        Ok(book)
    }

    /// 并发任务执行循环
    async fn execute_loop(&self, book: &Book, args: &TaskArgs, task_id: String) -> Result<()> {
        let text_dir = book.text_dir().await;
        let cover_dir = book.cover_dir().await;
        let images_dir = book.images_dir().await;

        let chapters: Vec<_> = book.chapters().collect();
        let total_chapters = chapters.len();

        self.core.emit(SpiderEvent::ChaptersDiscovered {
            total: total_chapters,
        });
        info!("Found {} chapters", total_chapters);

        let concurrency = self
            .site
            .config()
            .concurrent_tasks
            .unwrap_or(self.config.spider.concurrency);

        let ctx = Arc::new(RuntimeContext::new(
            self.site.clone(),
            self.core.clone(),
            concurrency,
            images_dir.clone(),
            total_chapters,
            self.core.events.clone(),
            Arc::new(args.clone()),
            task_id,
        ));

        let mut join_set = JoinSet::new();
        let mut seen_images = HashSet::new();
        let mut failures = Vec::new();

        let mut pending_tasks =
            self.create_initial_tasks(book, &text_dir, &cover_dir, &images_dir, &mut seen_images);

        while !pending_tasks.is_empty() || !join_set.is_empty() {
            // 填充并发槽位 (Concurrency Throttling)
            self.fill_task_slots(&mut join_set, &mut pending_tasks, concurrency, &ctx);

            // 处理已完成的任务结果
            if let Some(res) = join_set.join_next().await {
                self.handle_task_result(res, &mut pending_tasks, &mut seen_images, &mut failures);
            }
        }

        if !failures.is_empty() {
            error!("==========================================");
            error!("Execution completed with {} failures:", failures.len());
            for (desc, e) in &failures {
                error!(" - {}: {}", desc, e);
            }
            error!("==========================================");
        } else {
            info!("Scraping task completed successfully: {}", book.metadata.title);
        }

        Ok(())
    }

    /// 并发槽位填充逻辑 (Fill Slots)
    fn fill_task_slots(
        &self,
        join_set: &mut JoinSet<
            std::result::Result<TaskResult, (String, crate::core::error::SpiderError)>,
        >,
        pending_tasks: &mut VecDeque<Task>,
        concurrency: usize,
        ctx: &Arc<RuntimeContext>,
    ) {
        while join_set.len() < concurrency
            && let Some(task) = pending_tasks.pop_front()
        {
            let task_ctx = ctx.clone();
            let task_desc = task.to_string();

            join_set.spawn(async move {
                // 利用 run_optimistic 机制处理熔断与重试
                task.run(task_ctx).await.map_err(|e| (task_desc, e))
            });
        }
    }

    /// 任务结果传播与后续任务生成
    fn handle_task_result(
        &self,
        res: std::result::Result<
            std::result::Result<TaskResult, (String, crate::core::error::SpiderError)>,
            tokio::task::JoinError,
        >,
        pending_tasks: &mut VecDeque<Task>,
        seen_images: &mut HashSet<String>,
        failures: &mut Vec<(String, crate::core::error::SpiderError)>,
    ) {
        match res {
            Ok(Ok(TaskResult::Spawn(new_tasks))) => {
                for task in new_tasks {
                    match task {
                        // 基于 URL 的图片去重过滤
                        Task::Image { ref url, .. } if !seen_images.insert(url.clone()) => continue,
                        _ => pending_tasks.push_front(task),
                    }
                }
            }
            Ok(Err((desc, e))) => {
                error!("Task failed fatally [{}]: {}", desc, e);
                failures.push((desc, e));
            }
            Err(e) => {
                error!("Runtime panic or cancellation: {}", e);
            }
            _ => {}
        }
    }

    /// 构建初始任务队列
    fn create_initial_tasks(
        &self,
        book: &Book,
        text_dir: &Path,
        cover_dir: &Path,
        images_dir: &Path,
        seen_images: &mut HashSet<String>,
    ) -> VecDeque<Task> {
        let mut tasks = VecDeque::new();

        // 书籍封面下载
        if let Some(url) = &book.metadata.cover_url
            && let Some(filename) = book.metadata.cover_filename()
        {
            tasks.push_back(Task::Cover {
                url: url.clone(),
                path: cover_dir.join(filename),
            });
        }

        // 卷封面下载 (基于唯一 URL 过滤)
        for item in &book.items {
            if let BookItem::Volume(vol) = item
                && let Some(url) = &vol.cover_url
                && let Some(filename) = vol.cover_filename()
                && seen_images.insert(url.clone())
            {
                tasks.push_back(Task::Image {
                    url: url.clone(),
                    path: images_dir.join(filename),
                    source: format!("Volume Cover: {}", vol.title),
                });
            }
        }

        // 章节内容采集
        for chapter in book.chapters() {
            tasks.push_back(Task::Chapter {
                path: text_dir.join(chapter.filename()),
                chapter: chapter.clone(),
            });
        }

        tasks
    }

    /// 执行 EPUB 编译与生成
    async fn generate_epub(&self, book: Book) -> Result<()> {
        self.core.emit(SpiderEvent::EpubGenerating);
        info!("Generating EPUB artifact...");

        let generator = crate::core::epub::EpubGenerator::new(book.clone());
        let output_path = book.base_dir.join(format!("{}.epub", book.unique_id()));

        match generator.run(Some(&output_path)).await {
            Ok(path) => {
                self.core.emit(SpiderEvent::EpubGenerated {
                    path: path.display().to_string(),
                });
                info!("EPUB generation successful: {:?}", path);
                Ok(())
            }
            Err(e) => {
                error!("EPUB generation failed: {}", e);
                Ok(())
            }
        }
    }

    fn fail_task(&self, error: String) {
        error!("Task failed: {}", error);
        self.core.emit(SpiderEvent::TaskFailed { error });
    }

    fn finish_task(&self) {
        self.core.emit(SpiderEvent::TaskCompleted {
            title: self.site.id().to_string(),
        });
    }

    /// 从参数集中提取任务唯一标识符
    pub fn get_id(&self, args: &TaskArgs) -> String {
        args.get("id")
            .or_else(|| args.iter().find(|(k, _)| k.contains("id")).map(|(_, v)| v))
            .or_else(|| args.get("name"))
            .or_else(|| {
                args.iter()
                    .find(|(k, _)| k.contains("name"))
                    .map(|(_, v)| v)
            })
            .or_else(|| args.values().next())
            .cloned()
            .unwrap_or_else(|| "unknown".to_string())
    }
}
