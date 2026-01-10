//! 配置管理系统 (Configuration Management)
//!
//! 负责 `config.toml` 的反序列化及其层级结构映射，支持环境变量与默认值回退机制。
//! 如果配置文件不存在，会自动生成默认配置文件。

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use bon::Builder;
use config::{Config, File};
use serde::Deserialize;

use crate::core::error::{Result, SpiderError};

/// 全局应用配置
#[derive(Debug, Deserialize, Builder, Clone)]
pub struct AppConfig {
    /// 缓存与持久化目录基准路径
    #[serde(default = "default_cache_path")]
    pub cache_path: String,

    /// 代理 (Proxy) 相关配置
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// 自动化浏览器 (Chromium) 相关配置
    #[serde(default)]
    pub browser: BrowserConfig,

    /// 爬虫调度引擎通用参数
    #[serde(default)]
    pub spider: SpiderConfig,

    /// 站点特定配置覆盖映射
    #[serde(default)]
    pub sites: HashMap<String, SiteConfig>,
}

/// 代理网络配置
#[derive(Debug, Deserialize, Builder, Clone)]
pub struct ProxyConfig {
    /// 是否启用代理（false 时使用直连模式）
    #[serde(default = "default_proxy_enabled")]
    pub enabled: bool,
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,
    #[serde(default)]
    pub subscription_urls: Vec<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            proxy_port: 2080,
            subscription_urls: vec![],
        }
    }
}

fn default_proxy_enabled() -> bool {
    true
}

fn default_proxy_port() -> u16 {
    2080
}

/// 浏览器引擎配置
#[derive(Debug, Deserialize, Builder, Clone)]
pub struct BrowserConfig {
    /// 是否以无头模式 (Headless) 运行
    #[serde(default = "default_headless")]
    pub headless: bool,
    /// 自定义可执行文件路径
    pub chrome_path: Option<String>,
}

/// 调度引擎参数
#[derive(Debug, Deserialize, Builder, Clone)]
pub struct SpiderConfig {
    /// 全局任务并行度上限
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// 任务重试阈值
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
}

/// 站点特定配置覆盖
#[derive(Debug, Deserialize, Builder, Clone, Default)]
pub struct SiteConfig {
    /// 自定义域名
    pub base_url: Option<String>,
    /// 站点独占任务并行度
    pub concurrent_tasks: Option<usize>,
    /// 是否强制使用代理（true 时禁用代理则拒绝执行）
    #[serde(default)]
    pub require_proxy: bool,
}

impl Default for SpiderConfig {
    fn default() -> Self {
        Self {
            concurrency: default_concurrency(),
            retry_count: default_retry_count(),
        }
    }
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            headless: true,
            chrome_path: None,
        }
    }
}

fn default_cache_path() -> String {
    "cache".to_string()
}
fn default_headless() -> bool {
    true
}
fn default_concurrency() -> usize {
    32
}
fn default_retry_count() -> u32 {
    3
}

impl AppConfig {
    /// 从文件系统中加载并解析配置
    ///
    /// 如果配置文件不存在，会自动生成默认配置文件。
    pub fn load() -> Result<Self> {
        let config_path = Path::new("config.toml");

        // 如果配置文件不存在，生成默认配置
        if !config_path.exists() {
            Self::create_default_config(config_path)?;
        }

        // 加载配置文件
        let settings = Config::builder()
            .add_source(File::from(config_path))
            .build()
            .map_err(SpiderError::Config)?;

        settings.try_deserialize().map_err(SpiderError::Config)
    }

    /// 创建默认配置文件
    fn create_default_config(path: &Path) -> Result<()> {
        const DEFAULT_CONFIG: &str = r#"# 配置文件
# 首次运行自动生成，可根据需要修改

# 基础路径配置
cache_path = "cache"

[proxy]
enabled = true                 # 是否启用代理（false = 直连模式）
proxy_port = 2080              # 本地 HTTP 代理监听端口
# 代理订阅地址列表（为空时自动使用直连模式）
subscription_urls = [
    # "https://example.com/subscribe?token=xxx",
]

[browser]
headless = true               # 是否开启无头模式 (true: 不显示浏览器窗口)
# chrome_path = "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"

[spider]
concurrency = 32               # 全局最大并发采集任务数
retry_count = 3                # 请求失败时的最大重试次数

# 站点特定配置示例
# [sites.booktoki]
# base_url = "https://booktoki469.com"
# concurrent_tasks = 32
# require_proxy = false  # 是否强制使用代理 (默认: false)
"#;

        fs::write(path, DEFAULT_CONFIG)
            .map_err(SpiderError::Io)?;

        println!("Created default configuration file: {}", path.display());
        println!("Edit config.toml to customize your settings.");
        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            cache_path: default_cache_path(),
            proxy: ProxyConfig::default(),
            browser: BrowserConfig::default(),
            spider: SpiderConfig::default(),
            sites: HashMap::new(),
        }
    }
}
