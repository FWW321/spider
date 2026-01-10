//! 引擎运行时上下文 (Runtime Context)
//!
//! 维护并发任务间的共享状态、资源配额及进度统计。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use tokio::sync::Semaphore;

use crate::core::event::{EventSender, SpiderEvent};
use crate::interfaces::Site;
use crate::interfaces::site::{Context, TaskArgs};
use crate::network::context::ServiceContext;

/// 任务执行运行时上下文
/// 
/// 聚合站点抽象、网络层上下文、并发信号量及进度监测点。
pub struct RuntimeContext {
    /// 目标站点抽象实现
    pub site: Arc<dyn Site>,
    /// 全局服务上下文
    pub core: ServiceContext,
    /// 并发控制信号量 (Throttling)
    pub semaphore: Arc<Semaphore>,
    /// 静态资源存储基准路径
    pub images_dir: PathBuf,
    /// 任务展平后的总章节数
    pub total_chapters: usize,
    /// 原子计数：已完成采集的章节数
    pub completed_chapters: Arc<AtomicUsize>,
    /// 事件分发句柄
    pub events: Option<EventSender>,
    /// 冻结后的任务初始化参数
    pub args: Arc<TaskArgs>,
    /// 任务唯一流水号 (Book ID)
    pub task_id: String,
}

impl RuntimeContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        site: Arc<dyn Site>,
        core: ServiceContext,
        concurrency: usize,
        images_dir: PathBuf,
        total_chapters: usize,
        events: Option<EventSender>,
        args: Arc<TaskArgs>,
        task_id: String,
    ) -> Self {
        Self {
            site,
            core,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            images_dir,
            total_chapters,
            completed_chapters: Arc::new(AtomicUsize::new(0)),
            events,
            args,
            task_id,
        }
    }

    /// 向事件总线推送消息
    pub fn emit(&self, event: SpiderEvent) {
        if let Some(ref sender) = self.events {
            sender.emit(event);
        }
    }

    /// 派生面向 Site Trait 的执行上下文
    pub fn make_site_context(&self) -> Context {
        Context::new(
            self.task_id.clone(),
            (*self.args).clone(),
            self.core.clone(),
        )
    }
}
