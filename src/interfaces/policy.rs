use async_trait::async_trait;
use reqwest::Response;

use crate::core::error::{Result, SpiderError};
use crate::network::context::ServiceContext;

/// 策略判定结果
#[derive(Debug)]
pub enum PolicyResult {
    /// 检查通过，将 Response 传递给下一个策略或 Parser
    Pass(Response),
    /// 拦截并重试。
    /// is_force=true 意味着跳过全局阻塞等待（用于环境修复后的首个验证请求）
    Retry { is_force: bool, reason: String },
    /// 目标 URL 已变更，请跳转到新 URL 重试
    Redirect(String),
    /// 发生不可恢复的错误，终止任务
    Fail(SpiderError),
}

/// 网络策略接口
///
/// - 策略负责：识别异常状态、利用 Coordinator 抢占修复权、更新环境（Cookie/IP/Session）。
/// - 策略不负责：不负责发起原始业务请求的重试逻辑。
#[async_trait]
pub trait NetworkPolicy: Send + Sync + std::fmt::Debug {
    /// 策略名称 (用于调试/日志)
    fn name(&self) -> &str;

    /// 检查响应并决定后续行为
    /// @param resp: 原始响应（所有权转移）
    /// @param ctx: 提供修复环境所需的能力（Proxy/Browser/Session）
    async fn check(&self, resp: Response, ctx: &ServiceContext) -> Result<PolicyResult>;
}
