//! 进度显示 UI
//!
//! 基于 indicatif 实现的进度条显示，支持全局日志避让

use std::sync::{Arc, OnceLock};
use std::time::Duration;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use parking_lot::RwLock;
use tokio::task::JoinHandle;

use crate::core::event::{EventReceiver, SpiderEvent};

/// 全局 UI 容器，用于日志系统访问
static MULTI: OnceLock<MultiProgress> = OnceLock::new();

/// 获取全局 MultiProgress 实例
pub fn get_multi() -> &'static MultiProgress {
    MULTI.get_or_init(MultiProgress::new)
}

/// 进度条管理器
pub struct UiState {
    main_bar: Option<ProgressBar>,
    chapter_bar: Option<ProgressBar>,
}

impl UiState {
    fn new() -> Self {
        Self {
            main_bar: None,
            chapter_bar: None,
        }
    }
}

static STATE: OnceLock<Arc<RwLock<UiState>>> = OnceLock::new();

fn get_state() -> &'static Arc<RwLock<UiState>> {
    STATE.get_or_init(|| Arc::new(RwLock::new(UiState::new())))
}

pub struct Ui;

impl Ui {
    /// 启动事件处理循环
    pub fn run(receiver: EventReceiver) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = receiver.recv_async().await {
                Self::handle_event(event);
            }
        })
    }

    /// 处理 UI 事件
    fn handle_event(event: SpiderEvent) {
        let multi = get_multi();
        let state = get_state();
        let mut ui = state.write();

        match event {
            SpiderEvent::TaskStarted { title, .. } => {
                let style = ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] {msg}")
                    .unwrap()
                    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");

                let bar = multi.add(ProgressBar::new_spinner());
                bar.set_style(style);
                bar.set_message(format!("📚 {}", title));
                bar.enable_steady_tick(Duration::from_millis(100));
                ui.main_bar = Some(bar);
            }
            SpiderEvent::ChaptersDiscovered { total } => {
                let style = ProgressStyle::default_bar()
                    .template("{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}")
                    .unwrap()
                    .progress_chars("█▉▊▋▌▍▎▏  ");

                let bar = multi.add(ProgressBar::new(total as u64));
                bar.set_style(style);
                ui.chapter_bar = Some(bar);
            }
            SpiderEvent::ChapterProgress { current, title, .. } => {
                if let Some(ref bar) = ui.chapter_bar {
                    bar.set_position(current as u64);
                    bar.set_message(truncate_string(&title, 30));
                }
            }
            SpiderEvent::BlockDetected { reason, .. } => {
                if let Some(ref bar) = ui.main_bar {
                    bar.set_message(format!("⚠️ 阻断: {}", reason));
                }
            }
            SpiderEvent::Recovering { reason } => {
                if let Some(ref bar) = ui.main_bar {
                    bar.set_message(format!("🔄 修复中: {}", reason));
                }
            }
            SpiderEvent::RecoveryComplete => {
                if let Some(ref bar) = ui.main_bar {
                    bar.set_message("✅ 修复完成，继续任务");
                }
            }
            SpiderEvent::EpubGenerating => {
                if let Some(ref bar) = ui.main_bar {
                    bar.set_message("📖 正在生成 EPUB...");
                }
            }
            SpiderEvent::TaskCompleted { .. } => {
                if let Some(ref bar) = ui.chapter_bar {
                    bar.finish_with_message("✅ 下载完成");
                }
                if let Some(ref bar) = ui.main_bar {
                    bar.finish_with_message("✅ 任务完成");
                }
            }
            SpiderEvent::TaskFailed { error } => {
                if let Some(ref bar) = ui.main_bar {
                    bar.abandon_with_message(format!("❌ 任务失败: {}", error));
                }
            }
            _ => {}
        }
    }
}

/// 截断字符串
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}
