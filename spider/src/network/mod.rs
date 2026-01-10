pub mod browser;
pub mod client;
pub mod context;
pub mod middleware;
pub mod policies;
pub mod service;
pub mod session;

use reqwest::Response;

use reqwest::header::HeaderMap;

/// URL 扩展类型，用于在 Response 重建时保留 URL

#[derive(Clone, Debug)]

pub struct OriginalUrl(pub String);

/// 响应元数据，用于在 Response 被消费后重建
pub struct ResponseMetadata {
    pub status: reqwest::StatusCode,
    pub version: reqwest::Version,
    pub headers: HeaderMap,
    pub url: String,
    pub remote_addr: Option<std::net::SocketAddr>,
}

impl ResponseMetadata {
    pub fn from_response(resp: &Response) -> Self {
        Self {
            status: resp.status(),
            version: resp.version(),
            headers: resp.headers().clone(),
            url: resp.original_url().to_string(),
            remote_addr: resp.remote_addr(),
        }
    }

    pub fn rebuild(self, body: bytes::Bytes) -> Response {
        let http_resp = http::Response::builder()
            .status(self.status)
            .version(self.version)
            .body(body)
            .unwrap();

        let mut new_resp = Response::from(http_resp);

        // 复制所有 Headers
        *new_resp.headers_mut() = self.headers;

        // 保留原始 URL 扩展
        new_resp.extensions_mut().insert(OriginalUrl(self.url));

        // 保留远程地址信息
        if let Some(addr) = self.remote_addr {
            new_resp.extensions_mut().insert(addr);
        }

        new_resp
    }
}

/// 响应扩展 Trait
pub trait ResponseExt {
    /// 获取原始 URL (优先从扩展中读取，否则从 Response 自带的 URL 读取)
    fn original_url(&self) -> &str;
}

impl ResponseExt for Response {
    fn original_url(&self) -> &str {
        self.extensions()
            .get::<OriginalUrl>()
            .map(|u| u.0.as_str())
            .unwrap_or_else(|| self.url().as_str())
    }
}
