//! 会话状态管理 (Session Management)
//! 
//! 利用内部可变性 (Interior Mutability) 维护跨线程同步的会话凭据、指纹特征及自定义头部。

use reqwest::header::HeaderMap;
use std::sync::RwLock;

/// 网络会话容器
/// 
/// 存储 Cookie、User-Agent 等动态状态，确保 HTTP 客户端与自动化浏览器之间的身份一致性。
pub struct Session {
    /// 浏览器指纹 (User-Agent)
    ua: RwLock<String>,
    /// 身份凭据 (Cookie)
    cookie: RwLock<Option<String>>,
    /// 动态注入的自定义头部集
    headers: RwLock<HeaderMap>,
}

impl Session {
    /// 初始化默认会话环境
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

    /// 从响应头中同步状态
    pub fn update_from_headers(&self, headers: &HeaderMap) {
        if let Some(ua) = headers.get(reqwest::header::USER_AGENT)
            && let Ok(ua_str) = ua.to_str()
        {
            self.set_ua(ua_str.to_string());
        }
    }

    /// 检查凭据是否有效
    pub fn is_empty(&self) -> bool {
        self.cookie.read().unwrap().is_none()
    }

    /// 重置会话状态
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
