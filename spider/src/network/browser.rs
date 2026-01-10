//! 浏览器自动化服务 (Browser Automation Service)
//!
//! 封装基于 CDP 的浏览器操作，主要用于解决 Cloudflare 挑战及指纹特征提取。

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

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
    time::{interval, timeout},
};
use tracing::{debug, info};

use crate::core::config::AppConfig;
use crate::core::error::{BlockReason, Result, SpiderError};

/// 隐身补丁脚本
static STEALTH_JS: &str = include_str!("../../stealth.min.js");
/// 指纹缓存
static UA_CACHE: OnceCell<String> = OnceCell::const_new();

/// 浏览器端标准请求头集
static DEFAULT_BROWSER_HEADERS: OnceLock<HeaderMap> = OnceLock::new();

fn get_default_headers() -> &'static HeaderMap {
    DEFAULT_BROWSER_HEADERS.get_or_init(|| {
        let mut headers = HeaderMap::new();
        headers.insert("Upgrade-Insecure-Requests", HeaderValue::from_static("1"));
        headers.insert(
            "Accept",
            HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8"),
        );
        headers.insert("Cache-Control", HeaderValue::from_static("max-age=0"));
        headers
    })
}

/// 浏览器会话容器 (Browser Session)
/// 
/// 负责管理 Chromium 进程的生命周期及 CDP 事件循环。
pub struct BrowserSession {
    /// 浏览器实例
    browser: Option<Browser>,
    /// 事件循环句柄
    handler: Option<JoinHandle<()>>,
}

impl BrowserSession {
    /// 启动浏览器实例并初始化事件循环
    pub async fn launch(config: &AppConfig) -> Result<Self> {
        let ua = UA_CACHE.get_or_init(Self::probe_native_ua).await;
        let browser_config = build_browser_config(config, ua)?;

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
            browser: Some(browser),
            handler: Some(handle),
        })
    }

    /// 创建预置 Stealth 脚本的新页面
    pub async fn new_page(&self) -> Result<Page> {
        let browser = self
            .browser
            .as_ref()
            .ok_or_else(|| SpiderError::Browser("Browser session already closed".into()))?;
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
            debug!("Stealth script injection failed: {}", e);
        }

        Ok(page)
    }

    /// 优雅关闭浏览器会话并回收资源
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

    /// 原生 User-Agent 探测 (Fingerprint Probing)
    async fn probe_native_ua() -> String {
        debug!("Probing native system User-Agent...");

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
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(browser);

        match result {
            Ok(ua) => {
                let clean_ua = ua
                    .replace("HeadlessChrome", "Chrome")
                    .replace("Headless", "");
                debug!("Native UA probed: {}", clean_ua);
                clean_ua
            }
            Err(_) => Self::fallback_ua(),
        }
    }

    fn fallback_ua() -> String {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".to_string()
    }
}

/// 构建 Chromium 启动参数集
fn build_browser_config(config: &AppConfig, ua: &str) -> Result<BrowserConfig> {
    let mut builder = BrowserConfig::builder()
        .arg("--disable-blink-features=AutomationControlled")
        .arg(format!("--user-agent={}", ua))
        .arg("--disable-infobars")
        .arg("--no-sandbox")
        .arg("--window-size=1920,1080")
        .arg("--disable-extensions")
        .arg(format!(
            "--proxy-server=http://127.0.0.1:{}",
            config.proxy.proxy_port
        ));

    if config.browser.headless {
        builder = builder.arg("--headless=new");
    } else {
        builder = builder.with_head();
    }

    let chrome_path = if let Some(path) = &config.browser.chrome_path {
        Some(path.clone())
    } else {
        [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ]
        .iter()
        .find(|p| Path::new(p).exists())
        .map(|p| p.to_string())
    };

    if let Some(path) = chrome_path {
        builder = builder.chrome_executable(path);
    }

    builder.build().map_err(SpiderError::Browser)
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        if self.browser.is_some() {
            let mut browser = self.browser.take().unwrap();
            let handler = self.handler.take();
            tokio::spawn(async move {
                let _ = browser.close().await;
                if let Some(h) = handler {
                    let _ = h.await;
                }
            });
        }
    }
}

/// 自动化挑战解决服务 (Challenge Resolution Service)
pub struct BrowserService {
    config: std::sync::Arc<AppConfig>,
}

impl BrowserService {
    pub fn new(config: std::sync::Arc<AppConfig>) -> Self {
        Self { config }
    }

    /// 执行挑战绕过流程
    /// 
    /// 集成重试机制、代理轮换及状态上报。
    pub async fn bypass(
        &self,
        url: &str,
        ctx: &crate::network::context::ServiceContext,
    ) -> Result<()> {
        let mut last_error = None;
        let max_attempts = self.config.spider.retry_count;

        for attempt in 1..=max_attempts {
            info!("Bypassing verification ({}/{})...", attempt, max_attempts);

            match self.try_single_attempt(url, ctx).await {
                Ok(_) => {
                    info!("Verification passed");
                    return Ok(());
                }
                Err(e) => {
                    info!("Verification attempt failed, rotating proxy: {}", e);
                    last_error = Some(e);
                    ctx.force_rotate_proxy().await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SpiderError::Browser(format!("Bypass failed after {} attempts", max_attempts))
        }))
    }

    /// 执行单次绕过尝试
    async fn try_single_attempt(
        &self,
        url: &str,
        ctx: &crate::network::context::ServiceContext,
    ) -> Result<()> {
        let mut session = BrowserSession::launch(&self.config).await?;
        let result = self.execute_page_logic(&session, url, ctx).await;

        if let Err(e) = session.close().await {
            debug!("Non-fatal error during browser closure: {}", e);
        }

        result
    }

    /// 页面交互逻辑
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

    /// 状态监测与挑战确认 (Challenge Detection)
    async fn wait_for_challenge(&self, page: &Page) -> Result<()> {
        timeout(Duration::from_secs(25), async {
            let mut ticker = interval(Duration::from_secs(1));

            loop {
                ticker.tick().await;

                let title = page
                    .get_title()
                    .await
                    .unwrap_or_default()
                    .unwrap_or_default()
                    .to_lowercase();

                // 致命拦截判断 (Fatal Block)
                if title.contains("403 forbidden") || title.contains("access denied") {
                    return Err(SpiderError::SoftBlock(BlockReason::IpBlocked));
                }

                // 挑战页检测
                if title.is_empty()
                    || title.contains("just a moment")
                    || title.contains("cloudflare")
                {
                    debug!("Waiting for Cloudflare redirect (Title: {})...", title);
                    continue;
                }

                // 持久凭据校验 (Credential Validation)
                let cookies = page.get_cookies().await.unwrap_or_default();
                if cookies.iter().any(|c| c.name == "cf_clearance") {
                    debug!("Challenge solved, cf_clearance acquired (Title: {})", title);
                    return Ok(());
                }

                debug!("Page transitioned but cf_clearance missing, still waiting...");
            }
        })
        .await
        .map_err(|_| SpiderError::Browser("Challenge bypass timeout".into()))?
    }

    /// 数据提取与会话状态同步
    async fn extract_and_save_data(
        &self,
        page: &Page,
        ctx: &crate::network::context::ServiceContext,
    ) -> Result<()> {
        // 同步 Cookie 凭据
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

        // 同步 User-Agent
        if let Ok(ua_val) = page.evaluate("navigator.userAgent").await {
            if let Ok(ua) = ua_val.into_value::<String>() {
                if !ua.is_empty() {
                    ctx.session.set_ua(ua);
                }
            }
        }

        // 注入默认浏览器上下文请求头
        ctx.session.set_headers(get_default_headers().clone());

        Ok(())
    }
}
