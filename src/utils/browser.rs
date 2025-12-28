use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::{
    Page,
    browser::{Browser, BrowserConfig},
    cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams,
};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use tokio::{
    sync::OnceCell,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::{debug, info, warn};

use crate::core::config::AppConfig;
use crate::core::error::{Result, SpiderError};
use crate::sites::{Bypasser, SiteContext};

static STEALTH_JS: &str = include_str!("../../stealth.min.js");

static UA_CACHE: OnceCell<String> = OnceCell::const_new();

pub struct BrowserSession {
    browser: Browser,
    _handler: JoinHandle<()>,
}

impl BrowserSession {
    pub async fn launch(config: &AppConfig) -> Result<Self> {
        let ua = UA_CACHE.get_or_init(Self::probe_native_ua).await;

        let browser_config = Self::build_config(config, ua)?;

        let (browser, mut handler) = Browser::launch(browser_config)
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            browser,
            _handler: handle,
        })
    }

    fn build_config(config: &AppConfig, ua: &str) -> Result<BrowserConfig> {
        let mut builder = BrowserConfig::builder()
            .arg("--disable-blink-features=AutomationControlled")
            .arg(format!("--user-agent={}", ua))
            .arg("--disable-infobars")
            .arg("--no-sandbox")
            .arg("--window-size=1920,1080")
            .arg("--disable-extensions")
            .arg("--host-resolver-rules=MAP * ~NOTFOUND,EXCLUDE 127.0.0.1")
            .arg(format!(
                "--proxy-server=http://127.0.0.1:{}",
                config.singbox.proxy_port
            ));

        if config.browser.headless {
            builder = builder.arg("--headless=new");
        } else {
            builder = builder.with_head();
        }

        let chrome_path = if let Some(path) = &config.browser.chrome_path {
            Some(path.clone())
        } else {
            let default_paths = [
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            ];
            default_paths
                .iter()
                .find(|p| std::path::Path::new(p).exists())
                .map(|p| p.to_string())
        };

        if let Some(path) = chrome_path {
            builder = builder.chrome_executable(path);
        }

        builder.build().map_err(|e| SpiderError::Browser(e))
    }

    pub async fn new_page(&self) -> Result<Page> {
        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;

        if let Err(e) = page
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(
                STEALTH_JS.to_string(),
            ))
            .await
        {
            debug!("Stealth injection warning: {}", e);
        }

        Ok(page)
    }

    pub async fn close(mut self) -> Result<()> {
        self.browser
            .close()
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;
        Ok(())
    }

    async fn probe_native_ua() -> String {
        debug!("正在探测 User-Agent...");

        let config = match BrowserConfig::builder()
            .arg("--headless=new")
            .arg("--no-sandbox")
            .build()
        {
            Ok(c) => c,
            Err(_) => return Self::fallback_ua(),
        };

        let (mut browser, mut handler) = match Browser::launch(config).await {
            Ok(b) => b,
            Err(_) => return Self::fallback_ua(),
        };

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        let result = async {
            let page = browser.new_page("about:blank").await?;
            let ua: String = page.evaluate("navigator.userAgent").await?.into_value()?;
            Ok::<String, chromiumoxide::error::CdpError>(ua)
        }
        .await;

        let _ = browser.close().await;
        let _ = handle.await;

        match result {
            Ok(ua) => {
                let clean_ua = ua
                    .replace("HeadlessChrome", "Chrome")
                    .replace("Headless", "");
                debug!("UA 探测成功: {}", clean_ua);
                clean_ua
            }
            Err(_) => Self::fallback_ua(),
        }
    }

    fn fallback_ua() -> String {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36".to_string()
    }
}

// -----------------------------------------------------------------------------
// CloudflareBypasser
// -----------------------------------------------------------------------------

pub struct CloudflareBypasser;

#[async_trait]
impl Bypasser for CloudflareBypasser {
    async fn bypass(&self, url: &str, ctx: &SiteContext) -> Result<()> {
        let mut last_error = None;
        let max_attempts = ctx.config.spider.retry_count;

        for attempt in 1..=max_attempts {
            info!("正在绕过验证...");
            debug!("尝试次数: {}/{}, URL: {}", attempt, max_attempts, url);

            match self.try_single_attempt(url, ctx).await {
                Ok(_) => {
                    info!("验证通过");
                    return Ok(());
                }
                Err(e) => {
                    warn!("尝试失败，正在重试 ({}/{})", attempt, max_attempts);
                    debug!("错误详情: {}", e);
                    last_error = Some(e);

                    ctx.rotate_proxy().await;
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SpiderError::Browser(format!("验证在 {} 次尝试后失败", max_attempts))
        }))
    }
}

impl CloudflareBypasser {
    async fn try_single_attempt(&self, url: &str, ctx: &SiteContext) -> Result<()> {
        let session = BrowserSession::launch(&ctx.config).await?;
        let result = self.execute_page_logic(&session, url, ctx).await;

        if let Err(e) = session.close().await {
            debug!("关闭浏览器会话失败: {}", e);
        }

        result
    }

    async fn execute_page_logic(
        &self,
        session: &BrowserSession,
        url: &str,
        ctx: &SiteContext,
    ) -> Result<()> {
        let page = session.new_page().await?;

        debug!("浏览器访问 URL: {}", url);
        page.goto(url)
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;

        self.wait_for_challenge(&page).await?;
        self.extract_and_save_data(&page, ctx).await?;

        Ok(())
    }

    async fn wait_for_challenge(&self, page: &Page) -> Result<()> {
        timeout(Duration::from_secs(20), async {
            loop {
                let title = page
                    .get_title()
                    .await
                    .unwrap_or(Some("".into()))
                    .unwrap_or_default()
                    .to_lowercase();

                if title.contains("403 forbidden") || title.contains("access denied") {
                    return Err(SpiderError::SoftBlock("ip_blocked_in_browser".into()));
                }

                if !title.is_empty()
                    && !title.contains("just a moment")
                    && !title.contains("cloudflare")
                {
                    debug!("验证通过，页面标题: {}", title);
                    return Ok(());
                }

                debug!("等待 Cloudflare 挑战响应... (当前标题: {})", title);
                sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .map_err(|_| SpiderError::Browser("Cloudflare 绕过超时".into()))?
    }

    async fn extract_and_save_data(&self, page: &Page, ctx: &SiteContext) -> Result<()> {
        let cookies = page
            .get_cookies()
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;

        if cookies.is_empty() {
            warn!("未提取到任何 Cookie");
        } else {
            let cookie_str = cookies
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; ");

            debug!("成功提取 {} 个 Cookie", cookies.len());
            ctx.session.set_cookie(cookie_str);
        }

        if let Ok(ua_val) = page.evaluate("navigator.userAgent").await {
            if let Ok(ua) = ua_val.into_value::<String>() {
                if !ua.is_empty() {
                    ctx.session.set_ua(ua);
                }
            }
        }

        let mut headers = HeaderMap::new();
        headers.insert("Upgrade-Insecure-Requests", HeaderValue::from_static("1"));
        headers.insert(
            "Accept", 
            HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7")
        );
        headers.insert("Cache-Control", HeaderValue::from_static("max-age=0"));

        ctx.session.set_headers(headers);

        Ok(())
    }
}
