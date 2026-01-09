//! 引擎运行时上下文

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use tokio::sync::Semaphore;

use crate::core::event::{EventSender, SpiderEvent};
use crate::interfaces::site::{Context, TaskArgs};
use crate::interfaces::Site;
use crate::network::context::ServiceContext;

/// 运行时上下文 (Runtime Context)
/// 用于在并发任务之间共享状态
pub struct RuntimeContext {
    pub site: Arc<dyn Site>,
    pub core: ServiceContext,
    pub semaphore: Arc<Semaphore>,
    pub images_dir: PathBuf,
    pub total_chapters: usize,
    pub completed_chapters: Arc<AtomicUsize>,
    pub events: Option<EventSender>,
    /// 任务参数 (已冻结)
    pub args: Arc<TaskArgs>,
    /// 任务 ID
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

    /// 发送事件
    pub fn emit(&self, event: SpiderEvent) {
        if let Some(ref sender) = self.events {
            sender.emit(event);
        }
    }

    /// 创建站点上下文
    pub fn make_site_context(&self) -> Context {
        Context::new(
            self.task_id.clone(),
            (*self.args).clone(),
            self.core.clone(),
        )
    }
}
