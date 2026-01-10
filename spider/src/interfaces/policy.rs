//! 网络策略拦截器 (Network Policy Interceptor)
//!
//! 定义响应校验协议及故障恢复指令。

use async_trait::async_trait;
use reqwest::Response;

use crate::core::error::{BlockReason, Result, SpiderError};
use crate::network::context::ServiceContext;

/// 策略决策指令 (Decision Variant)
#[derive(Debug)]
pub enum PolicyResult {
    /// 决策通过：允许响应进入下游解析管线
    Pass(Response),
    /// 指令重试：拦截当前流程并请求重试。
    /// `is_force` 标记用于跳过前置阻塞检查，执行探测性质的验证请求。
    Retry { is_force: bool, reason: BlockReason },
    /// 指令重定向：目标资源已物理迁移
    Redirect(String),
    /// 指令终止：检测到不可恢复的逻辑异常
    Fail(SpiderError),
}

/// 响应验证与环境修复协议 (Response Validation & Recovery)
///
/// 负责监测异常状态，并利用 `ServiceContext` 执行环境重置 (IP/Session/Challenge)。
#[async_trait]
pub trait NetworkPolicy: Send + Sync + std::fmt::Debug {
    /// 策略标识符
    fn name(&self) -> &str;

    /// 执行响应校验决策逻辑
    async fn check(&self, resp: Response, ctx: &ServiceContext) -> Result<PolicyResult>;
}
