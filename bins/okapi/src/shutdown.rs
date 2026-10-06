//! 优雅下线（IMPLEMENTATION §14.3）：三个角色共用的退出信号，以及
//! "响应已发、结算还在后台"的任务计数——进程要等这些结算落账再退出。

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// 等待 SIGINT 或 SIGTERM。编排层（Docker / K8s）停容器发的是 SIGTERM；
/// 只听 Ctrl-C 的进程会被它直接掐死，在途 SSE 断在半截、后台结算丢失。
pub async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "注册 SIGTERM 失败，仅响应 Ctrl-C");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("收到退出信号，开始优雅下线");
}

/// 后台结算任务计数。响应先行、结算后台是热路径优化（docs/perf-report.md），
/// 代价是 `axum::serve` 的排水只等连接关闭、不等这些任务——不计数就会在退出时丢账。
#[derive(Clone, Default)]
pub struct Pending(Arc<Inner>);

struct Inner {
    count: AtomicUsize,
    idle: Notify,
    /// 进入下线：长连接（Realtime 会话）据此收尾结算，而不是被进程退出掐断。
    draining: tokio::sync::watch::Sender<bool>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            count: AtomicUsize::default(),
            idle: Notify::default(),
            draining: tokio::sync::watch::channel(false).0,
        }
    }
}

// Created before spawning so panic and cancellation also drain the counter.
struct PendingTask(Arc<Inner>);

impl Drop for PendingTask {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_one();
        }
    }
}

impl Pending {
    /// 代替 `tokio::spawn` 提交一个后台结算；任务结束时计数减一，归零唤醒 `wait_idle`。
    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let inner = Arc::clone(&self.0);
        inner.count.fetch_add(1, Ordering::SeqCst);
        let task = PendingTask(inner);
        // detach 说明：任务由本计数器跟踪，退出路径经 wait_idle 等待，不需持有 JoinHandle
        tokio::spawn(async move {
            let _task = task;
            fut.await;
        });
    }

    /// 通知长连接收尾。HTTP 连接由 `with_graceful_shutdown` 排水，升级后的 WS 不在其中。
    pub fn drain(&self) {
        self.0.draining.send_replace(true);
    }

    /// 下线开始时完成；之前一直挂起。
    pub async fn draining(&self) {
        let mut draining = self.0.draining.subscribe();
        let _ = draining.wait_for(|draining| *draining).await;
    }

    /// 等全部后台结算结束，最多等 `cap`；超时只告警不阻塞退出（对账兜底）。
    pub async fn wait_idle(&self, cap: Duration) {
        let wait = async {
            while self.0.count.load(Ordering::SeqCst) > 0 {
                self.0.idle.notified().await;
            }
        };
        if tokio::time::timeout(cap, wait).await.is_err() {
            tracing::warn!(
                pending = self.0.count.load(Ordering::SeqCst),
                "后台结算未在下线窗口内完成，交由对账修复"
            );
        }
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.0.count.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn panic_does_not_block_shutdown() {
        let pending = Pending::default();
        pending.spawn(async { panic!("synthetic settlement panic") });
        tokio::time::timeout(
            Duration::from_secs(1),
            pending.wait_idle(Duration::from_secs(5)),
        )
        .await
        .expect("a panicked task must drain its counter");
        assert_eq!(pending.in_flight(), 0);
    }

    #[tokio::test]
    async fn drain_wakes_long_lived_sessions_before_the_idle_wait() {
        let pending = Pending::default();
        let session = pending.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        pending.spawn(async move {
            let _ = started.send(());
            session.draining().await;
        });
        ready.await.unwrap();
        assert_eq!(pending.in_flight(), 1);
        pending.drain();
        tokio::time::timeout(
            Duration::from_secs(1),
            pending.wait_idle(Duration::from_secs(5)),
        )
        .await
        .expect("a drained session finishes");
        assert_eq!(pending.in_flight(), 0);
        // 已在下线中：之后才开始等的也立即返回
        tokio::time::timeout(Duration::from_secs(1), pending.draining())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_an_unpolled_task_drains_its_counter() {
        let pending = Pending::default();
        pending.0.count.fetch_add(1, Ordering::SeqCst);
        let task = PendingTask(Arc::clone(&pending.0));
        let handle = tokio::spawn(async move {
            let _task = task;
            std::future::pending::<()>().await;
        });
        handle.abort();
        let _ = handle.await;
        assert_eq!(pending.in_flight(), 0);
    }
}
