use std::env::consts::EXE_SUFFIX;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::{Client, Url};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

pub struct SingBoxController {
    executable: PathBuf,
    config_path: PathBuf,
    api_base: Url,
    api_secret: String,
    child: Mutex<Option<Child>>,
    client: Client,
}

impl SingBoxController {
    pub fn new(bin_dir: &Path, cache_dir: &Path, api_port: u16, api_secret: &str) -> Result<Self> {
        // 自动处理不同系统的扩展名 (Windows 为 .exe，Linux/Mac 为空)
        let executable = bin_dir.join(format!("sing-box{}", EXE_SUFFIX));
        let config_path = cache_dir.join("config.json");

        // 预处理 API URL，避免后续每次请求都解析
        let api_base = Url::parse(&format!("http://127.0.0.1:{}", api_port))
            .context("Invalid API URL construction")?;

        let client = Client::builder()
            .no_proxy()
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

    pub async fn start(&self) -> Result<()> {
        let mut child_guard = self.child.lock().await;
        if child_guard.is_some() {
            debug!("sing-box 已经在运行中");
            return Ok(());
        }

        if !self.executable.exists() {
            return Err(anyhow!("未找到 sing-box 执行文件: {}", self.executable.display()));
        }

        let log_path = self.config_path.with_file_name("sing-box.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .context("无法打开 sing-box 日志文件")?;

        info!("正在启动 sing-box...");

        let child = Command::new(&self.executable)
            .arg("run")
            .arg("-c")
            .arg(&self.config_path)
            .stdout(Stdio::from(log_file.try_clone()?))
            .stderr(Stdio::from(log_file))
            .kill_on_drop(true)
            .spawn()
            .context("启动 sing-box 进程失败")?;

        *child_guard = Some(child);
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            child.kill().await.context("无法停止 sing-box")?;
            child.wait().await.context("等待 sing-box 退出失败")?;
            info!("sing-box 已停止");
        }
        Ok(())
    }

    pub async fn wait_for_api(&self, timeout_secs: u64) -> Result<()> {
        let url = self.api_base.join("proxies")?;

        timeout(Duration::from_secs(timeout_secs), async {
            loop {
                match self.client
                    .get(url.clone())
                    .bearer_auth(&self.api_secret)
                    .send()
                    .await 
                {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("sing-box API 已就绪");
                        return Ok::<(), anyhow::Error>(());
                    }
                    _ => debug!("正在等待 sing-box API 响应..."),
                }
                sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("等待 sing-box API 超时"))??; 

        Ok(())
    }

    pub async fn switch_selector(&self, selector: &str, tag: &str) -> Result<()> {
        // 使用 join 安全拼接 URL
        let url = self.api_base.join(&format!("proxies/{}", selector))?;
        let body = serde_json::json!({ "name": tag });

        let resp = self.client
            .put(url)
            .bearer_auth(&self.api_secret)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to switch proxy ({} -> {}): Status {}, Body: {}",
                selector, tag, status, text
            ));
        }

        Ok(())
    }
}