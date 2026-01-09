//! 错误处理体系 (Error Handling System)
//!
//! 定义领域相关的错误类型、异常阻断原因以及全局 Result 别名。

use thiserror::Error;
use reqwest::StatusCode;

/// 系统阻断原因枚举 (Block Reasons)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// 触发 403 静态拦截
    IpBlocked,
    /// 触发 Cloudflare 挑战
    Cloudflare,
    /// 触发 429 速率限制
    RateLimit,
    /// 授权凭据失效
    TokenExpired,
    /// 站点相关的自定义阻断
    Custom(String),
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockReason::IpBlocked => write!(f, "IpBlocked(403)"),
            BlockReason::Cloudflare => write!(f, "Cloudflare"),
            BlockReason::RateLimit => write!(f, "RateLimit(429)"),
            BlockReason::TokenExpired => write!(f, "TokenExpired"),
            BlockReason::Custom(s) => write!(f, "Custom({})", s),
        }
    }
}

impl From<StatusCode> for BlockReason {
    fn from(code: StatusCode) -> Self {
        match code {
            StatusCode::FORBIDDEN => Self::IpBlocked,
            StatusCode::TOO_MANY_REQUESTS => Self::RateLimit,
            _ => Self::Custom(format!("HTTP {}", code)),
        }
    }
}

/// 全局错误定义 (Spider Domain Errors)
#[derive(Error, Debug)]
pub enum SpiderError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Middleware error: {0}")]
    Middleware(#[from] reqwest_middleware::Error),

    #[error("Browser error: {0}")]
    Browser(String),

    /// 指示需要刷新客户端上下文并重新入队
    #[error("Refresh required: {reason}")]
    RefreshRequired {
        reason: BlockReason,
        new_url: Option<String>,
    },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Parsing error: {0}")]
    Parse(String),

    /// 检测到软阻断，需触发故障恢复逻辑
    #[error("Soft block detected: {0}")]
    SoftBlock(BlockReason),

    #[error("Captcha not supported or failed")]
    CaptchaFailed,

    #[error("Other error: {0}")]
    Custom(String),
}

/// 全局 Result 别名
pub type Result<T> = std::result::Result<T, SpiderError>;

impl SpiderError {
    /// 探测并提取错误中的阻断原因
    /// 
    /// 支持中间件嵌套错误的分层解包 (Downcasting)。
    pub fn is_blocking(&self) -> Option<BlockReason> {
        match self {
            SpiderError::RefreshRequired { reason, .. } => Some(reason.clone()),
            SpiderError::SoftBlock(reason) => Some(reason.clone()),
            SpiderError::Middleware(reqwest_middleware::Error::Middleware(anyhow_err)) => {
                anyhow_err.downcast_ref::<SpiderError>().and_then(|e| e.is_blocking())
            }
            SpiderError::Network(e) => e.status().map(BlockReason::from),
            _ => None,
        }
    }
}
