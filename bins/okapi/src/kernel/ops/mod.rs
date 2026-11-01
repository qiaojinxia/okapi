//! 运维监控（IMPLEMENTATION §11.43）：服务器压力、中间件占用、近 24 小时趋势与告警日志。
//! 只读：会改数据的运维动作在控制台「运维操作」页（console::dlq 等），这里只看数。

use futures::FutureExt as _;
use std::{future::Future, panic::AssertUnwindSafe};

pub mod egress_traffic;

// 监控读数是展示用的近似值：u64 计数转 f64 丢掉的低位、秒数取整都无关紧要
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub mod host;
pub mod logbuf;
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub mod probes;
pub mod sampler;

/// 周期任务的一轮：panic 单独兜住并记一条错误，调用方的循环照常进入下一轮。
/// 裸 `tokio::spawn` 的循环只要有一轮 panic，整个任务就无声无息地死掉，此后永远不再执行。
pub(crate) async fn guarded(task: &'static str, round: impl Future<Output = ()>) {
    if AssertUnwindSafe(round).catch_unwind().await.is_err() {
        tracing::error!(
            task,
            "background round panicked; the next round runs as usual"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::guarded;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// 第一轮 panic，spawn 出去的循环不死，后两轮照常跑完。
    #[tokio::test]
    async fn a_panicking_round_does_not_end_the_spawned_loop() {
        let finished = Arc::new(AtomicUsize::new(0));
        let seen = finished.clone();
        let task = tokio::spawn(async move {
            for round in 0..3 {
                let seen = seen.clone();
                guarded("test", async move {
                    assert!(round != 0, "injected round panic");
                    seen.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            }
        });
        task.await.expect("the loop task itself must not panic");
        assert_eq!(finished.load(Ordering::SeqCst), 2);
    }
}
