//! 站点客户端封装 (Site-specific Client)
//!
//! 提供面向具体站点的语义化请求接口，并注入特定拦截策略链。

use crate::core::error::Result;
use crate::interfaces::NetworkPolicy;
use crate::network::context::ServiceContext;
use crate::network::session::Session;
use reqwest::Response;
use std::sync::Arc;

/// 站点特定的逻辑客户端
#[derive(Clone)]
pub struct SiteClient {
    /// 全局服务上下文
    pub ctx: ServiceContext,
    /// 绑定的网络策略集合
    pub policies: Vec<Arc<dyn NetworkPolicy>>,
}

impl SiteClient {
    pub fn new(ctx: ServiceContext, policies: Vec<Arc<dyn NetworkPolicy>>) -> Self {
        Self { ctx, policies }
    }

    /// 执行通用 HTTP GET 请求
    pub async fn get(&self, url: &str) -> Result<Response> {
        self.ctx
            .http
            .execute(&self.ctx, reqwest::Method::GET, url, self.policies.clone())
            .await
    }

    /// 提取响应体文本
    pub async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self.get(url).await?;
        let text = resp
            .text()
            .await
            .map_err(crate::core::error::SpiderError::Network)?;
        Ok(text)
    }

    /// 提取二进制负载
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.get(url).await?;
        let bytes = resp
            .bytes()
            .await
            .map_err(crate::core::error::SpiderError::Network)?;
        Ok(bytes.to_vec())
    }

    /// 获取当前会话状态句柄
    pub fn session(&self) -> &Session {
        &self.ctx.session
    }
}
