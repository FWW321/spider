//! 爬虫引擎调度器
//!
//! 负责协调任务的生命周期：初始化 -> 发现 -> 执行 -> 结束

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::core::event::SpiderEvent;
use crate::core::model::{Book, BookItem, Chapter};
use crate::interfaces::site::{Context, TaskArgs};
use crate::interfaces::Site;
use crate::network::context::ServiceContext;

use super::context::RuntimeContext;
use super::task::{Task, TaskResult};

/// 爬虫引擎
pub struct ScrapeEngine {
    site: Arc<dyn Site>,
    core: ServiceContext,
    config: Arc<AppConfig>,
}

impl ScrapeEngine {
    pub fn new(site: Arc<dyn Site>, core: ServiceContext, config: Arc<AppConfig>) -> Self {
        Self { site, core, config }
    }

    /// 执行抓取流程
    pub async fn run(&self, mut args: TaskArgs) -> Result<()> {
        let task_id = self.get_id(&args);
        
        // 1. 站点预热 (Prepare)
        self.prepare_site(&task_id, &args).await;

        // 2. 书籍发现 (Discover)
        let book = match self.discover_book(&task_id, &mut args).await {
            Ok(b) => b,
            Err(e) => {
                self.fail_task(e.to_string());
                return Err(e);
            }
        };

        // 3. 任务执行循环 (Loop)
        // 传递引用而非 Clone，减少内存开销
        if let Err(e) = self.execute_loop(&book, &args, task_id).await {
            self.fail_task(e.to_string());
            return Err(e);
        }

        // 4. 后处理 (Post-process)
        self.generate_epub(book).await?;
        self.finish_task();

        Ok(())
    }

    async fn prepare_site(&self, task_id: &str, args: &TaskArgs) {
        let site_ctx = Context::new(task_id.to_string(), args.clone(), self.core.clone());
        if let Err(e) = self.site.prepare(&site_ctx).await {
            warn!("站点预热警告: {}", e);
        }
    }

    async fn discover_book(&self, task_id: &str, args: &mut TaskArgs) -> Result<Book> {
        debug!("正在获取元数据...");
        let (metadata, discovered_args) = self
            .core
            .run_optimistic("获取元数据", || {
                let site_ctx = Context::new(task_id.to_string(), args.clone(), self.core.clone());
                let site = self.site.clone();
                async move { site.fetch_metadata(&site_ctx).await }
            })
            .await?;

        if let Some(new_args) = discovered_args {
            args.extend(new_args);
        }

        debug!("正在获取章节列表...");
        // 更新 args 后重新创建 context
        let site_ctx = Context::new(task_id.to_string(), args.clone(), self.core.clone());

        let mut items = self
            .core
            .run_optimistic("获取章节列表", || {
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

    async fn execute_loop(&self, book: &Book, args: &TaskArgs, task_id: String) -> Result<()> {
        let text_dir = book.text_dir().await;
        let cover_dir = book.cover_dir().await;
        let images_dir = book.images_dir().await;

        let chapters: Vec<_> = book.chapters().collect();
        let total_chapters = chapters.len();

        self.core.emit(SpiderEvent::ChaptersDiscovered {
            total: total_chapters,
        });
        info!("共发现 {} 个章节", total_chapters);

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

        // 初始化待执行任务队列
        let mut pending_tasks = self.create_initial_tasks(
            book, 
            &text_dir, 
            &cover_dir, 
            &images_dir,
            &mut seen_images
        );

        // 主循环：只要还有等待的任务或正在运行的任务，就继续
        while !pending_tasks.is_empty() || !join_set.is_empty() {
            // 1. 填充任务槽 (Fill Slots)
            self.fill_task_slots(&mut join_set, &mut pending_tasks, concurrency, &ctx);

            // 2. 等待结果 (Wait)
            if let Some(res) = join_set.join_next().await {
                self.handle_task_result(res, &mut pending_tasks, &mut seen_images, &mut failures);
            }
        }

        if !failures.is_empty() {
            error!("==========================================");
            error!("任务完成，但在 {} 个项目中遇到错误:", failures.len());
            for (desc, e) in &failures {
                error!(" - {}: {}", desc, e);
            }
            error!("==========================================");
        } else {
            info!("采集任务全部成功完成: {}", book.metadata.title);
        }

        Ok(())
    }

    /// 填充任务槽
    fn fill_task_slots(
        &self,
        join_set: &mut JoinSet<std::result::Result<TaskResult, (String, crate::core::error::SpiderError)>>,
        pending_tasks: &mut VecDeque<Task>,
        concurrency: usize,
        ctx: &Arc<RuntimeContext>,
    ) {
        while join_set.len() < concurrency && let Some(task) = pending_tasks.pop_front() {
            let task_ctx = ctx.clone();
            let task_desc = task.to_string();
            
            join_set.spawn(async move {
                // 内部使用了 run_optimistic，会自动处理阻断和重试
                task.run(task_ctx).await.map_err(|e| (task_desc, e))
            });
        }
    }

    /// 处理任务结果
    fn handle_task_result(
        &self,
        res: std::result::Result<std::result::Result<TaskResult, (String, crate::core::error::SpiderError)>, tokio::task::JoinError>,
        pending_tasks: &mut VecDeque<Task>,
        seen_images: &mut HashSet<String>,
        failures: &mut Vec<(String, crate::core::error::SpiderError)>,
    ) {
        match res {
            Ok(Ok(task_res)) => {
                if let TaskResult::Spawn(new_tasks) = task_res {
                    for task in new_tasks {
                        // 只有未见过的图片任务才添加
                        if let Task::Image { ref url, .. } = task {
                            if !seen_images.insert(url.clone()) {
                                continue;
                            }
                        }
                        pending_tasks.push_front(task);
                    }
                }
            }
            Ok(Err((desc, e))) => {
                // 因为 run_optimistic 会自动处理阻断并重试
                // 所以如果到了这里，说明是真正的不可恢复错误（如文件IO错误、解析错误等）
                // 或者重试次数彻底耗尽
                error!("任务彻底失败 [{}]: {}", desc, e);
                failures.push((desc, e));
            }
            Err(e) => {
                // 选择记录错误但不直接 panic，尽量保证其他任务能跑完
                // 但如果是 panic，可能意味着严重的逻辑错误
                error!("致命错误 (Panic/Cancel): {}", e);
            }
        }
    }

    fn create_initial_tasks(
        &self, 
        book: &Book, 
        text_dir: &Path,
        cover_dir: &Path,
        images_dir: &Path,
        seen_images: &mut HashSet<String>,
    ) -> VecDeque<Task> {
        let mut tasks = VecDeque::new();

         // 1. 书籍封面
        if let Some(url) = &book.metadata.cover_url
            && let Some(filename) = book.metadata.cover_filename()
        {
            tasks.push_back(Task::Cover {
                url: url.clone(),
                path: cover_dir.join(filename),
            });
        }

        // 2. 卷封面
        for item in &book.items {
            if let BookItem::Volume(vol) = item
                && let Some(url) = &vol.cover_url
                    && let Some(filename) = vol.cover_filename()
                        && seen_images.insert(url.clone())
            {
                tasks.push_back(
                    Task::Image {
                        url: url.clone(),
                        path: images_dir.join(filename),
                        source: format!("卷封面: {}", vol.title),
                    }
                );
            }
        }

        // 3. 章节内容
         for chapter in book.chapters() {
            tasks.push_back(
                Task::Chapter {
                    path: text_dir.join(chapter.filename()),
                    chapter: chapter.clone(),
                }
            );
        }

        tasks
    }

    async fn generate_epub(&self, book: Book) -> Result<()> {
        self.core.emit(SpiderEvent::EpubGenerating);
        info!("正在生成 EPUB 文件...");

        let generator = crate::core::epub::EpubGenerator::new(book.clone());
        let output_path = book.base_dir.join(format!("{}.epub", book.unique_id()));

        match generator.run(Some(&output_path)).await {
            Ok(path) => {
                self.core.emit(SpiderEvent::EpubGenerated {
                    path: path.display().to_string(),
                });
                info!("EPUB 生成成功: {:?}", path);
                Ok(())
            }
            Err(e) => {
                error!("EPUB 生成失败: {}", e);
                Ok(())
            }
        }
    }

    fn fail_task(&self, error: String) {
        error!("任务执行失败: {}", error);
        self.core.emit(SpiderEvent::TaskFailed { error });
    }

    fn finish_task(&self) {
        self.core.emit(SpiderEvent::TaskCompleted {
            title: self.site.id().to_string(),
        });
    }

    /// 解析 ID
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