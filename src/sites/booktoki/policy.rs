//! Booktoki 验证码对抗策略 (CAPTCHA Adversarial Policy)
//!
//! 实现自动化 K-CAPTCHA 识别与会话修复逻辑。

use async_trait::async_trait;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Method, Response};
use serde_json::json;
use tracing::{info, warn};
use url::Url;

use crate::core::error::{BlockReason, Result, SpiderError};
use crate::interfaces::policy::{NetworkPolicy, PolicyResult};
use crate::network::ResponseExt;
use crate::network::context::ServiceContext;
use crate::network::middleware::SkipPolicy;

/// 自动化验证码处理器
#[derive(Debug)]
pub struct CaptchaPolicy {
    /// 站点基准 URL
    base: Url,
}

impl CaptchaPolicy {
    pub fn new(base: Url) -> Self {
        Self { base }
    }

    fn normalize(&self, path: &str) -> String {
        crate::utils::to_absolute_url(&self.base, path)
    }

    /// 执行验证码识别与会话修复流水线 (Challenge-Response Pipeline)
    async fn do_solve_captcha(&self, url: &str, ctx: &ServiceContext) -> Result<()> {
        let solve_inner = async {
            // 1. 初始化验证码会话 (Session Initialization)
            let session_url = self.normalize("/plugin/kcaptcha/kcaptcha_session.php");
            ctx.request_builder(Method::POST, &session_url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&json!({})).unwrap_or_default())
                .with_extension(SkipPolicy::One("booktoki_captcha".into())) // 避免递归拦截 (Recursion Prevention)
                .send()
                .await
                .map_err(SpiderError::Middleware)?;

            // 2. 采集验证码特征素材
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let img_url = self.normalize(&format!("/plugin/kcaptcha/kcaptcha_image.php?t={}", ts));

            info!("Fetching CAPTCHA artifact...");
            let res = ctx
                .request_builder(Method::GET, &img_url)
                .with_extension(SkipPolicy::One("booktoki_captcha".into()))
                .send()
                .await
                .map_err(SpiderError::Middleware)?;

            let bytes = res.bytes().await.map_err(SpiderError::Network)?;

            // 3. 执行 OCR 识别 (Offloading to Blocking Thread)
            let code = tokio::task::spawn_blocking(move || booktoki_captcha::solve_captcha(&bytes))
                .await
                .map_err(|_| SpiderError::Custom("OCR worker panicked".into()))?
                .map_err(|_| SpiderError::CaptchaFailed)?;

            info!("OCR identification successful: {}, submitting payload...", code);

            // 4. 提交凭据校验 (Credential Submission)
            let encoded_url = utf8_percent_encode(url, NON_ALPHANUMERIC).to_string();
            let captcha_page_url = self.normalize(&format!("/bbs/captcha.php?url={}", encoded_url));
            let submit_url = self.normalize("/bbs/captcha_check.php");
            let form = vec![
                ("url".to_string(), url.to_string()),
                ("captcha_key".to_string(), code),
            ];

            let submit_res = ctx
                .request_builder(Method::POST, &submit_url)
                .header(reqwest::header::REFERER, captcha_page_url)
                .header(reqwest::header::ORIGIN, self.base.to_string())
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(serde_urlencoded::to_string(&form).unwrap_or_default())
                .with_extension(SkipPolicy::One("booktoki_captcha".into()))
                .send()
                .await
                .map_err(SpiderError::Middleware)?;

            // 5. 校验跳转反馈 (Redirect Feedback Analysis)
            let status = submit_res.status();
            if status.is_redirection() {
                let location = submit_res
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .map(|l: &reqwest::header::HeaderValue| String::from_utf8_lossy(l.as_bytes()))
                    .unwrap_or_default();

                if location.contains("captcha.php") {
                    return Err(SpiderError::CaptchaFailed);
                }
                info!("CAPTCHA verification passed");
            } else {
                warn!("Submission unexpected response (Status: {})", status);
                return Err(SpiderError::CaptchaFailed);
            }

            Ok::<(), SpiderError>(())
        };

        tokio::time::timeout(std::time::Duration::from_secs(45), solve_inner)
            .await
            .map_err(|_| SpiderError::Custom("CAPTCHA resolution timeout".into()))?
    }

    /// 执行抢占式修复任务
    async fn handle_captcha_solve(
        &self,
        target_url: &str,
        ctx: &ServiceContext,
    ) -> Result<PolicyResult> {
        // 利用协调器确保全局单一修复实例 (Singleton Fixer)
        if let Some(_guard) = ctx
            .coordinator
            .try_acquire_fix(BlockReason::Custom("captcha".into()))
            .await
        {
            match self.do_solve_captcha(target_url, ctx).await {
                Ok(_) => {
                    Ok(PolicyResult::Retry {
                        is_force: true,
                        reason: BlockReason::Custom("captcha_solved".into()),
                    })
                }
                Err(e) => {
                    // 修复失败，强行执行节点轮换。
                    // 注意：此处必须使用非阻塞锁版本的切换，防止死锁。
                    ctx.force_rotate_proxy().await;
                    Err(e)
                }
            }
        } else {
            // 已有其他任务在修复，挂起并等待恢复广播
            ctx.wait_if_blocked().await;
            Ok(PolicyResult::Retry {
                is_force: true,
                reason: BlockReason::Custom("waited_for_external_fix".into()),
            })
        }
    }
}

#[async_trait]
impl NetworkPolicy for CaptchaPolicy {
    fn name(&self) -> &str {
        "booktoki_captcha"
    }

    /// 实时监测响应状态并探测验证码陷阱 (Trap Detection)
    async fn check(&self, resp: Response, ctx: &ServiceContext) -> Result<PolicyResult> {
        let status = resp.status();
        let target_url = resp.original_url().to_string();

        if status.is_redirection() {
            let is_captcha = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .map(|l| String::from_utf8_lossy(l.as_bytes()).contains("captcha.php"))
                .unwrap_or(false);

            if is_captcha {
                info!("CAPTCHA trap detected, initiating resolution...");
                return self.handle_captcha_solve(&target_url, ctx).await;
            }
        }

        Ok(PolicyResult::Pass(resp))
    }
}
