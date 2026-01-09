use async_trait::async_trait;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{Response, Method};
use serde_json::json;
use tracing::{info, warn};
use url::Url;

use crate::core::coordinator::BlockReason;
use crate::core::error::{Result, SpiderError};
use crate::interfaces::policy::{NetworkPolicy, PolicyResult};
use crate::network::context::ServiceContext;
use crate::network::ResponseExt;
use crate::network::middleware::SkipPolicy;

#[derive(Debug)]
pub struct CaptchaPolicy {
    base: Url,
}

impl CaptchaPolicy {
    pub fn new(base: Url) -> Self {
        Self { base }
    }

    fn normalize(&self, path: &str) -> String {
        crate::utils::to_absolute_url(&self.base, path)
    }

    /// 核心修复逻辑：解题并更新 Cookie
    async fn do_solve_captcha(&self, url: &str, ctx: &ServiceContext) -> Result<()> {
        let solve_inner = async {
            // 1. 初始化会话
            let session_url = self.normalize("/plugin/kcaptcha/kcaptcha_session.php");
            ctx.request_builder(Method::POST, &session_url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&json!({})).unwrap_or_default())
                .with_extension(SkipPolicy::One("booktoki_captcha".into())) // 仅跳过自己，防止递归
                .send()
                .await
                .map_err(SpiderError::Middleware)?;

            // 2. 下载验证码图片
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
            let img_url = self.normalize(&format!("/plugin/kcaptcha/kcaptcha_image.php?t={}", ts));
            
            info!("正在获取验证码图片...");
            let res = ctx.request_builder(Method::GET, &img_url)
                .with_extension(SkipPolicy::One("booktoki_captcha".into()))
                .send()
                .await
                .map_err(SpiderError::Middleware)?;

            let bytes = res.bytes().await.map_err(SpiderError::Network)?;
            
            // 3. 本地识别 (同步逻辑切线程)
            let code = tokio::task::spawn_blocking(move || {
                booktoki_captcha::solve_captcha(&bytes)
            }).await.map_err(|_| SpiderError::Custom("识别任务崩溃".into()))?
              .map_err(|_| SpiderError::CaptchaFailed)?;

            info!("验证码识别成功: {}，正在提交...", code);

            // 4. 提交校验
            let encoded_url = utf8_percent_encode(url, NON_ALPHANUMERIC).to_string();
            let captcha_page_url = self.normalize(&format!("/bbs/captcha.php?url={}", encoded_url));
            let submit_url = self.normalize("/bbs/captcha_check.php");
            let form = vec![("url".to_string(), url.to_string()), ("captcha_key".to_string(), code)];
            
            let submit_res = ctx.request_builder(Method::POST, &submit_url)
                .header(reqwest::header::REFERER, captcha_page_url)
                .header(reqwest::header::ORIGIN, self.base.to_string())
                .form(&form)
                .with_extension(SkipPolicy::One("booktoki_captcha".into()))
                .send()
                .await
                .map_err(SpiderError::Middleware)?;

            // 5. 判定校验结果
            let status = submit_res.status();
            if status.is_redirection() {
                let location = submit_res.headers().get(reqwest::header::LOCATION)
                    .map(|l| String::from_utf8_lossy(l.as_bytes()))
                    .unwrap_or_default();
                
                if location.contains("captcha.php") {
                    return Err(SpiderError::CaptchaFailed);
                }
                info!("验证码校验成功");
            } else {
                warn!("验证码提交后未发生重定向 (Status: {})", status);
                return Err(SpiderError::CaptchaFailed);
            }
            
            Ok::<(), SpiderError>(())
        };

        tokio::time::timeout(std::time::Duration::from_secs(45), solve_inner)
            .await
            .map_err(|_| SpiderError::Custom("验证码修复超时".into()))?
    }

    async fn handle_captcha_solve(&self, target_url: &str, ctx: &ServiceContext) -> Result<PolicyResult> {
        // 尝试抢占修复权
        if let Some(_guard) = ctx.coordinator.try_acquire_fix(BlockReason::Custom("captcha".into())).await {
            match self.do_solve_captcha(target_url, ctx).await {
                Ok(_) => {
                    // 修复成功，返回"强制重试"指令
                    Ok(PolicyResult::Retry { is_force: true, reason: "captcha_solved".into() })
                }
                Err(e) => {
                    // 修复失败，旋转代理
                    // CRITICAL: 必须使用 force_rotate_proxy，因为当前已经持有了 FixGuard。
                    // 调用带锁的 rotate_proxy() 会尝试再次抢锁，导致死锁 (Waiting for self)。
                    ctx.force_rotate_proxy().await;
                    Err(e)
                }
            }
        } else {
            // 没抢到锁，说明别人已经修好了或正在修，等待系统恢复后直接重试
            ctx.wait_if_blocked().await;
            Ok(PolicyResult::Retry { is_force: true, reason: "waited_for_captcha_fix".into() })
        }
    }
}

#[async_trait]
impl NetworkPolicy for CaptchaPolicy {
    fn name(&self) -> &str { "booktoki_captcha" }

    async fn check(&self, resp: Response, ctx: &ServiceContext) -> Result<PolicyResult> {
        let status = resp.status();
        let target_url = resp.original_url().to_string();

        // 识别 302 到 captcha.php
        if status.is_redirection() {
            let is_captcha = resp.headers().get(reqwest::header::LOCATION)
                .map(|l| String::from_utf8_lossy(l.as_bytes()).contains("captcha.php"))
                .unwrap_or(false);

            if is_captcha {
                info!("检测到验证码页面，启动修复流程...");
                return self.handle_captcha_solve(&target_url, ctx).await;
            }
        }

        Ok(PolicyResult::Pass(resp))
    }
}
