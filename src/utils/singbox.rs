//! sing-box 进程管理器 (Process Controller)
//!
//! 负责代理核心进程的生命周期维护、动态配置生成及外部 API 指令编排。

use std::env::consts::EXE_SUFFIX;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, Url};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

/// sing-box 基础设施控制器
pub struct SingBoxController {
    /// 二进制文件路径
    executable: PathBuf,
    /// 运行时配置文件路径
    config_path: PathBuf,
    /// 后端控制接口基准 URL
    api_base: Url,
    /// 接口认证凭据
    api_secret: String,
    /// 活跃子进程句柄
    child: Mutex<Option<Child>>,
    /// 用于发送 API 指令的 HTTP 客户端
    client: Client,
}

impl SingBoxController {
    /// 初始化控制器实例
    pub fn new(bin_dir: &Path, cache_dir: &Path, api_port: u16, api_secret: &str) -> Result<Self> {
        let executable = bin_dir.join(format!("sing-box{}", EXE_SUFFIX));
        let config_path = cache_dir.join("config.json");

        let api_base = Url::parse(&format!("http://127.0.0.1:{}", api_port))
            .context("Invalid API URL construction")?;

        let client = Client::builder()
            .no_proxy() // 核心 API 请求必须绕过系统代理
            .timeout(Duration::from_secs(5))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            executable,
            config_path,
            api_base,
            api_secret: api_secret.to_string(),
            child: Mutex::new(None),
            client,
        })
    }

    /// 持久化核心配置文件
    pub async fn write_config(&self, config_content: &str) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create config directory")?;
        }
        tokio::fs::write(&self.config_path, config_content)
            .await
            .context("Failed to write config file")?;
        Ok(())
    }

    /// 启动代理核心进程
    pub async fn start(&self) -> Result<()> {
        let mut child_guard = self.child.lock().await;
        if child_guard.is_some() {
            debug!("Instance already active");
            return Ok(());
        }

        if !self.executable.exists() {
            return Err(anyhow!(
                "Artifact missing: {}",
                self.executable.display()
            ));
        }

        let log_path = self.config_path.with_file_name("sing-box.log");

        let log_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await
            .context("Failed to open artifact log")?;

        let log_file_std = log_file.into_std().await;

        info!("Spawning sing-box process...");

        let child = Command::new(&self.executable)
            .arg("run")
            .arg("-c")
            .arg(&self.config_path)
            .stdout(Stdio::from(
                log_file_std.try_clone().context("Handle cloning error")?,
            ))
            .stderr(Stdio::from(log_file_std))
            .kill_on_drop(true) // RAII: 释放资源时自动终止
            .spawn()
            .context("Failed to execute process")?;

        *child_guard = Some(child);
        Ok(())
    }

    /// 强制终止进程并释放资源
    pub async fn stop(&self) -> Result<()> {
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            child.kill().await.context("Failed to kill process")?;
            child.wait().await.context("Process wait error")?;
            info!("sing-box instance terminated");
        }
        Ok(())
    }

    /// 执行 API 健康检查，确保控制链路就绪
    pub async fn wait_for_api(&self, timeout_secs: u64) -> Result<()> {
        let url = self.api_base.join("proxies")?;

        timeout(Duration::from_secs(timeout_secs), async {
            loop {
                match self
                    .client
                    .get(url.clone())
                    .bearer_auth(&self.api_secret)
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("API endpoint is ready");
                        return Ok::<(), anyhow::Error>(());
                    }
                    _ => debug!("Awaiting API readiness..."),
                }
                sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("API readiness timeout"))??;

        Ok(())
    }

    /// 动态切换代理选择器的后端节点 (Selector Hot-swapping)
    pub async fn switch_selector(&self, selector: &str, tag: &str) -> Result<()> {
        let url = self.api_base.join(&format!("proxies/{}", selector))?;
        let body = serde_json::json!({ "name": tag });

        let resp = self
            .client
            .put(url)
            .bearer_auth(&self.api_secret)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Selector update failed ({} -> {}): Status {}, Body: {}",
                selector,
                tag,
                status,
                text
            ));
        }

        Ok(())
    }
}
