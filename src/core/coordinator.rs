//! 全局状态协调器 (Global State Coordinator)
//!
//! 基于 `tokio::sync::watch` 实现系统状态广播，结合 `Mutex` 提供抢占式的故障恢复机制。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, OwnedMutexGuard, watch};
use tracing::{debug, info};

use crate::core::error::BlockReason;

/// 系统运行状态枚举
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemState {
    /// 正常运行状态
    Running,
    /// 触发反爬策略或异常，系统进入阻塞状态
    Blocked(BlockReason),
}

impl SystemState {
    /// 检查当前是否处于阻塞状态
    pub fn is_blocked(&self) -> bool {
        match self {
            SystemState::Running => false,
            SystemState::Blocked(_) => true,
        }
    }
}

impl std::fmt::Display for SystemState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemState::Running => write!(f, "Running"),
            SystemState::Blocked(reason) => write!(f, "Blocked({})", reason),
        }
    }
}

/// 全局状态协调器 (Orchestrator)
///
/// 负责管理爬虫集群的生命周期状态，协调 IP 切换、验证码破解等故障恢复操作。
#[derive(Clone)]
pub struct Coordinator {
    /// 状态广播发送端
    state_tx: Arc<watch::Sender<SystemState>>,
    /// 状态订阅接收端
    state_rx: watch::Receiver<SystemState>,
    /// 故障恢复独占锁
    fix_lock: Arc<Mutex<()>>,
}

impl Coordinator {
    /// 初始化默认状态为 Running 的协调器
    pub fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(SystemState::Running);
        Self {
            state_tx: Arc::new(state_tx),
            state_rx,
            fix_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 获取当前快照状态
    pub fn state(&self) -> SystemState {
        self.state_rx.borrow().clone()
    }

    /// 判断系统是否正常运行
    pub fn is_running(&self) -> bool {
        matches!(self.state(), SystemState::Running)
    }

    /// 异步阻塞直至系统恢复 Running 状态
    pub async fn wait_until_running(&self) {
        let mut rx = self.state_rx.clone();
        loop {
            if matches!(*rx.borrow(), SystemState::Running) {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// 带超时的运行状态等待逻辑
    pub async fn wait_until_running_timeout(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.wait_until_running())
            .await
            .is_ok()
    }

    /// 尝试获取故障恢复权限 (Fix Authority)
    ///
    /// 采用抢占式设计：
    /// - 若获取成功，返回 `FixGuard`，系统状态切换至 `Blocked`。
    /// - 若获取失败（已有其他任务在修复），则等待修复完成后返回 `None`。
    pub async fn try_acquire_fix(&self, reason: BlockReason) -> Option<FixGuard> {
        if self.state().is_blocked() {
            debug!("Coordinator: System already blocked, waiting for recovery...");
            self.wait_until_running().await;
            return None;
        }

        let guard = self.fix_lock.clone().lock_owned().await;

        if self.is_running() {
            let new_state = SystemState::Blocked(reason.clone());
            let _ = self.state_tx.send(new_state.clone());
            info!("Coordinator: State transition -> {}", new_state);

            Some(FixGuard {
                coordinator: self.clone(),
                _guard: guard,
            })
        } else {
            drop(guard);
            self.wait_until_running().await;
            None
        }
    }

    /// 广播恢复正常运行的消息
    fn broadcast_running(&self) {
        if !self.is_running() {
            let _ = self.state_tx.send(SystemState::Running);
            info!("Coordinator: State transition -> Running");
        }
    }
}


/// Recovery Guard
///
/// 利用 RAII 机制，在 Guard 释放时自动将系统状态重置为 Running。
pub struct FixGuard {
    coordinator: Coordinator,
    _guard: OwnedMutexGuard<()>,
}

impl Drop for FixGuard {
    fn drop(&mut self) {
        self.coordinator.broadcast_running();
    }
}
