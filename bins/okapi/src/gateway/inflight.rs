//! 本节点 HTTP 在途量：持有守卫计数，空闲时不写 Redis，活动期间定期续报。
//! 后台任务只持有 receiver，不延长网关/守卫的寿命；最后一个持有者离开时收尾归零。

use super::sched_redis::SchedulerRedis;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, watch};
use tokio::time::Instant;

const REPORT_INTERVAL: Duration = Duration::from_secs(1);
const REPORT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct InFlightGauge {
    inner: Arc<Counter>,
}

struct Counter {
    count: watch::Sender<i64>,
    reporter: Arc<Reporter>,
}

struct Reporter {
    sched: SchedulerRedis,
    node: Arc<str>,
    latest: watch::Receiver<i64>,
    // Serializes writes and reads the latest count only after acquiring the
    // lock: an older asynchronous report cannot overwrite a newer zero.
    last: Mutex<Option<(Instant, i64)>>,
}

impl InFlightGauge {
    pub fn new(sched: SchedulerRedis, node: &str) -> Self {
        let (count, changes) = watch::channel(0);
        let reporter = Arc::new(Reporter {
            sched,
            node: Arc::from(node),
            latest: changes.clone(),
            last: Mutex::new(None),
        });
        tokio::spawn(report_changes(Arc::clone(&reporter), changes));
        Self {
            inner: Arc::new(Counter { count, reporter }),
        }
    }

    pub(crate) async fn enter(&self) -> InFlightGuard {
        self.inner.count.send_modify(|count| *count += 1);
        // Create the guard before awaiting: cancellation during the initial
        // report must release the count too.
        let guard = InFlightGuard(Arc::clone(&self.inner));
        self.inner.reporter.publish(false).await;
        guard
    }
}

pub(crate) struct InFlightGuard(Arc<Counter>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        // A watch update is synchronous and bounded; no per-response task is
        // spawned, including for errors, disconnects and handler cancellation.
        self.0.count.send_modify(|count| *count -= 1);
    }
}

impl Reporter {
    async fn publish(&self, force: bool) {
        let mut last = self.last.lock().await;
        let count = *self.latest.borrow();
        if last.is_none() && count == 0 {
            return;
        }
        if !force
            && last.as_ref().is_some_and(|(at, previous)| {
                // Both zero->active and active->zero bypass throttling. Other
                // changes coalesce within one second, preserving soft realtime
                // pricing without one Redis write per arriving request.
                (*previous == 0) == (count == 0) && at.elapsed() < REPORT_INTERVAL
            })
        {
            return;
        }
        if tokio::time::timeout(
            REPORT_TIMEOUT,
            self.sched.inflight_report(&self.node, count),
        )
        .await
        .is_err()
        {
            tracing::debug!(node = %self.node, "在途量上报超时");
        }
        // Bound retries during an outage as well as successful writes. The
        // monotonic clock is independent of wall-clock adjustments.
        *last = Some((Instant::now(), count));
    }
}

async fn report_changes(reporter: Arc<Reporter>, mut changes: watch::Receiver<i64>) {
    loop {
        let active = *changes.borrow() > 0;
        let closed = tokio::select! {
            result = changes.changed() => result.is_err(),
            () = tokio::time::sleep(REPORT_INTERVAL), if active => false,
        };
        reporter.publish(closed).await;
        if closed {
            break;
        }
    }
}
