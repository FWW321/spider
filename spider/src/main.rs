#![allow(dead_code)]
#![allow(unused_imports)]

//! 应用程序入口 (Application Entrypoint)
//!
//! 负责 CLI 指令解析、遥测层初始化、依赖注入及系统生命周期管理。

mod actors;
mod core;
mod engine;
mod interfaces;
mod network;
mod sites;
mod ui;
mod utils;

use std::io;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::fmt::MakeWriter;

use crate::core::config::AppConfig;
use crate::core::event::create_event_channel;
use crate::engine::ScrapeEngine;
use crate::interfaces::Site;
use crate::interfaces::site::TaskArgs;
use crate::network::browser::BrowserService;
use crate::network::context::ServiceContext;
use crate::network::service::HttpService;
use crate::network::session::Session;
use crate::sites::SiteRegistry;
use crate::ui::{Ui, get_multi};

/// 进度条感知的日志写入器 (TUI-aware Log Writer)
/// 
/// 确保非同步日志输出不会破坏终端进度条的渲染布局。
struct IndicatifWriter;

impl io::Write for IndicatifWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = String::from_utf8_lossy(buf);
        let _ = get_multi().println(s.trim_end());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for IndicatifWriter {
    type Writer = IndicatifWriter;

    fn make_writer(&self) -> Self::Writer {
        IndicatifWriter
    }
}

/// 命令行界面脚手架 (CLI Scaffolding)
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 执行自动化抓取任务
    Scrape {
        /// 目标站点标识符
        #[arg(short, long)]
        site: String,
        /// 目标资源唯一标识 (ID/Slug)
        #[arg(short, long)]
        id: String,
        /// 动态注入的站点参数 (KEY=VALUE)
        #[arg(short, long, value_parser = parse_key_val)]
        params: Vec<(String, String)>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 遥测层初始化 (Telemetry Layer Initialization)
    if std::env::var("RUST_LOG").is_err() {
        unsafe {
            std::env::set_var("RUST_LOG", "info");
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(IndicatifWriter)
        .with_target(false)
        .with_ansi(true)
        .init();

    // 依赖项初始化与注入 (Dependency Injection)
    let config = Arc::new(AppConfig::load()?);
    let cli = Cli::parse();

    let (proxy_tx, _proxy_handle) = actors::proxy::ProxyManager::start(config.clone()).await;
    let session = Arc::new(Session::new());
    let http = Arc::new(HttpService::new(config.clone(), session.clone()));
    let browser = Arc::new(BrowserService::new(config.clone()));
    let registry = SiteRegistry::new();

    match cli.command {
        Commands::Scrape {
            site: site_id,
            id,
            params,
        } => {
            // 建立 UI 事件反馈链路 (Event feedback loop)
            let (event_sender, event_receiver) = create_event_channel();
            let ui_handle = Ui::run(event_receiver);

            // 任务域限制 (Scope isolation for proper RAII cleanup)
            {
                let mut args = TaskArgs::new();
                args.insert("id".to_string(), id);
                for (k, v) in params {
                    args.insert(k, v);
                }

                let session = Arc::new(Session::new());
                session.set_ua("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into());

                let ctx = ServiceContext::new(http, session, proxy_tx, browser, config.clone())
                    .with_events(event_sender);

                // 信号处理与优雅退出 (Signal Handling)
                let ctx_clone = ctx.clone();
                tokio::spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        ctx_clone.shutdown.cancel();
                    }
                });

                let site_cfg = config.sites.get(&site_id).cloned().unwrap_or_default();
                let site: Arc<dyn Site> = match registry.create(&site_id, site_cfg, ctx.clone()) {
                    Some(s) => Arc::from(s),
                    None => {
                        tracing::error!("Unknown site identifier: {}", site_id);
                        return Ok(());
                    }
                };

                let engine = ScrapeEngine::new(site, ctx, config.clone());
                let _ = engine.run(args).await;

                tracing::info!("Execution flow completed for: {}", site_id);
            }

            // Await UI shutdown after event sender closure
            let _ = ui_handle.await;
        }
    }

    Ok(())
}

/// 执行 KEY=VALUE 格式参数解析
fn parse_key_val(s: &str) -> std::result::Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no = found in {}", s))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}
