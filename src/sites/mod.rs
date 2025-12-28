use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use flume::Sender;
use tracing::debug;

use crate::actors::proxy::ProxyMsg;
use crate::core::config::{AppConfig, SiteConfig};
use crate::core::error::Result;
use crate::core::model::{BookItem, Metadata};
use crate::network::service::HttpService;
use crate::network::session::Session;

pub mod booktoki;

/// 任务参数
pub type TaskArgs = HashMap<String, String>;

#[async_trait]
pub trait Bypasser: Send + Sync {
    async fn bypass(&self, url: &str, ctx: &SiteContext) -> Result<()>;
}

/// 站点执行上下文
#[derive(Clone)]
pub struct SiteContext {
    pub config: Arc<AppConfig>,
    pub proxy: Sender<ProxyMsg>,
    pub http: Arc<HttpService>,
    pub session: Arc<Session>,
    pub bypasser: Arc<dyn Bypasser>,
}

impl SiteContext {
    pub async fn get(&self, url: &str) -> Result<String> {
        self.http.get_text(url, self.session.clone(), None).await
    }

    pub async fn probe(&self, url: &str) -> Result<(u16, String)> {
        self.http.probe(url, self.session.clone(), None).await
    }

    pub async fn rotate_proxy(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .proxy
            .send(ProxyMsg::Rotate { reply: Some(tx) })
            .is_ok()
        {
            let _ = rx.await;
            debug!("代理节点已完成物理切换");
        }
    }
}

/// URL 构建器
pub trait UrlBuilder {
    fn base_url(&self) -> &str;
    fn build_url(&self, kind: &str, args: &TaskArgs) -> Result<String>;
}

/// HTML 解析器
pub trait Parser {
    fn parse_metadata(&self, html: &str) -> Result<(Metadata, Option<TaskArgs>)>;
    fn parse_chapters(&self, html: &str) -> Result<(Vec<BookItem>, Option<String>)>;
    fn parse_content(&self, html: &str) -> Result<(String, Option<String>)>;
}

/// 阻断检测与恢复
#[async_trait]
pub trait Guardian: Send + Sync {
    fn check_block(&self, url: &str, html: &str, status: u16) -> Result<()>;
    async fn recover(&self, url: &str, reason: &str, ctx: &SiteContext) -> Result<bool>;
}

/// 站点定义
#[async_trait]
pub trait Site: UrlBuilder + Parser + Guardian + Send + Sync {
    fn id(&self) -> &str;
    fn config(&self) -> &SiteConfig;
}

// ============================================================================
// 供中间件使用的上下文
// ============================================================================

pub struct MiddlewareContext {
    pub site: Arc<dyn Site>,
    pub ctx: SiteContext,
}

// ============================================================================
// 站点注册表
// ============================================================================

type SiteFactory = Box<dyn Fn(SiteConfig) -> Box<dyn Site> + Send + Sync>;

pub struct SiteRegistry {
    factories: HashMap<String, SiteFactory>,
}

impl SiteRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            factories: HashMap::new(),
        };
        registry.register("booktoki", |cfg| Box::new(booktoki::Booktoki::new(cfg)));
        registry
    }

    pub fn register<F>(&mut self, id: &str, factory: F)
    where
        F: Fn(SiteConfig) -> Box<dyn Site> + Send + Sync + 'static,
    {
        self.factories.insert(id.to_string(), Box::new(factory));
    }

    pub fn create(&self, id: &str, config: SiteConfig) -> Option<Box<dyn Site>> {
        self.factories.get(id).map(|f| f(config))
    }

    pub fn list(&self) -> Vec<&str> {
        self.factories.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for SiteRegistry {
    fn default() -> Self {
        Self::new()
    }
}
