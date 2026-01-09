//! 浏览器服务
//!
//! 封装浏览器相关操作，包括 Cloudflare 绕过等

use std::time::Duration;
use std::path::Path;

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

static STEALTH_JS: &str = include_str!("../../stealth.min.js");
static UA_CACHE: OnceCell<String> = OnceCell::const_new();

/// 浏览器会话
/// 采用显式的所有权管理，确保关闭逻辑的确定性
pub struct BrowserSession {
    browser: Option<Browser>,
    handler: Option<JoinHandle<()>>,
}

impl BrowserSession {
    /// 启动浏览器会话
    pub async fn launch(config: &AppConfig) -> Result<Self> {
        let ua = UA_CACHE.get_or_init(Self::probe_native_ua).await;
        let browser_config = Self::build_config(config, ua)?;

        let (browser, mut handler) = Browser::launch(browser_config)
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;

        // 启动事件循环
        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            browser: Some(browser),
            handler: Some(handle),
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
                .find(|p| Path::new(p).exists())
                .map(|p| p.to_string())
        };

        if let Some(path) = chrome_path {
            builder = builder.chrome_executable(path);
        }

        builder.build().map_err(SpiderError::Browser)
    }

    /// 创建新页面
    pub async fn new_page(&self) -> Result<Page> {
        let browser = self.browser.as_ref().ok_or_else(|| SpiderError::Browser("Browser already closed".into()))?;
        let page = browser
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

    /// 优雅关闭浏览器，并等待事件循环结束
    pub async fn close(&mut self) -> Result<()> {
        let browser = self.browser.take();
        let handler = self.handler.take();

        if let Some(mut b) = browser {
            let _ = b.close().await;
            if let Some(h) = handler {
                let _ = h.await;
            }
        }
        Ok(())
    }

    async fn probe_native_ua() -> String {
        debug!("正在探测系统原生 User-Agent...");

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

        // 执行探测逻辑
        let result = async {
            let page = browser.new_page("about:blank").await?;
            let ua: String = page.evaluate("navigator.userAgent").await?.into_value()?;
            Ok::<String, chromiumoxide::error::CdpError>(ua)
        }
        .await;

        // 显式保证关闭顺序：先 close，再 await handler，最后再让 browser 离开作用域
        let _ = browser.close().await;
        let _ = handle.await;
        sleep(Duration::from_millis(100)).await; // 额外等待以确保 OS 释放资源
        drop(browser);

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
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".to_string()
    }
}

// 在 Drop 时尝试最后一次保护，但不报 WARN
impl Drop for BrowserSession {
    fn drop(&mut self) {
        if self.browser.is_some() {
            let mut browser = self.browser.take().unwrap();
            let handler = self.handler.take();
            // 在后台悄悄清理
            tokio::spawn(async move {
                let _ = browser.close().await;
                if let Some(h) = handler {
                    let _ = h.await;
                }
            });
        }
    }
}

// =============================================================================
// BrowserService
// =============================================================================

pub struct BrowserService {
    config: std::sync::Arc<AppConfig>,
}

impl BrowserService {
    pub fn new(config: std::sync::Arc<AppConfig>) -> Self {
        Self { config }
    }

    pub async fn bypass(
        &self,
        url: &str,
        ctx: &crate::network::context::ServiceContext,
    ) -> Result<()> {
        let mut last_error = None;
        let max_attempts = self.config.spider.retry_count;

        for attempt in 1..=max_attempts {
            info!("正在尝试绕过验证 ({}/{})...", attempt, max_attempts);

            match self.try_single_attempt(url, ctx).await {
                Ok(_) => {
                    info!("验证通过");
                    return Ok(());
                }
                Err(e) => {
                    warn!("尝试失败: {}", e);
                    last_error = Some(e);
                    ctx.force_rotate_proxy().await;
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SpiderError::Browser(format!("验证在 {} 次尝试后失败", max_attempts))
        }))
    }

    async fn try_single_attempt(
        &self,
        url: &str,
        ctx: &crate::network::context::ServiceContext,
    ) -> Result<()> {
        let mut session = BrowserSession::launch(&self.config).await?;
        
        // 使用 scope 确保即使 execute_page_logic 出错也能走到 session.close()
        let result = self.execute_page_logic(&session, url, ctx).await;

        // 显式关闭并等待资源释放
        if let Err(e) = session.close().await {
            debug!("关闭浏览器时发生非致命错误: {}", e);
        }

        result
    }

    async fn execute_page_logic(
        &self,
        session: &BrowserSession,
        url: &str,
        ctx: &crate::network::context::ServiceContext,
    ) -> Result<()> {
        let page = session.new_page().await?;

        page.goto(url)
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;

        self.wait_for_challenge(&page).await?;
        self.extract_and_save_data(&page, ctx).await?;

        Ok(())
    }

    async fn wait_for_challenge(&self, page: &Page) -> Result<()> {
        timeout(Duration::from_secs(25), async {
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

                // 检查标题是否跳过了 Cloudflare 等待页
                let title_ok = !title.is_empty()
                    && !title.contains("just a moment")
                    && !title.contains("cloudflare");

                if title_ok {
                    let cookies = page.get_cookies().await.unwrap_or_default();
                    if cookies.iter().any(|c| c.name == "cf_clearance") {
                         debug!("页面验证通过且获取到 cf_clearance (标题: {})", title);
                         return Ok(());
                    }
                    // 如果没有 clearance，即使标题对了也继续等（或者直到超时）
                    debug!("页面已跳转但缺少 cf_clearance，继续等待...");
                }

                sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .map_err(|_| SpiderError::Browser("Cloudflare 绕过超时".into()))?
    }

    async fn extract_and_save_data(
        &self,
        page: &Page,
        ctx: &crate::network::context::ServiceContext,
    ) -> Result<()> {
        let cookies = page
            .get_cookies()
            .await
            .map_err(|e| SpiderError::Browser(e.to_string()))?;

        if !cookies.is_empty() {
            let cookie_str = cookies
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; ");
            ctx.session.set_cookie(cookie_str);
        }

        if let Ok(ua_val) = page.evaluate("navigator.userAgent").await
            && let Ok(ua) = ua_val.into_value::<String>()
                && !ua.is_empty() {
                    ctx.session.set_ua(ua);
                }

        // 设置基础 Header
        let mut headers = HeaderMap::new();
        headers.insert("Upgrade-Insecure-Requests", HeaderValue::from_static("1"));
        headers.insert(
            "Accept",
            HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8"),
        );
        headers.insert("Cache-Control", HeaderValue::from_static("max-age=0"));

        ctx.session.set_headers(headers);
        Ok(())
    }
}