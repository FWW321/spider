//! 爬虫引擎
//!
//! 支持策略链和事件系统的新架构引擎

use std::collections::HashSet;
use std::path::{PathBuf, Path};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::core::event::{EventSender, SpiderEvent};
use crate::core::model::{Book, BookItem, Chapter};
use crate::interfaces::Site;
use crate::interfaces::site::TaskArgs;
use crate::network::context::ServiceContext;
use crate::utils::{file_exists, generate_filename, save_file};

// =============================================================================
// 任务上下文
// =============================================================================

struct ScrapeContext {
    site: Arc<dyn Site>,
    ctx: ServiceContext,
    semaphore: Arc<Semaphore>,
    images_dir: PathBuf,
    total_chapters: usize,
    completed_chapters: Arc<AtomicUsize>,
    events: Option<EventSender>,
}

impl ScrapeContext {
    /// 发送事件
    fn emit(&self, event: SpiderEvent) {
        if let Some(ref sender) = self.events {
            sender.emit(event);
        }
    }
}

enum Task {
    Cover { url: String, path: PathBuf },
    Chapter { chapter: Chapter, path: PathBuf },
    Image { url: String, path: PathBuf, source: String },
}

impl Task {
    async fn run(self, ctx: Arc<ScrapeContext>) -> Result<Vec<Task>> {
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

    async fn handle_cover(url: String, path: PathBuf, ctx: &ScrapeContext) -> Result<Vec<Task>> {
        if file_exists(&path).await {
            return Ok(vec![]);
        }

        let bytes = ctx
            .ctx
            .run_optimistic("下载封面", || {
                let url = url.clone();
                let site = ctx.site.clone();
                async move { site.client().get_bytes(&url).await }
            })
            .await?;

        save_file(&path, &bytes).await?;

        ctx.emit(SpiderEvent::CoverDownloaded);
        info!("封面下载完成");

        Ok(vec![])
    }

    async fn handle_image(url: String, path: PathBuf, source: String, ctx: &ScrapeContext) -> Result<Vec<Task>> {
        if file_exists(&path).await {
            return Ok(vec![]);
        }

        match ctx
            .ctx
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
        Ok(vec![])
    }

    async fn handle_chapter(
        chapter: Chapter,
        path: PathBuf,
        ctx: &ScrapeContext,
    ) -> Result<Vec<Task>> {
        // 检查缓存
        if let Ok(html) = tokio::fs::read_to_string(&path).await
            && !html.is_empty() {
                debug!("跳过已存在章节: {}", chapter.title);

                // 更新进度
                let completed = ctx.completed_chapters.fetch_add(1, Ordering::SeqCst) + 1;
                ctx.emit(SpiderEvent::ChapterProgress {
                    current: completed,
                    total: ctx.total_chapters,
                    title: chapter.title.clone(),
                });

                return Ok(Self::parse_images_from_html(
                    &html,
                    &chapter.url,
                    &ctx.images_dir,
                    format!("章节: {}", chapter.title),
                ));
            }

        // 获取内容（使用 run_optimistic 自动重试）
        let raw_html = ctx
            .ctx
            .run_optimistic(format!("下载章节: {}", chapter.title), || {
                let url = chapter.url.clone();
                let site = ctx.site.clone();
                async move {
                    site.fetcher()
                        .fetch_full_content(&url, site.client())
                        .await
                }
            })
            .await?;

        let (processed_content, image_urls) =
            ScrapeEngine::process_content(&raw_html, &chapter.url);

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

        Ok(image_urls
            .into_iter()
            .map(|url| {
                let filename = generate_filename(&url);
                Task::Image {
                    url,
                    path: ctx.images_dir.join(filename),
                    source: format!("章节: {}", chapter.title),
                }
            })
            .collect())
    }

    fn parse_images_from_html(html: &str, base_url: &str, images_dir: &Path, source: String) -> Vec<Task> {
        let (_, images) = ScrapeEngine::process_content(html, base_url);
        images
            .into_iter()
            .map(|url| {
                let filename = generate_filename(&url);
                Task::Image {
                    url,
                    path: images_dir.join(filename),
                    source: source.clone(),
                }
            })
            .collect()
    }
}

// =============================================================================
// 爬虫引擎
// =============================================================================

/// 爬虫引擎
pub struct ScrapeEngine {
    site: Arc<dyn Site>,
    ctx: ServiceContext,
    config: Arc<AppConfig>,
}

impl ScrapeEngine {
    /// 创建新的爬虫引擎
    pub fn new(site: Arc<dyn Site>, ctx: ServiceContext, config: Arc<AppConfig>) -> Self {
        Self { site, ctx, config }
    }

    /// 执行抓取流程
    pub async fn run(&self, mut args: TaskArgs) -> Result<()> {
        let run_inner = async {
            // 调用站点预热钩子
            if let Err(e) = self.site.prepare(&self.ctx).await {
                warn!("站点预热失败: {}", e);
            }

            let book = self.prepare_book(&mut args).await?;

            // 发送任务开始事件
            self.ctx.emit(SpiderEvent::TaskStarted {
                site_id: self.site.id().to_string(),
                book_id: book.id.clone(),
                title: book.metadata.title.clone(),
            });

            let text_dir = book.text_dir().await;
            let cover_dir = book.cover_dir().await;
            let images_dir = book.images_dir().await;

            let chapters: Vec<_> = book.chapters().collect();
            let total_chapters = chapters.len();

            // 发送章节发现事件
            self.ctx.emit(SpiderEvent::ChaptersDiscovered {
                total: total_chapters,
            });

            info!("共发现 {} 个章节", total_chapters);

            let concurrency = self
                .site
                .config()
                .concurrent_tasks
                .unwrap_or(self.config.spider.concurrency);

            let scrape_ctx = Arc::new(ScrapeContext {
                site: self.site.clone(),
                ctx: self.ctx.clone(),
                semaphore: Arc::new(Semaphore::new(concurrency)),
                images_dir: images_dir.clone(),
                total_chapters,
                completed_chapters: Arc::new(AtomicUsize::new(0)),
                events: self.ctx.events.clone(),
            });

            let mut join_set = JoinSet::new();
            let mut seen_images = HashSet::new();

            self.seed_cover_task(&mut join_set, &book, &cover_dir, &scrape_ctx);
            self.seed_volume_covers(
                &mut join_set,
                &mut seen_images,
                &book,
                &images_dir,
                &scrape_ctx,
            );
            self.seed_chapter_tasks(&mut join_set, chapters, &text_dir, &scrape_ctx);

            while let Some(res) = join_set.join_next().await {
                let result = match res {
                    Ok(r) => r,
                    Err(e) => {
                        error!("并发调度错误: {}", e);
                        continue;
                    }
                };

                let new_tasks = match result {
                    Ok(tasks) => tasks,
                    Err(e) => {
                        error!("任务执行失败: {}", e);
                        continue;
                    }
                };

                for task in new_tasks {
                    if let Task::Image { ref url, .. } = task
                        && !seen_images.insert(url.clone()) {
                            continue;
                        }

                    join_set.spawn(task.run(scrape_ctx.clone()));
                }
            }

            info!("采集任务已完成: {}", book.metadata.title);

            self.generate_epub(book).await?;

            // 发送任务完成事件
            self.ctx.emit(SpiderEvent::TaskCompleted {
                title: self.site.id().to_string(),
            });

            Ok::<(), SpiderError>(())
        };

        match run_inner.await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("任务执行失败: {}", e);
                self.ctx.emit(SpiderEvent::TaskFailed {
                    error: e.to_string(),
                });
                Err(e)
            }
        }
    }

    fn seed_cover_task(
        &self,
        set: &mut JoinSet<Result<Vec<Task>>>,
        book: &Book,
        dir: &Path,
        ctx: &Arc<ScrapeContext>,
    ) {
        if let Some(url) = &book.metadata.cover_url
            && let Some(filename) = book.metadata.cover_filename() {
                set.spawn(
                    Task::Cover {
                        url: url.clone(),
                        path: dir.join(filename),
                    }
                    .run(ctx.clone()),
                );
            }
    }

    fn seed_volume_covers(
        &self,
        set: &mut JoinSet<Result<Vec<Task>>>,
        seen: &mut HashSet<String>,
        book: &Book,
        dir: &Path,
        ctx: &Arc<ScrapeContext>,
    ) {
        for item in &book.items {
            if let BookItem::Volume(vol) = item
                && let Some(url) = &vol.cover_url
                    && let Some(filename) = vol.cover_filename()
                        && seen.insert(url.clone()) {
                            set.spawn(
                                Task::Image {
                                    url: url.clone(),
                                    path: dir.join(filename),
                                    source: format!("卷封面: {}", vol.title),
                                }
                                .run(ctx.clone()),
                            );
                        }
        }
    }

    fn seed_chapter_tasks(
        &self,
        set: &mut JoinSet<Result<Vec<Task>>>,
        chapters: Vec<Chapter>,
        dir: &Path,
        ctx: &Arc<ScrapeContext>,
    ) {
        for chapter in chapters {
            set.spawn(
                Task::Chapter {
                    path: dir.join(chapter.filename()),
                    chapter,
                }
                .run(ctx.clone()),
            );
        }
    }

    async fn generate_epub(&self, book: Book) -> Result<()> {
        self.ctx.emit(SpiderEvent::EpubGenerating);
        info!("正在生成 EPUB 文件...");

        let generator = crate::core::epub::EpubGenerator::new(book.clone());
        let output_path = book.base_dir.join(format!("{}.epub", book.unique_id()));

        match generator.run(Some(&output_path)).await {
            Ok(path) => {
                self.ctx.emit(SpiderEvent::EpubGenerated {
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

    async fn prepare_book(&self, args: &mut TaskArgs) -> Result<Book> {
        debug!("正在获取元数据...");
        let (metadata, discovered_args) = self
            .ctx
            .run_optimistic("获取元数据", || {
                let args = args.clone();
                let site = self.site.clone();
                async move { site.indexer().fetch_metadata(&args, site.client()).await }
            })
            .await?;

        if let Some(new_args) = discovered_args {
            args.extend(new_args);
        }

        debug!("正在解析章节列表...");
        let mut items = self.fetch_all_chapters(args).await?;
        items.sort_by_key(|item| item.index());

        Ok(Book::new(
            self.site.id().to_string(),
            self.get_id(args),
            metadata,
            items,
            PathBuf::from(&self.config.cache_path),
        ))
    }

    async fn fetch_all_chapters(&self, args: &TaskArgs) -> Result<Vec<BookItem>> {
        let mut all_items = Vec::new();
        let mut next_url: Option<String> = None;
        let mut is_first = true;

        loop {
            if !is_first && next_url.is_none() {
                break;
            }

            let (items, next) = self
                .ctx
                .run_optimistic("获取章节列表", || {
                    let args = args.clone();
                    let next_url = next_url.clone();
                    let is_first_run = is_first;
                    let site = self.site.clone();

                    async move {
                        if is_first_run {
                            site.indexer().fetch_chapters(&args, site.client()).await
                        } else if let Some(ref url) = next_url {
                            site.indexer()
                                .fetch_chapters_by_url(url, site.client())
                                .await
                        } else {
                            unreachable!("Should have broken loop if next_url is None");
                        }
                    }
                })
                .await?;

            if is_first {
                is_first = false;
            }

            all_items.extend(items);
            next_url = next;

            if next_url.is_none() {
                break;
            }
            debug!("发现下一页章节: {:?}", next_url);
        }

        Ok(all_items)
    }

    /// 处理 HTML 内容：提取图片 URL 并替换为本地路径
    pub fn process_content(html: &str, base_url: &str) -> (String, Vec<String>) {
        use crate::utils::{generate_filename, to_absolute_url};
        use scraper::{Html, Selector};
        use url::Url;

        let document = Html::parse_document(html);
        let selector = Selector::parse("img").unwrap();

        let mut images = Vec::new();
        let mut new_html = html.to_string();
        let base = Url::parse(base_url).ok();

        for element in document.select(&selector) {
            let Some(src) = element.value().attr("src") else {
                continue;
            };
            if src.is_empty() {
                continue;
            }

            // 如果已经处理过（包含 data-original-url），则直接复用
            if let Some(original_url) = element.value().attr("data-original-url") {
                images.push(original_url.to_string());
                continue;
            }

            let absolute_url = base
                .as_ref()
                .map(|b| to_absolute_url(b, src))
                .unwrap_or_else(|| src.to_string());

            images.push(absolute_url.clone());

            let filename = generate_filename(&absolute_url);
            let local_path = format!("../Images/{}", filename);
            let old_tag = element.html();

            // 构造新的 img 标签属性，保留原始链接用于追溯
            let new_tag = old_tag
                .replacen(
                    &format!("src=\"{}\"", src),
                    &format!(
                        "src=\"{}\" data-original-url=\"{}\"",
                        local_path, absolute_url
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
