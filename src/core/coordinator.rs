//! 全局状态协调器 (Global State Coordinator)
//!
//! 利用 `tokio::sync::watch` 广播系统状态，配合 `Mutex` 实现抢占式修复。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, OwnedMutexGuard, watch};
use tracing::{debug, info};

/// 系统状态枚举
///
/// 核心状态只有两种：运行中 或 阻塞中。
/// 阻塞的具体原因由 BlockReason 携带，避免状态枚举膨胀。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemState {
    /// 绿灯：系统正常运行
    Running,
    /// 红灯：系统暂停，正在处理阻塞 (携带原因)
    Blocked(BlockReason),
}

impl SystemState {
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

/// 阻断原因
///
/// 描述造成系统阻塞的具体原因。
/// 核心枚举只包含通用的网络层/中间件层阻断。
/// 站点特定的业务阻断（如验证码、付费墙）应使用 Custom。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// IP 被封禁 (403/429)
    IpBlocked,
    /// Cloudflare 盾 (通用反爬中间件)
    Cloudflare,
    /// 认证失效 (需重新登录)
    TokenExpired,
    /// 速率限制
    RateLimit,
    /// 其他自定义原因 (如：站点验证码、维护中、付费墙等)
    Custom(String),
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockReason::IpBlocked => write!(f, "IpBlocked"),
            BlockReason::Cloudflare => write!(f, "Cloudflare"),
            BlockReason::TokenExpired => write!(f, "TokenExpired"),
            BlockReason::RateLimit => write!(f, "RateLimit"),
            BlockReason::Custom(s) => write!(f, "Custom({})", s),
        }
    }
}

/// 全局状态协调器
#[derive(Clone)]
pub struct Coordinator {
    state_tx: Arc<watch::Sender<SystemState>>,
    state_rx: watch::Receiver<SystemState>,
    fix_lock: Arc<Mutex<()>>,
}

impl Coordinator {
    pub fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(SystemState::Running);
        Self {
            state_tx: Arc::new(state_tx),
            state_rx,
            fix_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn state(&self) -> SystemState {
        self.state_rx.borrow().clone()
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state(), SystemState::Running)
    }

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

    pub async fn wait_until_running_timeout(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.wait_until_running())
            .await
            .is_ok()
    }

    /// 尝试获取修复权限
    ///
    /// 这是一个"抢锁"操作。
    /// - 成功：返回 Guard，系统状态变为 Blocked(reason)。
    /// - 失败：说明已有其他人正在修复，自动进入等待，直到系统恢复 Running 后返回 None。
    pub async fn try_acquire_fix(&self, reason: BlockReason) -> Option<FixGuard> {
        // 1. 快速检查：如果已经阻塞，直接等待别人修好
        if self.state().is_blocked() {
            debug!("Coordinator: System already blocked, waiting...");
            self.wait_until_running().await;
            return None;
        }

        // 2. 尝试获取物理锁
        let guard = self.fix_lock.clone().lock_owned().await;

        // 3. 拿到锁后再次检查状态
        if self.is_running() {
            let new_state = SystemState::Blocked(reason.clone());
            let _ = self.state_tx.send(new_state.clone());
            info!("Coordinator: State change -> {}", new_state);

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

    fn broadcast_running(&self) {
        if !self.is_running() {
            let _ = self.state_tx.send(SystemState::Running);
            info!("Coordinator: Recovered -> Running");
        }
    }
}

/// 修复守卫，在 drop 时自动恢复 Running 状态
pub struct FixGuard {
    coordinator: Coordinator,
    _guard: OwnedMutexGuard<()>,
}

impl Drop for FixGuard {
    fn drop(&mut self) {
        self.coordinator.broadcast_running();
    }
}
