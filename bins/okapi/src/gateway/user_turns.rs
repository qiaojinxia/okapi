//! 同一用户的账本临界区（预扣 / 结算）先在进程内排队，轮到了才去拿 PG 连接与用户咨询锁。
//!
//! PG 咨询锁只保证串行，不管等待者占着什么：一个高并发用户的几十个请求会各自拿着
//! 池连接卡在 `pg_advisory_lock` 上，把全站连接池耗尽；结算还会先占着全局结算闸再等锁，
//! 一个大客户就能让所有人的结算排队。这里按用户 FIFO 排队，其余等待者不占任何共享资源；
//! 跨进程的串行仍由 PG 锁负责。
//!
//! 预扣与结算各用一个登记表（`AppState::admission_turns` / `settlement_turns`）：结算先排队
//! 再等全局结算闸，若与预扣共用一队，闸满时排着的结算会挡住同一用户的新请求准入。
//! 分开后每个用户在每个进程至多两条连接等锁；结算持闸只等 PG 锁，预扣不碰闸，无环形等待。
use okapi_ledger::LedgerError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;
use tokio::sync::{Mutex as Queue, OwnedMutexGuard};

/// 预扣在客户端请求路径上：排不上就让调用方稍后重试，不无限挂着连接。
pub const ADMISSION_WAIT: Duration = Duration::from_secs(5);
/// 结算在后台：多等一会儿比退回重试（重新排到队尾）更公平。
pub const SETTLEMENT_WAIT: Duration = Duration::from_mins(1);

#[derive(Clone, Default)]
pub struct UserTurns(Arc<Mutex<Registry>>);

#[derive(Default)]
struct Registry {
    queues: HashMap<i64, Weak<Queue<()>>>,
    prune_at: usize,
}

/// 持有期间本进程内该用户的其它账本操作在排队；drop 即轮到下一位。
pub struct UserTurn {
    _held: OwnedMutexGuard<()>,
}

impl UserTurns {
    pub async fn wait(&self, user_id: i64, limit: Duration) -> Result<UserTurn, LedgerError> {
        let queue = self.queue(user_id);
        tokio::time::timeout(limit, queue.lock_owned())
            .await
            .map(|held| UserTurn { _held: held })
            .map_err(|_| LedgerError::UserBusy)
    }

    fn queue(&self, user_id: i64) -> Arc<Queue<()>> {
        let mut registry = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(queue) = registry.queues.get(&user_id).and_then(Weak::upgrade) {
            return queue;
        }
        // 空闲用户的队列随最后一个持有者释放；按翻倍阈值清理死项，摊还 O(1)
        if registry.queues.len() >= registry.prune_at {
            registry.queues.retain(|_, queue| queue.strong_count() > 0);
            registry.prune_at = (registry.queues.len() * 2).max(1024);
        }
        let queue = Arc::new(Queue::new(()));
        registry.queues.insert(user_id, Arc::downgrade(&queue));
        queue
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queues
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_user_waits_in_order_and_other_users_do_not() {
        let turns = UserTurns::default();
        let first = turns.wait(7, ADMISSION_WAIT).await.unwrap();
        // 另一个用户不受影响
        let other = tokio::time::timeout(Duration::from_millis(100), turns.wait(8, ADMISSION_WAIT))
            .await
            .expect("other users never queue behind user 7")
            .unwrap();
        drop(other);
        // 同一用户排队，超时给出可重试的 UserBusy
        assert!(matches!(
            turns.wait(7, Duration::from_millis(50)).await,
            Err(LedgerError::UserBusy)
        ));
        let waiter = tokio::spawn({
            let turns = turns.clone();
            async move { turns.wait(7, ADMISSION_WAIT).await.map(|_| ()) }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "still queued behind the holder");
        drop(first);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn idle_users_do_not_accumulate() {
        let turns = UserTurns::default();
        for user in 0..5000 {
            drop(turns.wait(user, ADMISSION_WAIT).await.unwrap());
        }
        assert!(turns.tracked() <= 2048, "{}", turns.tracked());
    }
}
