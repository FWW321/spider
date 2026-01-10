//! 终端进度渲染引擎 (Terminal UI Progress Engine)
//! 
//! 基于 `indicatif` 实现非阻塞式进度条编排，支持多任务管线状态的实时同步。

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use parking_lot::RwLock;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::core::event::{EventReceiver, SpiderEvent};

/// 全局 TUI 容器 (Singleton)
static MULTI: OnceLock<MultiProgress> = OnceLock::new();

/// 获取全局进度容器实例
pub fn get_multi() -> &'static MultiProgress {
    MULTI.get_or_init(MultiProgress::new)
}

/// TUI 状态容器
pub struct UiState {
    /// 全局任务主状态条
    main_bar: Option<ProgressBar>,
    /// 资源采集进度条
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

/// 进度协调器 (Progress Orchestrator)
pub struct Ui;

impl Ui {
    /// 激活事件监听循环，启动异步渲染管线
    pub fn run(receiver: EventReceiver) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = receiver.recv_async().await {
                Self::handle_event(event);
            }
        })
    }

    /// 执行 UI 状态转换与渲染更新
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
                    bar.set_message(format!("⚠️ BLOCK: {}", reason));
                }
            }
            SpiderEvent::Recovering { reason } => {
                if let Some(ref bar) = ui.main_bar {
                    bar.set_message(format!("🔄 RECOVERING: {}", reason));
                }
            }
            SpiderEvent::RecoveryComplete => {
                if let Some(ref bar) = ui.main_bar {
                    bar.set_message("✅ RECOVERED: Resuming pipeline...");
                }
            }
            SpiderEvent::EpubGenerating => {
                if let Some(ref bar) = ui.main_bar {
                    bar.set_message("📖 COMPILING: Generating artifact...");
                }
            }
            SpiderEvent::TaskCompleted { .. } => {
                if let Some(ref bar) = ui.chapter_bar {
                    bar.finish_with_message("✅ DOWNLOADED");
                }
                if let Some(ref bar) = ui.main_bar {
                    bar.finish_with_message("✅ TASK FINISHED");
                }
            }
            SpiderEvent::TaskFailed { error } => {
                if let Some(ref bar) = ui.main_bar {
                    bar.abandon_with_message(format!("❌ FAILED: {}", error));
                }
            }
            _ => {}
        }
    }
}

/// 执行语义化字符串截断
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}
