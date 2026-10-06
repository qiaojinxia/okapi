//! Restart unexpectedly stopped workers without restarting during shutdown.
use futures::FutureExt;
use std::{future::Future, panic::AssertUnwindSafe, time::Duration};
use tokio::sync::watch;

pub(super) async fn run<F, Fut>(name: &'static str, mut stop: watch::Receiver<bool>, mut start: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut delay = Duration::from_secs(1);
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
        if started.elapsed() >= Duration::from_mins(1) {
            delay = Duration::from_secs(1);
        }
        tracing::error!(
            worker = name,
            panicked = outcome.is_err(),
            restart_delay_ms = delay.as_millis(),
            "worker exited unexpectedly; restarting"
        );
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(delay) => {}
        }
        delay = delay.saturating_mul(2).min(Duration::from_secs(30));
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
        let worker = tokio::spawn(run("test", stopped, move || {
            let attempts = observed.clone();
            let mut receiver = receiver.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                assert!(attempt != 0, "injected worker panic");
                let _ = receiver.changed().await;
            }
        }));
        tokio::time::timeout(Duration::from_secs(3), async {
            while attempts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
