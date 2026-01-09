use std::collections::HashMap;
use std::path::Path;

use bon::Builder;
use config::{Config, File};
use serde::Deserialize;

use crate::core::error::{Result, SpiderError};

#[derive(Debug, Deserialize, Builder, Clone)]
pub struct AppConfig {
    #[serde(default = "default_cache_path")]
    pub cache_path: String,

    pub singbox: SingboxConfig,

    #[serde(default)]
    pub browser: BrowserConfig,

    #[serde(default)]
    pub spider: SpiderConfig,

    #[serde(default)]
    pub sites: HashMap<String, SiteConfig>,
}

#[derive(Debug, Deserialize, Builder, Clone, Default)]
pub struct SingboxConfig {
    #[serde(default = "default_bin_path")]
    pub bin_path: String,
    pub proxy_port: u16,
    pub api_port: u16,
    pub api_secret: String,
    pub subscription_urls: Vec<String>,
}

#[derive(Debug, Deserialize, Builder, Clone)]
pub struct BrowserConfig {
    #[serde(default = "default_headless")]
    pub headless: bool,
    pub chrome_path: Option<String>,
}

#[derive(Debug, Deserialize, Builder, Clone)]
pub struct SpiderConfig {
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
}

#[derive(Debug, Deserialize, Builder, Clone, Default)]
pub struct SiteConfig {
    pub base_url: Option<String>,
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
