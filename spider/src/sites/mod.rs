//! 站点模块
//!
//! 包含所有站点实现和站点注册表

pub mod booktoki;

use std::collections::HashMap;

use crate::core::config::SiteConfig;
use crate::interfaces::Site;
use crate::network::context::ServiceContext;

pub use booktoki::Booktoki;

// =============================================================================
// 站点注册表
// =============================================================================

type SiteFactory = Box<dyn Fn(SiteConfig, ServiceContext) -> Box<dyn Site> + Send + Sync>;

/// 站点注册表
pub struct SiteRegistry {
    factories: HashMap<String, SiteFactory>,
}

impl SiteRegistry {
    /// 创建新的注册表
    pub fn new() -> Self {
        let mut registry = Self {
            factories: HashMap::new(),
        };

        // 注册默认站点
        registry.register("booktoki", |cfg, ctx| Box::new(Booktoki::new(cfg, ctx)));

        registry
    }

    /// 注册站点工厂
    pub fn register<F>(&mut self, id: &str, factory: F)
    where
        F: Fn(SiteConfig, ServiceContext) -> Box<dyn Site> + Send + Sync + 'static,
    {
        self.factories.insert(id.to_string(), Box::new(factory));
    }

    /// 创建站点实例
    pub fn create(
        &self,
        id: &str,
        config: SiteConfig,
        ctx: ServiceContext,
    ) -> Option<Box<dyn Site>> {
        self.factories.get(id).map(|f| f(config, ctx))
    }

    /// 列出所有已注册的站点
    pub fn list(&self) -> Vec<&str> {
        self.factories.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for SiteRegistry {
    fn default() -> Self {
        Self::new()
    }
}
