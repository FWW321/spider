//! 配置管理系统 (Configuration Management)
//!
//! 负责 `config.toml` 的反序列化及其层级结构映射，支持环境变量与默认值回退机制。

use std::collections::HashMap;
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

    /// 代理枢纽 (sing-box) 相关配置
    pub singbox: SingboxConfig,

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
#[derive(Debug, Deserialize, Builder, Clone, Default)]
pub struct SingboxConfig {
    /// 二进制执行文件搜索路径
    #[serde(default = "default_bin_path")]
    pub bin_path: String,
    pub proxy_port: u16,
    pub api_port: u16,
    pub api_secret: String,
    pub subscription_urls: Vec<String>,
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
    /// 自定义域名 (用于私有部署或镜像站点)
    pub base_url: Option<String>,
    /// 站点独占任务并行度
    pub concurrent_tasks: Option<usize>,
}

impl Default for SpiderConfig {
    fn default() -> Self {
        Self {
            concurrency: 5,
            retry_count: 3,
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
fn default_bin_path() -> String {
    "bin".to_string()
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
    pub fn load() -> Result<Self> {
        let config_path = Path::new("config.toml");
        let builder = Config::builder();

        let builder = if config_path.exists() {
            builder.add_source(File::from(config_path))
        } else {
            builder
        };

        let settings = builder.build().map_err(SpiderError::Config)?;
        settings.try_deserialize().map_err(SpiderError::Config)
    }
}
