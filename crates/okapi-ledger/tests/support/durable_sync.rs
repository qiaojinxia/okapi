use super::*;
use fred::interfaces::{HashesInterface, KeysInterface};
use okapi_ledger::{
    BalanceLedger, LimitCaps, ReserveOutcome, ReserveRequest, holds::UserGuard, sync,
};
use std::collections::BTreeMap;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
struct SyncBed {
    base: Bed,
    redis: fred::clients::Client,
    ledger: BalanceLedger,
    input: SettlementInput<'static>,
}
impl SyncBed {
    async fn new(pool: Pool) -> Self {
        let base = bed().await;
        let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
            .await
            .unwrap();
        let ledger = BalanceLedger::new(redis.clone());
        let initial = Money::from_micros(10_000);
        ledger.credit(base.user_id, initial).await.unwrap();
        record_credit(&base.pg, base.user_id, initial, "adjust", "test", json!({}))
            .await
            .unwrap();
        if pool == Pool::Subscription {
            ledger
                .sub_set(base.user_id, initial, chrono::Utc::now().timestamp() + 3600)
                .await
                .unwrap();
            record_sub_event(
                &base.pg,
                base.user_id,
                initial,
                initial,
                "sub_activate",
                "test",
                json!({}),
            )
            .await
            .unwrap();
        }
        let mut input = base.committed(Uuid::new_v4());
        input.pool = pool;
        let outcome = ledger
            .reserve(
                ReserveRequest {
                    user_id: base.user_id,
                    api_key_id: base.key_id,
                    request_id: input.request_id,
                    est: Money::from_micros(1000),
                    est_tokens: 200,
                    caps: LimitCaps::default(),
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(outcome,ReserveOutcome::Reserved { pool: found, .. } if found==pool));
        Self {
            base,
            redis,
            ledger,
            input,
        }
    }
    fn counter(&self) -> String {
        format!("conc:{{{}}}:k:{}", self.base.user_id, self.base.key_id)
    }
    async fn corrupt(&self) {
        self.redis.del::<(), _>(self.counter()).await.unwrap();
        self.redis
            .hset::<(), _, _>(self.counter(), ("bad", "type"))
            .await
            .unwrap();
    }
    async fn restore(&self) {
        self.redis.del::<(), _>(self.counter()).await.unwrap();
        self.redis
            .set::<(), _, _>(self.counter(), "1", None, None, false)
            .await
            .unwrap();
    }
    async fn pending(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM billing_sync WHERE request_id=$1")
            .bind(self.input.request_id)
            .fetch_one(&self.base.pg)
            .await
            .unwrap()
    }
    async fn balance(&self) -> BTreeMap<String, String> {
        self.redis
            .hgetall(format!("bal:{{{}}}", self.base.user_id))
            .await
            .unwrap()
    }
    async fn recover(&self) -> Result<(), okapi_ledger::LedgerError> {
        // Fresh connections/state: recovery cannot rely on the original request.
        let ledger = BalanceLedger::new(self.redis.clone());
        let mut guard = UserGuard::acquire(&self.base.pg, self.base.user_id).await?;
        guard.synchronize(&ledger).await
    }
    async fn check_closed(&self) {
        assert_eq!(self.pending().await, 0);
        assert!(
            self.ledger
                .list_reservations(self.base.user_id)
                .await
                .unwrap()
                .is_empty()
        );
        let value: i64 = self
            .redis
            .hget(
                format!("bal:{{{}}}", self.base.user_id),
                self.input.pool.field(),
            )
            .await
            .unwrap();
        assert_eq!(value, 9760);
        let conc: i64 = self.redis.get(self.counter()).await.unwrap();
        assert_eq!(conc, 0);
        assert_eq!(self.base.events(self.input.request_id).await.len(), 1);
        assert_eq!(self.base.outbox(self.input.request_id).await.len(), 1);
        assert_eq!(self.base.key_used().await.0, 240);
    }
}

#[tokio::test]
async fn pg_first_preserves_usage_until_both_pools_recover_once() -> TestResult {
    for pool in [Pool::Wallet, Pool::Subscription] {
        let b = SyncBed::new(pool).await;
        b.corrupt().await;
        let before = b.balance().await;
        assert!(sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?);
        assert_eq!(b.balance().await, before);
        assert_eq!(b.pending().await, 1);
        let events = b.base.events(b.input.request_id).await;
        assert_eq!(events[0].1, -240);
        assert_eq!(events[0].2, None, "no invented post-Redis balance");
        let outbox = b.base.outbox(b.input.request_id).await;
        assert_eq!(outbox[0].1["prompt_tokens"], 100);
        assert_eq!(outbox[0].1["cache_write_tokens"], 10);
        assert_eq!(outbox[0].1["pool"], pool.as_i16());
        assert!(b.recover().await.is_err());
        assert_eq!(b.pending().await, 1);
        assert!(
            admin_refund(&b.base.pg, b.input.request_id, "test", "test")
                .await
                .is_err()
        );
        b.restore().await;
        b.recover().await?;
        b.recover().await?;
        assert!(!sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?);
        b.check_closed().await;
    }
    Ok(())
}

#[tokio::test]
async fn pg_failure_cannot_close_or_charge_a_reservation() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let before = b.balance().await;
    let mut bad = b.input.clone();
    bad.client_ip = Some("invalid inet");
    assert!(sync::record(&b.base.pg, &b.ledger, bad).await.is_err());
    assert_eq!(b.balance().await, before);
    assert_eq!(b.pending().await, 0);
    assert!(b.base.events(b.input.request_id).await.is_empty());
    assert!(b.base.outbox(b.input.request_id).await.is_empty());
    assert_eq!(b.base.key_used().await.0, 0);
    assert!(sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?);
    b.check_closed().await;
    Ok(())
}

#[tokio::test]
async fn lost_close_ack_rebuilds_without_double_charging_or_losing_live_holds() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    b.corrupt().await;
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    b.restore().await;
    b.ledger
        .commit(
            b.base.user_id,
            b.base.key_id,
            b.input.request_id,
            b.input.amount,
        )
        .await?;
    let other = Uuid::new_v4();
    b.ledger
        .reserve(
            ReserveRequest {
                user_id: b.base.user_id,
                api_key_id: b.base.key_id,
                request_id: other,
                est: Money::from_micros(500),
                est_tokens: 1,
                caps: LimitCaps::default(),
            },
            chrono::Utc::now(),
        )
        .await?;
    // Crash after Redis closed but before PG deleted its recovery row.
    b.recover().await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9260);
    assert_eq!(b.ledger.list_reservations(b.base.user_id).await?.len(), 1);
    b.ledger
        .refund(b.base.user_id, b.base.key_id, other)
        .await?;
    b.recover().await?;
    b.check_closed().await;
    Ok(())
}

#[tokio::test]
async fn credit_finishes_lost_ack_recovery_before_adding_funds() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    b.corrupt().await;
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    b.restore().await;
    b.ledger
        .commit(
            b.base.user_id,
            b.base.key_id,
            b.input.request_id,
            b.input.amount,
        )
        .await?;
    assert_eq!(b.pending().await, 1);
    let after = okapi_ledger::operations::credit(
        &b.base.pg,
        &b.ledger,
        b.base.user_id,
        Money::from_micros(500),
        "adjust",
        "test:credit",
        json!({}),
    )
    .await?;
    assert_eq!(after.balance_after.unwrap().as_micros(), 10_260);
    assert_eq!(b.base.wallet_snapshot().await, 10_260);
    assert_eq!(b.pending().await, 0);
    b.recover().await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_260);
    Ok(())
}

#[tokio::test]
async fn money_operations_reuse_a_single_connection_and_refund_once() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    let pg = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;
    let credit = okapi_ledger::operations::credit(
        &pg,
        &b.ledger,
        b.base.user_id,
        Money::from_micros(500),
        "adjust",
        "test",
        json!({}),
    )
    .await?;
    assert_eq!(credit.balance_after.unwrap().as_micros(), 10_260);
    let first =
        okapi_ledger::operations::refund(&pg, &b.ledger, b.input.request_id, "test", "test")
            .await?;
    assert_eq!(first.unwrap().1.balance_after.unwrap().as_micros(), 10_500);
    assert!(
        okapi_ledger::operations::refund(&pg, &b.ledger, b.input.request_id, "retry", "test")
            .await?
            .is_none()
    );
    for _ in 0..2 {
        okapi_ledger::operations::import_credit(
            &pg,
            Some(&b.ledger),
            b.base.user_id,
            Money::from_micros(700),
            "test:import",
            json!({}),
        )
        .await?;
    }
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 11_200);
    assert_eq!(b.base.wallet_snapshot().await, 11_200);
    assert_eq!(b.base.key_used().await.0, 0);
    sqlx::query("UPDATE users SET balance_expires_at=now()-interval '1 minute' WHERE id=$1")
        .bind(b.base.user_id)
        .execute(&pg)
        .await?;
    let expired =
        okapi_ledger::operations::expire(&pg, &b.ledger, b.base.user_id, chrono::Utc::now())
            .await?;
    assert_eq!(expired.as_micros(), 11_200);
    assert!(
        okapi_ledger::operations::expire(&pg, &b.ledger, b.base.user_id, chrono::Utc::now())
            .await?
            .is_zero()
    );
    assert_eq!(b.base.wallet_snapshot().await, 0);
    assert_eq!(b.ledger.balance(b.base.user_id).await?, Money::ZERO);
    pg.close().await;
    Ok(())
}

#[tokio::test]
async fn debit_drains_available_balance_and_records_negative_adjust() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    // SyncBed 留了一笔 1000 的在途预扣；先释放，让可用额 == 总额，断言口径才干净
    b.ledger
        .refund(b.base.user_id, b.base.key_id, b.input.request_id)
        .await?;
    okapi_ledger::operations::credit(
        &b.base.pg,
        &b.ledger,
        b.base.user_id,
        Money::from_micros(500),
        "adjust",
        "test:credit",
        json!({}),
    )
    .await?;
    // 余额充足：按请求额全扣，PG 事件与热账本同步为负额 adjust
    let (applied, receipt) = okapi_ledger::operations::debit(
        &b.base.pg,
        &b.ledger,
        b.base.user_id,
        Money::from_micros(300),
        "adjust",
        "test:debit",
        json!({}),
    )
    .await?;
    assert_eq!(applied.as_micros(), 300);
    assert_eq!(receipt.unwrap().balance_after.unwrap().as_micros(), 10_200);
    assert_eq!(b.base.wallet_snapshot().await, 10_200);
    let clawed: Vec<i64> = sqlx::query_scalar!(
        r#"SELECT delta_micro AS "delta_micro!" FROM billing_events
           WHERE user_id=$1 AND event_type='adjust' AND delta_micro<0"#,
        b.base.user_id
    )
    .fetch_all(&b.base.pg)
    .await?;
    assert_eq!(
        clawed,
        vec![-300],
        "扣减必须以负额 adjust 留痕（clawed 统计口径）"
    );
    // 余额不足：钳到可用额度，绝不产生负余额
    let (applied, receipt) = okapi_ledger::operations::debit(
        &b.base.pg,
        &b.ledger,
        b.base.user_id,
        Money::from_micros(50_000),
        "adjust",
        "test:debit",
        json!({}),
    )
    .await?;
    assert_eq!(applied.as_micros(), 10_200);
    assert!(receipt.is_some());
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 0);
    assert_eq!(b.base.wallet_snapshot().await, 0);
    // 余额为 0：无事件、无转账
    let (applied, receipt) = okapi_ledger::operations::debit(
        &b.base.pg,
        &b.ledger,
        b.base.user_id,
        Money::from_micros(5),
        "adjust",
        "test:debit",
        json!({}),
    )
    .await?;
    assert!(applied.is_zero());
    assert!(receipt.is_none());
    Ok(())
}

#[tokio::test]
async fn invalid_user_rejects_and_redis_failure_retains_durable_credit() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let absent = -b.base.user_id;
    assert!(
        okapi_ledger::operations::credit(
            &b.base.pg,
            &b.ledger,
            absent,
            Money::from_micros(500),
            "adjust",
            "test",
            json!({})
        )
        .await
        .is_err()
    );
    assert!(
        !b.redis
            .exists::<bool, _>(format!("bal:{{{absent}}}"))
            .await?
    );
    let key = format!("bal:{{{}}}", b.base.user_id);
    let before: String = b.redis.hget(&key, Pool::Wallet.field()).await?;
    b.redis
        .hset::<(), _, _>(&key, (Pool::Wallet.field(), "invalid"))
        .await?;
    let result = okapi_ledger::operations::credit(
        &b.base.pg,
        &b.ledger,
        b.base.user_id,
        Money::from_micros(500),
        "adjust",
        "test:rejected",
        json!({}),
    )
    .await;
    b.redis
        .hset::<(), _, _>(&key, (Pool::Wallet.field(), before))
        .await?;
    assert!(result?.balance_after.is_none());
    assert_eq!(b.base.wallet_snapshot().await, 10_500);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM billing_events WHERE user_id=$1 AND actor='test:rejected'",
    )
    .bind(b.base.user_id)
    .fetch_one(&b.base.pg)
    .await?;
    assert_eq!(count, 1);
    okapi_ledger::transfers::recover_pending(&b.base.pg, &b.ledger, 1000).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_260);
    Ok(())
}

#[tokio::test]
async fn parallel_imports_and_refunds_credit_only_once() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let pg = b.base.pg.clone();
        let ledger = b.ledger.clone();
        let uid = b.base.user_id;
        let request_id = b.input.request_id;
        jobs.spawn(async move {
            okapi_ledger::operations::import_credit(
                &pg,
                Some(&ledger),
                uid,
                Money::from_micros(700),
                "test:concurrent-import",
                json!({}),
            )
            .await?;
            okapi_ledger::operations::refund(&pg, &ledger, request_id, "test", "test").await
        });
    }
    let mut refunds = 0;
    while let Some(result) = jobs.join_next().await {
        refunds += usize::from(result??.is_some());
    }
    assert_eq!(refunds, 1);
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_700);
    assert_eq!(b.base.wallet_snapshot().await, 10_700);
    assert_eq!(b.base.events(b.input.request_id).await.len(), 2);
    assert_eq!(b.base.outbox(b.input.request_id).await.len(), 2);
    assert_eq!(b.base.key_used().await.0, 0);
    Ok(())
}

#[tokio::test]
async fn expiry_rechecks_extension_after_waiting_for_recovery() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    sqlx::query("UPDATE users SET balance_expires_at=now()-interval '1 minute' WHERE id=$1")
        .bind(b.base.user_id)
        .execute(&b.base.pg)
        .await?;
    let mut guard = UserGuard::acquire(&b.base.pg, b.base.user_id).await?;
    let pg = b.base.pg.clone();
    let ledger = b.ledger.clone();
    let uid = b.base.user_id;
    let task = tokio::spawn(async move {
        okapi_ledger::operations::expire(&pg, &ledger, uid, chrono::Utc::now()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            assert!(!task.is_finished(), "expiry must wait for recovery");
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid=$1::bigint::oid AND objid=(hashtext($2)::bigint & 4294967295)::oid)"
            ).bind(okapi_store::image_batches::HOLD_LOCK_NAMESPACE).bind(uid.to_string())
                .fetch_one(&b.base.pg).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await?;
    sqlx::query("UPDATE users SET balance_expires_at=now()+interval '1 day' WHERE id=$1")
        .bind(uid)
        .execute(guard.connection()?)
        .await?;
    drop(guard);
    assert!(task.await??.is_zero());
    assert_eq!(b.ledger.balance(uid).await?.as_micros(), 9000);
    assert_eq!(b.base.wallet_snapshot().await, 10_000);
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    b.check_closed().await;
    Ok(())
}

#[tokio::test]
async fn lost_redis_balance_recovers_from_durable_actual_charge() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    b.corrupt().await;
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    b.redis
        .del::<(), _>(format!("bal:{{{}}}", b.base.user_id))
        .await?;
    b.redis.del::<(), _>(b.counter()).await?;
    b.recover().await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9760);
    assert_eq!(b.pending().await, 0);
    assert_eq!(b.base.key_used().await.0, 240);
    assert_eq!(b.base.events(b.input.request_id).await.len(), 1);
    Ok(())
}

#[tokio::test]
async fn concurrent_completion_and_conflicting_replays_do_not_double_bill() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let mut joins = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let pg = b.base.pg.clone();
        let ledger = b.ledger.clone();
        let input = b.input.clone();
        joins.spawn(async move { sync::record(&pg, &ledger, input).await });
    }
    let mut inserted = 0;
    while let Some(result) = joins.join_next().await {
        inserted += usize::from(result??);
    }
    assert_eq!(inserted, 1);
    for conflict in 0..3 {
        let mut input = b.input.clone();
        match conflict {
            0 => {
                input.amount = Money::from_micros(241);
                input.delta_micro = -241;
            }
            1 => input.pool = Pool::Subscription,
            _ => input.api_key_id += 1,
        }
        assert!(sync::record(&b.base.pg, &b.ledger, input).await.is_err());
    }
    b.check_closed().await;
    Ok(())
}

#[tokio::test]
async fn wrong_pool_is_rejected_before_closing_the_redis_receipt() -> TestResult {
    let b = SyncBed::new(Pool::Subscription).await;
    let before = b.balance().await;
    assert!(
        b.ledger
            .commit_in_pool(
                b.base.user_id,
                b.base.key_id,
                b.input.request_id,
                b.input.amount,
                Pool::Wallet
            )
            .await
            .is_err()
    );
    assert_eq!(b.balance().await, before);
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    b.check_closed().await;
    Ok(())
}

async fn queue_credit(b: &SyncBed, amount: i64) -> TestResultWithId {
    use sqlx::Connection as _;
    let mut guard = UserGuard::acquire(&b.base.pg, b.base.user_id).await?;
    let mut tx = guard.connection()?.begin().await?;
    let id = okapi_ledger::transfers::credit_in_tx(
        &mut tx,
        b.base.user_id,
        Money::from_micros(amount),
        "adjust",
        "test:fund-recovery",
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}
type TestResultWithId = Result<Uuid, Box<dyn std::error::Error + Send + Sync>>;

async fn apply_transfer(b: &SyncBed, id: Uuid, amount: i64, pool: Pool) -> String {
    use fred::interfaces::LuaInterface as _;
    let sequence: i64 = sqlx::query_scalar("SELECT sequence FROM fund_transfers WHERE id=$1")
        .bind(id)
        .fetch_one(&b.base.pg)
        .await
        .unwrap();
    b.redis
        .eval(
            include_str!("../../src/lua/fund_transfer.lua"),
            vec![format!("bal:{{{}}}", b.base.user_id)],
            vec![
                id.to_string(),
                amount.to_string(),
                pool.field().to_owned(),
                sequence.to_string(),
            ],
        )
        .await
        .unwrap()
}
async fn recover_transfers(b: &SyncBed) -> TestResult {
    let fresh =
        BalanceLedger::new(okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?);
    let mut guard = UserGuard::acquire(&b.base.pg, b.base.user_id).await?;
    guard.synchronize(&fresh).await?;
    Ok(())
}

#[tokio::test]
async fn transfer_lost_redis_ack_replays_once_and_cleans_receipt() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let id = queue_credit(&b, 500).await?;
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    recover_transfers(&b).await?;
    recover_transfers(&b).await?;
    let state: (bool, bool) = sqlx::query_as(
        "SELECT applied_at IS NOT NULL,cleaned_at IS NOT NULL FROM fund_transfers WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&b.base.pg)
    .await?;
    assert_eq!(state, (true, true));
    let receipt: Option<String> = b
        .redis
        .hget(format!("bal:{{{}}}", b.base.user_id), format!("c:{id}"))
        .await?;
    assert!(receipt.is_none());
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_260);
    Ok(())
}

#[tokio::test]
async fn transfer_cleanup_after_pg_ack_never_reapplies() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let id = queue_credit(&b, 500).await?;
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    sqlx::query("UPDATE fund_transfers SET applied_at=now() WHERE id=$1")
        .bind(id)
        .execute(&b.base.pg)
        .await?;
    // Crash after receipt deletion but before PG recorded cleanup.
    b.redis
        .hdel::<(), _, _>(format!("bal:{{{}}}", b.base.user_id), format!("c:{id}"))
        .await?;
    recover_transfers(&b).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    Ok(())
}

#[tokio::test]
async fn missing_balance_recovers_accepted_credit_from_pg_snapshot() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    b.ledger
        .refund(b.base.user_id, b.base.key_id, b.input.request_id)
        .await?;
    let id = queue_credit(&b, 500).await?;
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    // Even with historical event retention, the wallet snapshot retains old money.
    sqlx::query("DELETE FROM billing_events WHERE user_id=$1 AND actor='test'")
        .bind(b.base.user_id)
        .execute(&b.base.pg)
        .await?;
    b.redis
        .del::<(), _>(format!("bal:{{{}}}", b.base.user_id))
        .await?;
    recover_transfers(&b).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_500);
    recover_transfers(&b).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_500);
    Ok(())
}

#[tokio::test]
async fn balance_repair_includes_pending_credits_without_double_application() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    queue_credit(&b, 500).await?;
    queue_credit(&b, 700).await?;
    let mut guard = UserGuard::acquire(&b.base.pg, b.base.user_id).await?;
    guard
        .repair(&b.ledger, Money::from_micros(11_200), Money::ZERO)
        .await?;
    drop(guard);
    recover_transfers(&b).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_200);
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_960);
    Ok(())
}

#[tokio::test]
async fn conflicting_transfer_receipt_and_unsafe_balance_fail_before_writes() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let id = queue_credit(&b, 500).await?;
    let key = format!("bal:{{{}}}", b.base.user_id);
    let field = format!("c:{id}");
    b.redis
        .hset::<(), _, _>(&key, (&field, "avail|501"))
        .await?;
    let conflict = recover_transfers(&b).await;
    let before = b.ledger.balance(b.base.user_id).await?;
    b.redis.hdel::<(), _, _>(&key, &field).await?;
    b.redis
        .hset::<(), _, _>(&key, ("avail", okapi_ledger::holds::MAXIMUM_MICROS))
        .await?;
    let overflow = apply_transfer(&b, id, 500, Pool::Wallet).await;
    let untouched = b.ledger.balance(b.base.user_id).await?;
    let marker: Option<String> = b.redis.hget(&key, &field).await?;
    // Restore the injected corruption before making assertions.
    b.redis
        .hset::<(), _, _>(&key, ("avail", before.as_micros()))
        .await?;
    assert!(conflict.is_err());
    assert_eq!(before.as_micros(), 9_000);
    assert_eq!(overflow, "invalid");
    assert_eq!(untouched.as_micros(), okapi_ledger::holds::MAXIMUM_MICROS);
    assert!(marker.is_none());
    recover_transfers(&b).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    Ok(())
}

#[tokio::test]
async fn subscription_refund_lost_ack_recovers_only_original_pool() -> TestResult {
    use sqlx::Connection as _;
    let b = SyncBed::new(Pool::Subscription).await;
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    let mut guard = UserGuard::acquire(&b.base.pg, b.base.user_id).await?;
    let mut tx = guard.connection()?.begin().await?;
    let refund =
        okapi_ledger::pg::admin_refund_in_tx(&mut tx, b.input.request_id, "failure", "test")
            .await?
            .unwrap();
    let id = okapi_ledger::transfers::enqueue(&mut tx, b.base.user_id, refund.amount, refund.pool)
        .await?;
    tx.commit().await?;
    drop(guard);
    assert_eq!(
        apply_transfer(&b, id, 240, Pool::Subscription).await,
        "applied"
    );
    recover_transfers(&b).await?;
    assert_eq!(
        b.ledger.sub_balance(b.base.user_id).await?.0.as_micros(),
        10_000
    );
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 10_000);
    assert!(
        okapi_ledger::operations::refund(
            &b.base.pg,
            &b.ledger,
            b.input.request_id,
            "again",
            "test"
        )
        .await?
        .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn delayed_transfer_after_receipt_cleanup_cannot_credit_again() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let id = queue_credit(&b, 500).await?;
    recover_transfers(&b).await?;
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    // The original connection's timed-out EVAL arrives only after a new worker
    // has acknowledged the operation and removed its c:* receipt.
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    // A full repair also retains the sequence of already-cleaned transfers.
    let mut guard = UserGuard::acquire(&b.base.pg, b.base.user_id).await?;
    guard
        .repair(&b.ledger, Money::from_micros(10_500), Money::ZERO)
        .await?;
    drop(guard);
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    Ok(())
}

#[tokio::test]
async fn transfer_watermark_preserves_full_bigint_precision() -> TestResult {
    let b = SyncBed::new(Pool::Wallet).await;
    let id = queue_credit(&b, 500).await?;
    let high = i64::MAX - b.base.user_id;
    sqlx::query("UPDATE fund_transfers SET sequence=$2 WHERE id=$1")
        .bind(id)
        .bind(high)
        .execute(&b.base.pg)
        .await?;
    let key = format!("bal:{{{}}}", b.base.user_id);
    b.redis
        .hset::<(), _, _>(&key, ("fund_seq", (high - 1).to_string()))
        .await?;
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    let sequence: String = b.redis.hget(&key, "fund_seq").await?;
    assert_eq!(sequence, high.to_string());
    recover_transfers(&b).await?;
    assert_eq!(apply_transfer(&b, id, 500, Pool::Wallet).await, "applied");
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9_500);
    Ok(())
}

#[tokio::test]
async fn delayed_balance_repair_cannot_erase_a_newly_committed_charge() -> TestResult {
    use fred::interfaces::LuaInterface;
    let b = SyncBed::new(Pool::Wallet).await;
    let key = format!("bal:{{{}}}", b.base.user_id);
    let expected: String = b
        .redis
        .eval(
            concat!(
                include_str!("../../src/lua/balance_state.lua"),
                "\nreturn ledger_balance_state(KEYS[1])"
            ),
            vec![key.clone()],
            Vec::<String>::new(),
        )
        .await?;
    sync::record(&b.base.pg, &b.ledger, b.input.clone()).await?;
    let before = b.balance().await;
    let result: String = b
        .redis
        .eval(
            concat!(
                include_str!("../../src/lua/balance_state.lua"),
                "\n",
                include_str!("../../src/lua/hold_repair.lua")
            ),
            vec![key],
            vec![
                "10000".to_owned(),
                "0".to_owned(),
                "[]".to_owned(),
                String::new(),
                "0".to_owned(),
                "[]".to_owned(),
                "0".to_owned(),
                expected,
            ],
        )
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result)?["error"],
        "recovery_required"
    );
    assert_eq!(b.balance().await, before);
    assert_eq!(b.ledger.balance(b.base.user_id).await?.as_micros(), 9760);
    Ok(())
}
