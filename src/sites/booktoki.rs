use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use scraper::{ElementRef, Html, Selector};
use serde_json::json;
use tokio::fs;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use url::Url;

use crate::core::config::SiteConfig;
use crate::core::error::{Result, SpiderError};
use crate::core::model::{BookItem, Chapter, Metadata};
use crate::sites::{Guardian, Parser, Site, SiteContext, TaskArgs, UrlBuilder};
use crate::utils::to_absolute_url;

// --- 预编译 Selectors ---
struct SiteSelectors {
    detail_desc: Selector,
    view_content: Selector,
    view_img: Selector,
    bold: Selector,
    icon_user: Selector,
    icon_tags: Selector,
    icon_building: Selector,
    wr_none: Selector,
    list_item: Selector,
    wr_subject_link: Selector,
    wr_num: Selector,
    pagination_next: Selector,
    novel_content: Selector,
    paragraph: Selector,
}

static SELECTORS: OnceLock<SiteSelectors> = OnceLock::new();

impl SiteSelectors {
    fn get() -> &'static SiteSelectors {
        SELECTORS.get_or_init(|| SiteSelectors {
            detail_desc: Selector::parse("div[itemprop='description']").unwrap(),
            view_content: Selector::parse("div.view-content").unwrap(),
            view_img: Selector::parse("div.view-img img").unwrap(),
            bold: Selector::parse("b").unwrap(),
            icon_user: Selector::parse("i.fa-user").unwrap(),
            icon_tags: Selector::parse("i.fa-tags").unwrap(),
            icon_building: Selector::parse("i.fa-building-o").unwrap(),
            wr_none: Selector::parse("div.wr-none").unwrap(),
            list_item: Selector::parse("ul.list-body > li.list-item").unwrap(),
            wr_subject_link: Selector::parse("div.wr-subject > a").unwrap(),
            wr_num: Selector::parse("div.wr-num").unwrap(),
            pagination_next: Selector::parse("ul.pagination li.active + li a[href]").unwrap(),
            novel_content: Selector::parse("div#novel_content").unwrap(),
            paragraph: Selector::parse("p").unwrap(),
        })
    }
}

pub struct Booktoki {
    config: SiteConfig,
    base: Url,
    recovery_lock: Mutex<()>,
}

impl Booktoki {
    pub fn new(config: SiteConfig) -> Self {
        let base_url = config
            .base_url
            .as_deref()
            .unwrap_or("https://booktoki469.com");
        Self {
            base: Url::parse(base_url).expect("Invalid base URL"),
            config,
            recovery_lock: Mutex::new(()),
        }
    }

    #[inline]
    fn normalize(&self, path: &str) -> String {
        to_absolute_url(&self.base, path)
    }

    fn extract_icon_text(&self, parent: &ElementRef, selector: &Selector) -> Option<String> {
        parent
            .select(selector)
            .next()
            .and_then(|el| el.next_sibling())
            .and_then(|n| n.value().as_text())
            .map(|t| t.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

// ============================================================================
// Trait 实现
// ============================================================================

impl UrlBuilder for Booktoki {
    fn base_url(&self) -> &str {
        self.base.as_str()
    }

    fn build_url(&self, kind: &str, args: &TaskArgs) -> Result<String> {
        match kind {
            "metadata" | "chapters" => {
                let id = args
                    .get("id")
                    .ok_or_else(|| SpiderError::Parse("Missing id".into()))?;
                Ok(self.normalize(&format!("/novel/{}", id)))
            }
            _ => Err(SpiderError::Parse(format!("Unknown kind: {}", kind))),
        }
    }
}

impl Parser for Booktoki {
    fn parse_metadata(&self, html: &str) -> Result<(Metadata, Option<TaskArgs>)> {
        let doc = Html::parse_document(html);
        let s = SiteSelectors::get();

        let detail = doc
            .select(&s.detail_desc)
            .next()
            .ok_or_else(|| SpiderError::Parse("Detail element not found".into()))?;

        let mut contents = detail.select(&s.view_content);

        let title = contents
            .next()
            .and_then(|el| el.select(&s.bold).next())
            .map(|b| b.text().collect::<String>())
            .unwrap_or_else(|| "Unknown Title".into());

        let (mut author, mut tags, mut publisher) = (None, Vec::new(), None);
        if let Some(info) = contents.next() {
            author = self.extract_icon_text(&info, &s.icon_user);

            if let Some(tag_str) = self.extract_icon_text(&info, &s.icon_tags) {
                tags = tag_str.split(',').map(|s| s.trim().to_string()).collect();
            }

            if let Some(pub_str) = self.extract_icon_text(&info, &s.icon_building) {
                publisher = Some(format!("booktoki: {}", pub_str));
            }
        }

        let summary = contents
            .next()
            .map(|el| el.text().collect::<Vec<_>>().join("\n").trim().to_string());

        let cover_url = detail
            .select(&s.view_img)
            .next()
            .and_then(|img| img.value().attr("src"))
            .map(|url| self.normalize(url));

        Ok((
            Metadata {
                title,
                author,
                language: "ko".into(),
                summary,
                cover_url,
                tags,
                publisher,
            },
            None,
        ))
    }

    fn parse_chapters(&self, html: &str) -> Result<(Vec<BookItem>, Option<String>)> {
        if html.is_empty() {
            return Ok((vec![], None));
        }

        let doc = Html::parse_document(html);
        let s = SiteSelectors::get();

        if doc.select(&s.wr_none).next().is_some() {
            return Ok((vec![], None));
        }

        let chapters = doc
            .select(&s.list_item)
            .filter_map(|item| {
                let link = item.select(&s.wr_subject_link).next()?;
                let url = link.value().attr("href")?.to_string();
                if url.is_empty() {
                    return None;
                }

                let id = item
                    .select(&s.wr_num)
                    .next()
                    .map(|el| el.text().collect::<String>().trim().to_string())
                    .unwrap_or_else(|| "0".to_string());

                let title = link
                    .children()
                    .filter_map(|node| node.value().as_text())
                    .map(|t| t.trim())
                    .collect::<String>()
                    .trim()
                    .to_string();

                Some(BookItem::Chapter(Chapter {
                    index: id.parse().unwrap_or(0),
                    id,
                    title,
                    url: self.normalize(&url),
                }))
            })
            .collect();

        let next_url = doc
            .select(&s.pagination_next)
            .next()
            .and_then(|a| a.value().attr("href"))
            .filter(|href| !href.is_empty() && !href.starts_with('#'))
            .map(|href| self.normalize(href));

        Ok((chapters, next_url))
    }

    fn parse_content(&self, html: &str) -> Result<(String, Option<String>)> {
        let doc = Html::parse_document(html);
        let s = SiteSelectors::get();

        let node = match doc.select(&s.novel_content).next() {
            Some(n) => n,
            None => {
                self.save_failed_html(html);
                return Err(SpiderError::Parse(
                    "Novel content container not found".into(),
                ));
            }
        };

        let content = node
            .select(&s.paragraph)
            .filter_map(|p| {
                let text = p.text().collect::<String>();
                let trimmed = text.trim();

                let is_spam = trimmed.len() > 40
                    && !trimmed.is_empty()
                    && trimmed.chars().all(|c| c.is_alphanumeric());

                if is_spam {
                    None
                } else {
                    Some(p.html().trim().to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok((content, None))
    }
}

#[async_trait]
impl Guardian for Booktoki {
    fn check_block(&self, url: &str, html: &str, status: u16) -> Result<()> {
        let lower = html.to_lowercase();

        if lower.contains("just a moment...") {
            debug!("检测到 Cloudflare 阻断: {}", url);
            return Err(SpiderError::SoftBlock("cloudflare".into()));
        }

        if status == 403 {
            debug!("检测到 403 禁止访问: {}", url);
            return Err(SpiderError::SoftBlock("ip_blocked".into()));
        }

        if url.contains("captcha") || lower.contains("id=\"captcha\"") {
            debug!("检测到验证码页面或异常访问: {}", url);
            return Err(SpiderError::SoftBlock("captcha".into()));
        }

        Ok(())
    }

    async fn recover(&self, url: &str, reason: &str, ctx: &SiteContext) -> Result<bool> {
        let _guard = self.recovery_lock.lock().await;
        debug!("开始阻断恢复: {} (原因: {})", url, reason);

        if let Ok((status, html)) = ctx.probe(url).await {
            if self.check_block(url, &html, status).is_ok() {
                debug!("页面已由其他任务恢复");
                return Ok(true);
            }
        }

        match reason {
            "ip_blocked" => {
                info!("IP 被封禁，正在切换线路...");
                ctx.session.clear();
                ctx.rotate_proxy().await;
                tokio::time::sleep(Duration::from_secs(1)).await;
                ctx.bypasser.bypass(url, ctx).await?;
            }
            "captcha" => {
                info!("检测到验证码，正在尝试识别...");
                self.solve_captcha(url, ctx).await?;
            }
            "cloudflare" | "soft_block_detected" => {
                info!("正在通过浏览器绕过 Cloudflare...");
                ctx.rotate_proxy().await;
                ctx.bypasser.bypass(url, ctx).await?;
            }
            _ => {
                info!("检测到访问受限，正在尝试修复...");
                ctx.bypasser.bypass(url, ctx).await?;
            }
        }

        Ok(true)
    }
}

impl Booktoki {
    async fn solve_captcha(&self, url: &str, ctx: &SiteContext) -> Result<()> {
        debug!("正在获取验证码会话...");

        let session_url = self.normalize("/plugin/kcaptcha/kcaptcha_session.php");
        // 获取 Session 时也不要通过中间件，避免死锁
        ctx.http
            .post(&session_url, &json!({}), ctx.session.clone(), None)
            .await?;

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let img_url = self.normalize(&format!("/plugin/kcaptcha/kcaptcha_image.php?t={}", ts));

        let (status, bytes) = match ctx.http.get(&img_url, ctx.session.clone(), None).await {
            Ok(res) => (res.status(), res.bytes().await.unwrap_or_default()),
            Err(e) => {
                warn!("获取验证码图片失败: {}", e);
                // 网络错误也尝试切换代理
                ctx.rotate_proxy().await;
                return Box::pin(self.solve_captcha(url, ctx)).await;
            }
        };

        if status == reqwest::StatusCode::FORBIDDEN {
            warn!("获取验证码图片被拒 (403)，正在切换线路重试...");
            ctx.session.clear();
            ctx.rotate_proxy().await;
            tokio::time::sleep(Duration::from_secs(1)).await;

            if let Err(e) = ctx.bypasser.bypass(url, ctx).await {
                warn!("验证码流程中尝试绕过阻断失败: {}", e);
            }

            return Box::pin(self.solve_captcha(url, ctx)).await;
        }

        if !status.is_success() || bytes.is_empty() {
            return Err(SpiderError::SoftBlock(format!(
                "captcha_img_fetch_failed:{}",
                status
            )));
        }

        let code = match booktoki_captcha::solve_captcha(&bytes) {
            Ok(code) => code,
            Err(_) => {
                let _ = self
                    .save_failed_captcha(&bytes, &ctx.config.cache_path)
                    .await;
                return Err(SpiderError::CaptchaFailed);
            }
        };
        info!("验证码识别成功: {}", code);

        let submit_url = self.normalize("/bbs/captcha_check.php");
        let form = vec![
            ("url".to_string(), url.to_string()),
            ("captcha_key".to_string(), code),
        ];

        // 提交时同样绕过中间件
        ctx.http
            .post_form(&submit_url, &form, ctx.session.clone(), None)
            .await?;
        Ok(())
    }

    fn save_failed_html(&self, html: &str) {
        let cache_dir = PathBuf::from("cache").join("html_failed");
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            warn!("无法创建缓存目录: {}", e);
            return;
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let filename = format!("failed_content_{}.html", timestamp);
        let filepath = cache_dir.join(&filename);

        if let Err(e) = std::fs::write(&filepath, html) {
            warn!("无法保存失败的 HTML: {}", e);
        } else {
            info!("已保存失败的 HTML 到: {}", filepath.display());
        }
    }

    async fn save_failed_captcha(&self, img: &[u8], cache_path: &str) -> Result<()> {
        let captcha_dir = PathBuf::from(cache_path).join("captcha");
        fs::create_dir_all(&captcha_dir).await?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let filename = format!("captcha_failed_{}.jpg", timestamp);
        let filepath = captcha_dir.join(&filename);

        fs::write(&filepath, img).await?;
        info!("已保存失败的验证码图片到: {}", filepath.display());
        Ok(())
    }
}

#[async_trait]
impl Site for Booktoki {
    fn id(&self) -> &str {
        "booktoki"
    }

    fn config(&self) -> &SiteConfig {
        &self.config
    }
}
