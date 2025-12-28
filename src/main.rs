mod actors;
mod core;
mod engine;
mod network;
mod sites;
mod utils;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing::{error, info};

use crate::core::config::AppConfig;
use crate::engine::pipeline::ScrapeEngine;
use crate::network::service::HttpService;
use crate::network::session::Session;
use crate::sites::{Site, SiteContext, SiteRegistry, TaskArgs};

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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Spider启动中...");

    let config = Arc::new(AppConfig::load()?);
    let cli = Cli::parse();

    let (proxy_tx, _proxy_handle) = actors::proxy::ProxyManager::start(config.clone()).await;
    let http = Arc::new(HttpService::new(config.clone(), proxy_tx.clone()));
    let registry = SiteRegistry::new();

    match cli.command {
        Commands::Scrape {
            site: site_id,
            id,
            params,
        } => {
            let mut args = TaskArgs::new();
            args.insert("id".to_string(), id);
            for (k, v) in params {
                args.insert(k, v);
            }

            let site_cfg = config.sites.get(&site_id).cloned().unwrap_or_default();
            let site: Arc<dyn Site> = match registry.create(&site_id, site_cfg) {
                Some(s) => Arc::from(s),
                None => {
                    error!("未知的站点类型: {}。可用站点: {:?}", site_id, registry.list());
                    return Ok(());
                }
            };

            info!("开始执行任务: {}", site.id());

            let session = Arc::new(Session::new());
            session.set_ua("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into());

            let ctx = SiteContext {
                config: config.clone(),
                proxy: proxy_tx,
                http,
                session,
                bypasser: Arc::new(utils::browser::CloudflareBypasser),
                site: Some(site.clone()),
            };

            let engine = ScrapeEngine::new(site, ctx, config.clone());
            if let Err(e) = engine.run(args).await {
                error!("任务执行失败: {}", e);
            }
        }
    }

    Ok(())
}

fn parse_key_val(s: &str) -> std::result::Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no = found in {}", s))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}
