#![allow(dead_code)]
#![allow(unused_imports)]

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

/// 自定义 Writer，用于将日志通过 indicatif 打印
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

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Scrape {
        #[arg(short, long)]
        site: String,
        #[arg(short, long)]
        id: String,
        #[arg(short, long, value_parser = parse_key_val)]
        params: Vec<(String, String)>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 设置日志环境变量
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "info"); }
    }

    // 初始化 tracing，使用自定义的 IndicatifWriter
    // 这样所有的 info!/warn!/error! 都会自动避让进度条
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(IndicatifWriter)
        .with_target(false) // 隐藏模块路径，让输出更整洁
        .with_ansi(true)    // 保留颜色
        .init();

    let config = Arc::new(AppConfig::load()?);
    let cli = Cli::parse();

    let (proxy_tx, _proxy_handle) = actors::proxy::ProxyManager::start(config.clone()).await;
    let session = Arc::new(Session::new());
    let http = Arc::new(HttpService::new(config.clone(), session.clone()));
    let browser = Arc::new(BrowserService::new(config.clone()));
    let registry = SiteRegistry::new();

    match cli.command {
        Commands::Scrape { site: site_id, id, params } => {
            let (event_sender, event_receiver) = create_event_channel();
            let ui_handle = Ui::run(event_receiver);

            // 使用代码块确保 ctx, engine, site 在任务完成后立即被 Drop
            // 从而关闭 event_sender，让 ui_handle 能够结束
            {
                let mut args = TaskArgs::new();
                args.insert("id".to_string(), id);
                for (k, v) in params { args.insert(k, v); }

                let session = Arc::new(Session::new());
                session.set_ua("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into());

                let ctx = ServiceContext::new(http, session, proxy_tx, browser, config.clone())
                    .with_events(event_sender);

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
                        tracing::error!("未知的站点类型: {}", site_id);
                        return Ok(());
                    }
                };

                let engine = ScrapeEngine::new(site, ctx, config.clone());
                let _ = engine.run(args).await;
                
                tracing::info!("任务完成: {}", site_id);
            }

            // 此时所有 Sender 已被 Drop，UI 线程将接收到 None 并退出
            let _ = ui_handle.await;
        }
    }

    Ok(())
}

fn parse_key_val(s: &str) -> std::result::Result<(String, String), String> {
    let pos = s.find('=').ok_or_else(|| format!("invalid KEY=VALUE: no = found in {}", s))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}