//! 事件总线系统 (Event Bus System)
//! 
//! 基于 MPMC (Multi-Producer, Multi-Consumer) 架构实现 Engine 与 UI 之间的解耦通信。

use flume::{Receiver, Sender};

/// 全局生命周期事件 (Spider Lifecycle Events)
#[derive(Debug, Clone)]
pub enum SpiderEvent {
    /// 抓取任务初始化
    TaskStarted {
        site_id: String,
        book_id: String,
        title: String,
    },

    /// 资源发现完成
    ChaptersDiscovered { total: usize },

    /// 章节采集进度更新
    ChapterProgress {
        current: usize,
        total: usize,
        title: String,
    },

    /// 单章节采集完成
    ChapterCompleted { index: usize, title: String },

    /// 单章节采集失败
    ChapterFailed {
        index: usize,
        title: String,
        error: String,
    },

    /// 媒体资源下载进度
    ImageProgress { downloaded: usize, total: usize },

    /// 封面资源下载完成
    CoverDownloaded,

    /// 观测到反爬阻断
    BlockDetected { reason: String, url: String },

    /// 故障恢复流程激活
    Recovering { reason: String },

    /// 故障恢复流程完成
    RecoveryComplete,

    /// 出口代理轮换
    ProxyRotated { new_proxy: Option<String> },

    /// 文档编译开始
    EpubGenerating,

    /// 文档编译完成
    EpubGenerated { path: String },

    /// 任务队列全部清空
    TaskCompleted { title: String },

    /// 任务出现致命错误
    TaskFailed { error: String },

    /// 结构化日志消息 (用于 UI 实时展现)
    Log { level: LogLevel, message: String },
}

/// 语义化日志等级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// 事件分发器 (Event Dispatcher)
#[derive(Clone)]
pub struct EventSender {
    tx: Sender<SpiderEvent>,
}

impl EventSender {
    pub fn new(tx: Sender<SpiderEvent>) -> Self {
        Self { tx }
    }

    /// 将事件推入总线
    pub fn emit(&self, event: SpiderEvent) {
        let _ = self.tx.send(event);
    }

    pub fn task_started(&self, site_id: &str, book_id: &str, title: &str) {
        self.emit(SpiderEvent::TaskStarted {
            site_id: site_id.to_string(),
            book_id: book_id.to_string(),
            title: title.to_string(),
        });
    }

    pub fn chapter_progress(&self, current: usize, total: usize, title: &str) {
        self.emit(SpiderEvent::ChapterProgress {
            current,
            total,
            title: title.to_string(),
        });
    }

    pub fn chapter_completed(&self, index: usize, title: &str) {
        self.emit(SpiderEvent::ChapterCompleted {
            index,
            title: title.to_string(),
        });
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.emit(SpiderEvent::Log {
            level,
            message: message.into(),
        });
    }

    pub fn info(&self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.log(LogLevel::Warn, message);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }
}

/// 事件接收端 (Event Consumer)
pub struct EventReceiver {
    rx: Receiver<SpiderEvent>,
}

impl EventReceiver {
    pub fn new(rx: Receiver<SpiderEvent>) -> Self {
        Self { rx }
    }

    /// 阻塞式监听
    pub fn recv(&self) -> Option<SpiderEvent> {
        self.rx.recv().ok()
    }

    /// 轮询式监听
    pub fn try_recv(&self) -> Option<SpiderEvent> {
        self.rx.try_recv().ok()
    }

    /// 异步监听
    pub async fn recv_async(&self) -> Option<SpiderEvent> {
        self.rx.recv_async().await.ok()
    }

    /// 获取底层接收器引用
    pub fn inner(&self) -> &Receiver<SpiderEvent> {
        &self.rx
    }
}

/// 创建双向事件通道
pub fn create_event_channel() -> (EventSender, EventReceiver) {
    let (tx, rx) = flume::unbounded();
    (EventSender::new(tx), EventReceiver::new(rx))
}

