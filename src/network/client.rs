use std::sync::Arc;
use reqwest::Response;
use crate::core::error::Result;
use crate::interfaces::NetworkPolicy;
use crate::network::context::ServiceContext;
use crate::network::session::Session;

/// 面向站点的 HTTP 客户端封装
#[derive(Clone)]
pub struct SiteClient {
    pub ctx: ServiceContext,
    pub policies: Vec<Arc<dyn NetworkPolicy>>,
}

impl SiteClient {
    pub fn new(ctx: ServiceContext, policies: Vec<Arc<dyn NetworkPolicy>>) -> Self {
        Self { ctx, policies }
    }

    /// 执行通用 GET 请求
    pub async fn get(&self, url: &str) -> Result<Response> {
        self.ctx.http.execute(
            &self.ctx,
            reqwest::Method::GET,
            url,
            self.policies.clone()
        ).await
    }

    /// 获取文本内容
    pub async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self.get(url).await?;
        let text = resp.text().await.map_err(crate::core::error::SpiderError::Network)?;
        Ok(text)
    }

    /// 获取二进制内容
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.get(url).await?;
        let bytes = resp.bytes().await.map_err(crate::core::error::SpiderError::Network)?;
        Ok(bytes.to_vec())
    }

    pub fn session(&self) -> &Session {
        &self.ctx.session
    }
}
