use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use scraper::{Html, Selector};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};
use url::Url;

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::core::model::{Book, BookItem, Chapter};
use crate::engine::client::SiteClient;
use crate::sites::{Site, SiteContext, TaskArgs};
use crate::utils::{file_exists, generate_filename, save_file, to_absolute_url};

// ============================================================================
// Task System
// ============================================================================

struct ScrapeContext {
    client: Arc<SiteClient>,
    semaphore: Arc<Semaphore>,
    images_dir: PathBuf,
    total_chapters: usize,
}

enum Task {
    Cover { url: String, path: PathBuf },
    Chapter { chapter: Chapter, path: PathBuf },
    Image { url: String, path: PathBuf },
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
            Task::Image { url, path } => Self::handle_image(url, path, &ctx).await,
            Task::Chapter { chapter, path } => Self::handle_chapter(chapter, path, &ctx).await,
        }
    }

    async fn handle_cover(url: String, path: PathBuf, ctx: &ScrapeContext) -> Result<Vec<Task>> {
        if file_exists(&path).await {
            return Ok(vec![]);
        }

        let bytes = ctx.client.fetch_bytes(&url).await?;
        save_file(&path, &bytes).await?;
        debug!("封面下载完成");
        Ok(vec![])
    }

    async fn handle_image(url: String, path: PathBuf, ctx: &ScrapeContext) -> Result<Vec<Task>> {
        if file_exists(&path).await {
            return Ok(vec![]);
        }

        match ctx.client.fetch_bytes(&url).await {
            Ok(bytes) => {
                save_file(&path, &bytes).await?;
                debug!("图片已保存: {}", url);
            }
            Err(e) => warn!("图片下载失败: {}", e),
        }
        Ok(vec![])
    }

    async fn handle_chapter(
        chapter: Chapter,
        path: PathBuf,
        ctx: &ScrapeContext,
    ) -> Result<Vec<Task>> {
        if let Ok(html) = tokio::fs::read_to_string(&path).await {
            if !html.is_empty() {
                debug!("跳过已存在章节: {}", chapter.title);
                return Ok(Self::parse_images_from_html(
                    &html,
                    &chapter.url,
                    &ctx.images_dir,
                ));
            } else {
                debug!("本地章节文件存在但为空: {}, 将重新下载", chapter.title);
            }
        }

        let raw_html = ctx.client.fetch_full_content(&chapter.url).await?;
        let (processed_content, image_urls) =
            ScrapeEngine::process_content(&raw_html, &chapter.url);

        save_file(&path, processed_content.as_bytes()).await?;
        info!(
            "[{}/{}] {}",
            chapter.index, ctx.total_chapters, chapter.title
        );

        Ok(image_urls
            .into_iter()
            .map(|url| {
                let filename = generate_filename(&url);
                Task::Image {
                    url,
                    path: ctx.images_dir.join(filename),
                }
            })
            .collect())
    }

    fn parse_images_from_html(html: &str, base_url: &str, images_dir: &PathBuf) -> Vec<Task> {
        let (_, images) = ScrapeEngine::process_content(html, base_url);
        images
            .into_iter()
            .map(|url| {
                let filename = generate_filename(&url);
                Task::Image {
                    url,
                    path: images_dir.join(filename),
                }
            })
            .collect()
    }
}

// ============================================================================
// Scrape Engine
// ============================================================================

pub struct ScrapeEngine {
    client: Arc<SiteClient>,
    config: Arc<AppConfig>,
}

impl ScrapeEngine {
    pub fn new(site: Arc<dyn Site>, ctx: SiteContext, config: Arc<AppConfig>) -> Self {
        Self {
            client: Arc::new(SiteClient::new(site, ctx)),
            config,
        }
    }

    /// 执行抓取流程
    pub async fn run(&self, mut args: TaskArgs) -> Result<()> {
        let book = self.prepare_book(&mut args).await?;

        let text_dir = book.text_dir().await;
        let cover_dir = book.cover_dir().await;
        let images_dir = book.images_dir().await;

        let chapters: Vec<_> = book.chapters().collect();
        let total_chapters = chapters.len();
        info!("共发现 {} 个章节", total_chapters);

        let concurrency = self
            .client
            .site
            .config()
            .concurrent_tasks
            .unwrap_or(self.config.spider.concurrency);

        let ctx = Arc::new(ScrapeContext {
            client: self.client.clone(),
            semaphore: Arc::new(Semaphore::new(concurrency)),
            images_dir: images_dir.clone(),
            total_chapters,
        });

        let mut join_set = JoinSet::new();
        let mut seen_images = HashSet::new();

        self.seed_cover_task(&mut join_set, &book, &cover_dir, &ctx);
        self.seed_volume_covers(&mut join_set, &mut seen_images, &book, &images_dir, &ctx);
        self.seed_chapter_tasks(&mut join_set, chapters, &text_dir, &ctx);

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
                if let Task::Image { ref url, .. } = task {
                    if !seen_images.insert(url.clone()) {
                        continue;
                    }
                }

                join_set.spawn(task.run(ctx.clone()));
            }
        }

        info!("采集任务已完成: {}", book.metadata.title);

        self.generate_epub(book).await?;

        Ok(())
    }

    fn seed_cover_task(
        &self,
        set: &mut JoinSet<Result<Vec<Task>>>,
        book: &Book,
        dir: &PathBuf,
        ctx: &Arc<ScrapeContext>,
    ) {
        if let Some(url) = &book.metadata.cover_url
            && let Some(filename) = book.metadata.cover_filename()
        {
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
        dir: &PathBuf,
        ctx: &Arc<ScrapeContext>,
    ) {
        for item in &book.items {
            if let BookItem::Volume(vol) = item
                && let Some(url) = &vol.cover_url
                && let Some(filename) = vol.cover_filename()
                && seen.insert(url.clone())
            {
                set.spawn(
                    Task::Image {
                        url: url.clone(),
                        path: dir.join(filename),
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
        dir: &PathBuf,
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
        info!("正在生成 EPUB 文件...");
        let generator = crate::core::epub::EpubGenerator::new(book.clone());
        let output_path = book.base_dir.join(format!("{}.epub", book.unique_id()));

        match generator.run(Some(output_path)).await {
            Ok(path) => {
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
        let (metadata, discovered_args) = self.client.fetch_metadata(args).await?;
        if let Some(new_args) = discovered_args {
            args.extend(new_args);
        }

        debug!("正在解析章节列表...");
        let mut items = self.fetch_all_chapters(args).await?;
        items.sort_by_key(|item| item.index());

        Ok(Book::new(
            self.client.site.id().to_string(),
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
            let (items, next) = if is_first {
                is_first = false;
                self.client.fetch_chapters(args).await?
            } else if let Some(url) = next_url {
                self.client.fetch_chapters_by_url(&url).await?
            } else {
                break;
            };

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
    fn process_content(html: &str, base_url: &str) -> (String, Vec<String>) {
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
