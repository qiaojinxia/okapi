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

#[derive(Default)]
struct Inner {
    count: AtomicUsize,
    idle: Notify,
}

impl Pending {
    /// 代替 `tokio::spawn` 提交一个后台结算；任务结束时计数减一，归零唤醒 `wait_idle`。
    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let inner = Arc::clone(&self.0);
        inner.count.fetch_add(1, Ordering::SeqCst);
        // detach 说明：任务由本计数器跟踪，退出路径经 wait_idle 等待，不需持有 JoinHandle
        tokio::spawn(async move {
            fut.await;
            if inner.count.fetch_sub(1, Ordering::SeqCst) == 1 {
                // notify_one 在无等待者时存一张许可，先归零后等待也不会漏醒
                inner.idle.notify_one();
            }
        });
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
