//! 常驻后台协程统一监督器：异常退出或 panic 时指数退避重启（1s → 30s），
//! 收到下线信号（`watch::Receiver<bool>`）后立即停止重启并优雅退出。
//!
//! gateway / console / worker 三个角色的所有常驻后台同步与维护任务统一收口于此，
//! 消除裸 `tokio::spawn` 发生单次 panic 后静默失联的风险。

use futures::FutureExt as _;
use std::{future::Future, panic::AssertUnwindSafe, time::Duration};
use tokio::sync::watch;

const INITIAL_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
const HEALTHY_RESET: Duration = Duration::from_mins(1);

/// 运行一个受监督的后台协程：非下线状态下的意外退出或 panic 按退避自动重启。
pub async fn run<F, Fut>(name: &'static str, mut stop: watch::Receiver<bool>, mut start: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut delay = INITIAL_DELAY;
    loop {
        if *stop.borrow() {
            return;
        }
        let started = std::time::Instant::now();
        let outcome = AssertUnwindSafe(async { start().await })
            .catch_unwind()
            .await;
        if *stop.borrow() || stop.has_changed().is_err() {
            return;
        }
        if started.elapsed() >= HEALTHY_RESET {
            delay = INITIAL_DELAY;
        }
        tracing::error!(
            task = name,
            panicked = outcome.is_err(),
            restart_delay_ms = delay.as_millis(),
            "supervised background task exited unexpectedly; restarting"
        );
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(delay) => {}
        }
        delay = delay.saturating_mul(2).min(MAX_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn panic_is_restarted_and_shutdown_stops_restarts() {
        let (stop, stopped) = watch::channel(false);
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = attempts.clone();
        let receiver = stopped.clone();
        let task = tokio::spawn(run("test", stopped, move || {
            let attempts = observed.clone();
            let mut receiver = receiver.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                assert!(attempt != 0, "injected supervised task panic");
                let _ = receiver.changed().await;
            }
        }));
        tokio::time::timeout(Duration::from_secs(3), async {
            while attempts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("task must restart after panic");
        stop.send(true).expect("send stop");
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("task must exit promptly on stop")
            .expect("task join ok");
    }
}
