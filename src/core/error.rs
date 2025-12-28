use thiserror::Error;

#[derive(Error, Debug)]
pub enum SpiderError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Middleware error: {0}")]
    Middleware(#[from] reqwest_middleware::Error),

    #[error("Browser error: {0}")]
    Browser(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Parsing error: {0}")]
    Parse(String),

    /// 软阻断：代表被反爬策略拦截（验证码、Cloudflare等待页等）
    /// 这种错误是可以恢复的，客户端应尝试调用 recover
    #[error("Soft block detected: {0}")]
    SoftBlock(String),

    #[error("Captcha not supported or failed")]
    CaptchaFailed,

    #[error("Other error: {0}")]
    Custom(String),
}

pub type Result<T> = std::result::Result<T, SpiderError>;
