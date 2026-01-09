//! 事件系统定义
//!
//! 用于 Engine 与 UI 之间的完全解耦通信

use flume::{Receiver, Sender};

/// Spider 事件类型
#[derive(Debug, Clone)]
pub enum SpiderEvent {
    /// 任务开始
    TaskStarted {
        site_id: String,
        book_id: String,
        title: String,
    },

    /// 发现章节总数
    ChaptersDiscovered { total: usize },

    /// 章节下载进度
    ChapterProgress {
        current: usize,
        total: usize,
        title: String,
    },

    /// 章节下载完成
    ChapterCompleted { index: usize, title: String },

    /// 章节下载失败
    ChapterFailed {
        index: usize,
        title: String,
        error: String,
    },

    /// 图片下载进度
    ImageProgress { downloaded: usize, total: usize },

    /// 封面下载完成
    CoverDownloaded,

    /// 检测到阻断
    BlockDetected { reason: String, url: String },

    /// 阻断恢复中
    Recovering { reason: String },

    /// 阻断恢复完成
    RecoveryComplete,

    /// 代理切换
    ProxyRotated { new_proxy: Option<String> },

    /// EPUB 生成开始
    EpubGenerating,

    /// EPUB 生成完成
    EpubGenerated { path: String },

    /// 任务完成
    TaskCompleted { title: String },

    /// 任务失败
    TaskFailed { error: String },

    /// 日志消息（用于 UI 显示）
    Log { level: LogLevel, message: String },
}

/// 日志级别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// 事件发送器
#[derive(Clone)]
pub struct EventSender {
    tx: Sender<SpiderEvent>,
}

impl EventSender {
    pub fn new(tx: Sender<SpiderEvent>) -> Self {
        Self { tx }
    }

    /// 发送事件
    pub fn emit(&self, event: SpiderEvent) {
        let _ = self.tx.send(event);
    }

    /// 发送任务开始事件
    pub fn task_started(&self, site_id: &str, book_id: &str, title: &str) {
        self.emit(SpiderEvent::TaskStarted {
            site_id: site_id.to_string(),
            book_id: book_id.to_string(),
            title: title.to_string(),
        });
    }

    /// 发送章节进度事件
    pub fn chapter_progress(&self, current: usize, total: usize, title: &str) {
        self.emit(SpiderEvent::ChapterProgress {
            current,
            total,
            title: title.to_string(),
        });
    }

    /// 发送章节完成事件
    pub fn chapter_completed(&self, index: usize, title: &str) {
        self.emit(SpiderEvent::ChapterCompleted {
            index,
            title: title.to_string(),
        });
    }

    /// 发送日志事件
    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.emit(SpiderEvent::Log {
            level,
            message: message.into(),
        });
    }

    /// 发送信息日志
    pub fn info(&self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    /// 发送警告日志
    pub fn warn(&self, message: impl Into<String>) {
        self.log(LogLevel::Warn, message);
    }

    /// 发送错误日志
    pub fn error(&self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }
}

/// 事件接收器
pub struct EventReceiver {
    rx: Receiver<SpiderEvent>,
}

impl EventReceiver {
    pub fn new(rx: Receiver<SpiderEvent>) -> Self {
        Self { rx }
    }

    /// 阻塞接收事件
    pub fn recv(&self) -> Option<SpiderEvent> {
        self.rx.recv().ok()
    }

    /// 非阻塞接收事件
    pub fn try_recv(&self) -> Option<SpiderEvent> {
        self.rx.try_recv().ok()
    }

    /// 异步接收事件
    pub async fn recv_async(&self) -> Option<SpiderEvent> {
        self.rx.recv_async().await.ok()
    }

    /// 获取内部接收器引用
    pub fn inner(&self) -> &Receiver<SpiderEvent> {
        &self.rx
    }
}

/// 创建事件通道
pub fn create_event_channel() -> (EventSender, EventReceiver) {
    let (tx, rx) = flume::unbounded();
    (EventSender::new(tx), EventReceiver::new(rx))
}
