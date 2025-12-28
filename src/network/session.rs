use std::sync::Arc;

use parking_lot::RwLock;
use reqwest::header::HeaderMap;

#[derive(Debug, Default)]
pub struct Session {
    pub ua: Arc<RwLock<String>>,
    pub cookie: Arc<RwLock<Option<String>>>,
    pub extra_headers: Arc<RwLock<HeaderMap>>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_ua(&self, ua: String) {
        *self.ua.write() = ua;
    }

    pub fn set_cookie(&self, cookie: String) {
        *self.cookie.write() = Some(cookie);
    }

    pub fn set_headers(&self, headers: HeaderMap) {
        *self.extra_headers.write() = headers;
    }

    pub fn is_empty(&self) -> bool {
        self.ua.read().is_empty() && self.cookie.read().is_none()
    }

    /// 清空所有 Session 数据
    pub fn clear(&self) {
        self.ua.write().clear();
        *self.cookie.write() = None;
        self.extra_headers.write().clear();
    }
}
