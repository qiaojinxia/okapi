//! Worker 任务启停过滤 + 复用 `okapi_kernel::supervision` 统一监督重启。
use std::{future::Future, sync::OnceLock};
use tokio::sync::watch;

/// 只观测 / 只外联、关掉也不影响记账的任务。气隙或只读部署用 `OKAPI_WORKER_DISABLE` 逗号分隔关掉；
/// 结算恢复、清扫、对账、分区、chsink 这类任务关了会错账或撑爆存储，不在清单里，写了也只打 WARN。
const OPTIONAL: &[&str] = &["egress_probe", "ops_sampler", "unit_calibration"];

/// `(要关的, 不允许关 / 认不出的)`。
fn parse_disabled(raw: &str) -> (Vec<&str>, Vec<&str>) {
    raw.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .partition(|name| OPTIONAL.contains(name))
}

fn disabled(name: &str) -> bool {
    static DISABLED: OnceLock<Vec<String>> = OnceLock::new();
    DISABLED
        .get_or_init(|| {
            let raw = std::env::var("OKAPI_WORKER_DISABLE").unwrap_or_default();
            let (off, rejected) = parse_disabled(&raw);
            if !rejected.is_empty() {
                tracing::warn!(
                    ?rejected,
                    allowed = ?OPTIONAL,
                    "OKAPI_WORKER_DISABLE: these tasks cannot be disabled; ignored"
                );
            }
            off.into_iter().map(str::to_owned).collect()
        })
        .iter()
        .any(|off| off == name)
}

pub(super) async fn run<F, Fut>(name: &'static str, stop: watch::Receiver<bool>, start: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    if disabled(name) {
        tracing::info!(worker = name, "disabled by OKAPI_WORKER_DISABLE");
        return;
    }
    okapi_kernel::supervision::run(name, stop, start).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    #[test]
    fn only_optional_tasks_can_be_disabled() {
        let (off, rejected) = parse_disabled(" egress_probe, ops_sampler ,,reconcile,sweep,typo");
        assert_eq!(off, ["egress_probe", "ops_sampler"]);
        assert_eq!(rejected, ["reconcile", "sweep", "typo"]);
        assert_eq!(parse_disabled(""), (vec![], vec![]));
    }

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
