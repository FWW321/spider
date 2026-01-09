use std::sync::RwLock;
use reqwest::header::HeaderMap;

/// 用户会话状态
/// 存储 Cookie、User-Agent 等动态信息，用于跨组件同步
pub struct Session {
    ua: RwLock<String>,
    cookie: RwLock<Option<String>>,
    headers: RwLock<HeaderMap>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            ua: RwLock::new("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".to_string()),
            cookie: RwLock::new(None),
            headers: RwLock::new(HeaderMap::new()),
        }
    }

    pub fn set_ua(&self, ua: String) {
        let mut guard = self.ua.write().unwrap();
        *guard = ua;
    }

    pub fn get_ua(&self) -> String {
        self.ua.read().unwrap().clone()
    }

    pub fn set_cookie(&self, cookie: String) {
        let mut guard = self.cookie.write().unwrap();
        *guard = Some(cookie);
    }

    pub fn get_cookie(&self) -> Option<String> {
        self.cookie.read().unwrap().clone()
    }

    pub fn set_headers(&self, headers: HeaderMap) {
        let mut guard = self.headers.write().unwrap();
        *guard = headers;
    }

    pub fn get_headers(&self) -> HeaderMap {
        self.headers.read().unwrap().clone()
    }

    pub fn update_from_headers(&self, headers: &HeaderMap) {
        if let Some(ua) = headers.get(reqwest::header::USER_AGENT)
            && let Ok(ua_str) = ua.to_str() {
                self.set_ua(ua_str.to_string());
            }
    }

    pub fn is_empty(&self) -> bool {
        self.cookie.read().unwrap().is_none()
    }

    pub fn clear(&self) {
        let mut cookie = self.cookie.write().unwrap();
        *cookie = None;
        let mut headers = self.headers.write().unwrap();
        headers.clear();
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}