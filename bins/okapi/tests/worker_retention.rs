//! Destructive retention tests use a fresh database per case. Redis users have
//! disjoint IDs so no other suite's balances or recovery evidence are touched.
use chrono::Utc;
use okapi::{console, gateway, worker};
use okapi_domain::{BillingState, Money, TokenUsage};
use okapi_ledger::{BalanceLedger, LimitCaps, Pool, ReserveRequest, SettlementInput};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct Bed {
    cleanup_url: String,
    database_name: String,
    pg: PgPool,
    ledger: BalanceLedger,
    redis: fred::clients::Client,
    uid: i64,
    kid: i64,
    user_token: String,
    admin_token: String,
    addr: SocketAddr,
}
fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}
async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database = std::env::var("DATABASE_URL").unwrap();
    let cleanup_url = database.clone();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let suffix = Uuid::new_v4();
    let name = format!("okapi_retention_case_{}", suffix.simple());
    let root = okapi_store::connect_pg(&database).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&root)
        .await
        .unwrap();
    root.close().await;
    let mut url = reqwest::Url::parse(&database).unwrap();
    url.set_path(&name);
    let database = url.to_string();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let random = u64::from_be_bytes(suffix.as_bytes()[..8].try_into().unwrap());
    let first = i64::try_from(random & ((1_u64 << 47) - 1)).unwrap() + 1_000_000_000_000;
    sqlx::query("SELECT setval('users_id_seq',$1,false)")
        .bind(first)
        .execute(&pg)
        .await
        .unwrap();
    let uid = okapi_store::provision::create_user(&pg, "retention-user")
        .await
        .unwrap();
    let user_token = format!("sk-retention-user-{suffix}");
    let kid = okapi_store::provision::create_api_key(&pg, uid, &hash(&user_token), "sk-retention")
        .await
        .unwrap();
    let admin = okapi_store::provision::create_user(&pg, "retention-admin")
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(admin)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-retention-admin-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin, &hash(&admin_token), "sk-rtn-admin")
        .await
        .unwrap();
    let state = gateway::build_state(&database, &redis_url, "test-retention", None, None)
        .await
        .unwrap();
    let ledger = state.ledger.clone();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = console::router(state);
    // Test server is scoped to this test's Tokio runtime.
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    for table in ["billing_records", "billing_events", "audit_logs"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TABLE {table}_y2020m01 PARTITION OF {table} FOR VALUES FROM ('2020-01-01 00:00:00+00') TO ('2020-02-01 00:00:00+00')"))).execute(&pg).await.unwrap();
    }
    sqlx::query("INSERT INTO settings(key,value) VALUES ('retention_months','12') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").execute(&pg).await.unwrap();
    Bed {
        cleanup_url,
        database_name: name,
        pg,
        ledger,
        redis,
        uid,
        kid,
        user_token,
        admin_token,
        addr,
    }
}
// Each case owns its UUID database. Release it even when the assertion panics;
// scoped HTTP test tasks may still hold idle connections until runtime shutdown.
impl Drop for Bed {
    fn drop(&mut self) {
        let url = self.cleanup_url.clone();
        let database = self.database_name.clone();
        let result = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async {
                let pg = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&url)
                    .await
                    .map_err(|error| error.to_string())?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP DATABASE {database} WITH (FORCE)"
                )))
                .execute(&pg)
                .await
                .map_err(|error| error.to_string())?;
                pg.close().await;
                Ok(())
            })
        })
        .join();
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(?result, "isolated retention test database cleanup failed");
        }
    }
}

impl Bed {
    async fn credit(&self, amount: i64, actor: &str) {
        let receipt = okapi_ledger::operations::credit(
            &self.pg,
            &self.ledger,
            self.uid,
            Money::from_micros(amount),
            "adjust",
            actor,
            json!({}),
        )
        .await
        .unwrap();
        assert!(receipt.balance_after.is_some());
    }
    async fn age(&self) {
        sqlx::query(
            "UPDATE billing_events SET created_at='2020-01-15 00:00:00+00' WHERE user_id=$1",
        )
        .bind(self.uid)
        .execute(&self.pg)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE billing_records SET created_at='2020-01-15 00:00:00+00' WHERE user_id=$1",
        )
        .bind(self.uid)
        .execute(&self.pg)
        .await
        .unwrap();
    }
    async fn prune(&self) -> Vec<String> {
        worker::drop_expired_partitions(&self.pg, Utc::now())
            .await
            .unwrap()
    }
    async fn repair(&self) -> Value {
        let response = reqwest::Client::new()
            .post(format!("http://{}/admin/reconciliation/repair", self.addr))
            .bearer_auth(&self.admin_token)
            .json(&json!({"user_id":self.uid}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }
    fn bill(&self) -> SettlementInput<'static> {
        SettlementInput {
            source_window: None,
            dimensions: okapi_ledger::pg::UsageDimensions::new(
                "test",
                "test",
                "/v1/chat/completions",
                "/v1/chat/completions",
            ),
            request_id: Uuid::new_v4(),
            log_type: 2,
            user_id: self.uid,
            api_key_id: self.kid,
            group_code: "default",
            model_name: "test",
            channel_id: None,
            channel_key_id: None,
            state: BillingState::Committed,
            usage: TokenUsage::default(),
            amount: Money::from_micros(240),
            original: Money::from_micros(300),
            discount: Money::from_micros(60),
            list_price: Money::from_micros(300),
            upstream_cost: Some(Money::from_micros(150)),
            pricing_epoch: None,
            pricing_snapshot: None,
            latency_ms: 1,
            ttft_ms: None,
            is_stream: false,
            retry_count: 0,
            failover_count: 0,
            upstream_status: Some(200),
            error_code: None,
            upstream_request_id: None,
            node: "retention-test",
            sticky_layer: 0,
            client_type: "test",
            client_ip: None,
            delta_micro: -240,
            balance_after: None,
            event_type: "commit",
            pool: Pool::Wallet,
        }
    }
    async fn subscribe(&self) {
        let plan_id = okapi_store::admin::create_plan(
            &self.pg,
            &okapi_store::admin::PlanSpec {
                plan_code: "retention-plan",
                display_name: "Retention",
                kind: 1,
                grant_micro: 3_000_000,
                group_code: None,
                balance_valid_days: None,
                price_micro: 1_000_000,
                period: Some(1),
                duration_days: Some(30),
                sort_order: 0,
                description: None,
            },
        )
        .await
        .unwrap();
        let plan = okapi_store::subscriptions::sub_plan_by_id(&self.pg, plan_id)
            .await
            .unwrap()
            .unwrap();
        okapi_ledger::subscriptions::grant(
            &self.pg,
            &self.ledger,
            self.uid,
            &plan,
            "test:retention",
            "test",
        )
        .await
        .unwrap();
    }
    async fn settle(&self) -> SettlementInput<'static> {
        self.credit(9_000_000, "test").await;
        let bill = self.bill();
        self.ledger
            .reserve(
                ReserveRequest {
                    user_id: self.uid,
                    api_key_id: self.kid,
                    request_id: bill.request_id,
                    est: Money::from_micros(1000),
                    caps: LimitCaps::default(),
                    est_tokens: 10,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(
            okapi_ledger::sync::record(&self.pg, &self.ledger, bill.clone())
                .await
                .unwrap()
        );
        bill
    }
}

#[tokio::test]
async fn pruned_events_still_repair_both_balances_and_preserve_inflight() {
    let b = setup().await;
    b.credit(9_000_000, "test").await;
    // Reserve wallet first; enabling a subscription afterwards must retain it.
    b.ledger
        .reserve(
            ReserveRequest {
                user_id: b.uid,
                api_key_id: b.kid,
                request_id: Uuid::new_v4(),
                est: Money::from_micros(100_000),
                caps: LimitCaps::default(),
                est_tokens: 1,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    b.subscribe().await;
    b.age().await;
    let dropped = b.prune().await;
    assert!(dropped.iter().any(|name| name == "billing_events_y2020m01"));
    let drifts = worker::reconcile_balances(&b.pg, &b.ledger, 100)
        .await
        .unwrap();
    b.ledger.drain(b.uid).await.unwrap();
    b.ledger.sub_set(b.uid, Money::ZERO, 0).await.unwrap();
    let result = b.repair().await;
    assert_eq!(
        result["data"][0]["redis_after_micro"], 8_900_000,
        "pruning history must not erase wallet credits"
    );
    assert_eq!(result["data"][0]["sub_redis_after_micro"], 3_000_000);
    assert_eq!(result["data"][0]["inflight_micro"], 100_000);
    assert!(
        !drifts.iter().any(|d| d.user_id == b.uid),
        "retention must not create false drift"
    );
    assert!(b.prune().await.is_empty());
    let again = b.repair().await;
    assert_eq!(again["data"][0]["redis_after_micro"], 8_900_000);
}

#[tokio::test]
async fn retention_only_drops_owned_partitions_with_matching_time_bounds() {
    let b = setup().await;
    sqlx::query("CREATE TABLE billing_events_y2019m01 (data text)")
        .execute(&b.pg)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE audit_logs_y2019m02 PARTITION OF audit_logs FOR VALUES FROM ('2100-02-01 00:00:00+00') TO ('2100-03-01 00:00:00+00')").execute(&b.pg).await.unwrap();
    b.prune().await;
    let standalone: bool =
        sqlx::query_scalar("SELECT to_regclass('billing_events_y2019m01') IS NOT NULL")
            .fetch_one(&b.pg)
            .await
            .unwrap();
    let future: bool = sqlx::query_scalar("SELECT to_regclass('audit_logs_y2019m02') IS NOT NULL")
        .fetch_one(&b.pg)
        .await
        .unwrap();
    assert!(
        standalone,
        "a matching name is not authorization to drop an unrelated table"
    );
    assert!(
        future,
        "the actual partition boundary must agree with its month name"
    );
}

#[tokio::test]
async fn retained_actor_totals_prevent_double_import_and_preserve_affiliate_summary() {
    let b = setup().await;
    okapi_ledger::operations::import_credit(
        &b.pg,
        Some(&b.ledger),
        b.uid,
        Money::from_micros(1_000_000),
        "import:retention",
        json!({}),
    )
    .await
    .unwrap();
    b.credit(2_000_000, "system:aff").await;
    b.age().await;
    b.prune().await;
    okapi_ledger::operations::import_credit(
        &b.pg,
        Some(&b.ledger),
        b.uid,
        Money::from_micros(1_000_000),
        "import:retention",
        json!({}),
    )
    .await
    .unwrap();
    let response = reqwest::Client::new()
        .get(format!("http://{}/api/me/aff", b.addr))
        .bearer_auth(&b.user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let aff: Value = response.json().await.unwrap();
    assert_eq!(
        b.ledger.balance(b.uid).await.unwrap().as_micros(),
        3_000_000,
        "retention must preserve the import replay barrier"
    );
    assert_eq!(aff["reward_sum_micro"], 2_000_000);
}

#[tokio::test]
async fn pruned_bill_still_blocks_settlement_replay() {
    let b = setup().await;
    let bill = b.settle().await;
    b.age().await;
    b.prune().await;
    let inserted = okapi_ledger::sync::record(&b.pg, &b.ledger, bill)
        .await
        .unwrap();
    assert!(
        !inserted,
        "deleting detailed logs must not permit another charge for the same request"
    );
    assert_eq!(
        b.ledger.balance(b.uid).await.unwrap().as_micros(),
        8_999_760
    );
}

#[tokio::test]
async fn historical_refund_uses_original_amount_and_is_idempotent_after_retention() {
    let b = setup().await;
    let bill = b.settle().await;
    b.age().await;
    b.prune().await;
    let request = || {
        reqwest::Client::new()
            .post(format!("http://{}/admin/billing/refund", b.addr))
            .bearer_auth(&b.admin_token)
            .json(&json!({"request_id":bill.request_id,"reason":"retained financial receipt"}))
    };
    let response = request().send().await.unwrap();
    assert_eq!(
        response.status(),
        200,
        "retention must preserve the financial facts required to refund"
    );
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["refunded_micro"], 240);
    assert_eq!(
        b.ledger.balance(b.uid).await.unwrap().as_micros(),
        9_000_000
    );
    let replay = request().send().await.unwrap();
    assert_eq!(replay.status(), 200);
    assert_eq!(
        replay.json::<Value>().await.unwrap()["outcome"],
        "already_refunded"
    );
    let result = b.repair().await;
    assert_eq!(result["data"][0]["redis_after_micro"], 9_000_000);
    let facts:Value=sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.refunded' AND payload->>'request_id'=$1")
        .bind(bill.request_id.to_string()).fetch_one(&b.pg).await.unwrap();
    assert_eq!(facts["amount_micro"], -240);
    assert_eq!(facts["original_amount_micro"], -300);
    assert_eq!(facts["discount_micro"], -60);
    assert_eq!(facts["upstream_cost_micro"], -150);
}

#[tokio::test]
async fn archived_subscription_refund_returns_to_the_original_pool() {
    let b = setup().await;
    b.credit(9_000_000, "test").await;
    b.subscribe().await;
    let mut bill = b.bill();
    bill.pool = Pool::Subscription;
    b.ledger
        .reserve(
            ReserveRequest {
                user_id: b.uid,
                api_key_id: b.kid,
                request_id: bill.request_id,
                est: Money::from_micros(1000),
                caps: LimitCaps::default(),
                est_tokens: 1,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(
        okapi_ledger::sync::record(&b.pg, &b.ledger, bill.clone())
            .await
            .unwrap()
    );
    b.age().await;
    b.prune().await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/admin/billing/refund", b.addr))
        .bearer_auth(&b.admin_token)
        .json(&json!({"request_id":bill.request_id,"reason":"sub receipt"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["refunded_micro"],
        240
    );
    assert_eq!(
        b.ledger.sub_balance(b.uid).await.unwrap().0.as_micros(),
        3_000_000
    );
    assert_eq!(
        b.ledger.balance(b.uid).await.unwrap().as_micros(),
        9_000_000
    );
    assert!(matches!(
        okapi_ledger::sync::record(&b.pg, &b.ledger, bill).await,
        Err(okapi_ledger::LedgerError::ReservationConflict)
    ));
    let repair = b.repair().await;
    assert_eq!(repair["data"][0]["sub_redis_after_micro"], 3_000_000);
    assert_eq!(repair["data"][0]["redis_after_micro"], 9_000_000);
}

#[tokio::test]
async fn failed_drop_rolls_back_carry_and_retry_is_exact() {
    let b = setup().await;
    b.credit(9_000_000, "test").await;
    b.age().await;
    sqlx::query("CREATE VIEW retention_dependency AS SELECT * FROM billing_events_y2020m01")
        .execute(&b.pg)
        .await
        .unwrap();
    let failure = worker::drop_expired_partitions(&b.pg, Utc::now()).await;
    let carry: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_event_carry")
        .fetch_one(&b.pg)
        .await
        .unwrap();
    let live: i64 =
        sqlx::query_scalar("SELECT SUM(delta_micro)::bigint FROM billing_events WHERE user_id=$1")
            .bind(b.uid)
            .fetch_one(&b.pg)
            .await
            .unwrap();
    sqlx::query("DROP VIEW retention_dependency")
        .execute(&b.pg)
        .await
        .unwrap();
    assert!(
        failure.is_err(),
        "dependent objects must not be dropped via CASCADE"
    );
    assert_eq!(carry, 0, "a failed DROP must roll back its preceding carry");
    assert_eq!(live, 9_000_000);
    b.prune().await;
    let repair = b.repair().await;
    assert_eq!(repair["data"][0]["redis_after_micro"], 9_000_000);
}

#[tokio::test]
async fn concurrent_pruners_carry_each_partition_once() {
    let b = setup().await;
    b.settle().await;
    b.age().await;
    let (left, right) = tokio::join!(
        worker::drop_expired_partitions(&b.pg, Utc::now()),
        worker::drop_expired_partitions(&b.pg, Utc::now())
    );
    let mut dropped = left.unwrap();
    dropped.extend(right.unwrap());
    for table in [
        "billing_events_y2020m01",
        "billing_records_y2020m01",
        "audit_logs_y2020m01",
    ] {
        assert_eq!(dropped.iter().filter(|s| *s == table).count(), 1);
    }
    let (events,delta):(i64,i64)=sqlx::query_as("SELECT SUM(event_count)::bigint,SUM(delta_micro)::bigint FROM billing_event_carry WHERE user_id=$1")
        .bind(b.uid).fetch_one(&b.pg).await.unwrap();
    assert_eq!(events, 2);
    assert_eq!(delta, 8_999_760);
    let receipts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM billing_record_receipts WHERE user_id=$1")
            .bind(b.uid)
            .fetch_one(&b.pg)
            .await
            .unwrap();
    assert_eq!(receipts, 1);
    assert_eq!(b.repair().await["data"][0]["redis_after_micro"], 8_999_760);
}

#[tokio::test]
async fn history_reader_fences_drop_until_its_financial_snapshot_is_read() {
    let b = setup().await;
    b.credit(9_000_000, "test").await;
    b.age().await;
    let mut reader = okapi_store::history::read(&b.pg).await.unwrap();
    let pg = b.pg.clone();
    let prune = tokio::spawn(async move { worker::drop_expired_partitions(&pg, Utc::now()).await });
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory')")
                .fetch_one(&b.pg).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let before: i64 = sqlx::query_scalar(
        "SELECT delta_micro FROM billing_balance_totals WHERE user_id=$1 AND pool=0",
    )
    .bind(b.uid)
    .fetch_one(&mut *reader)
    .await
    .unwrap();
    reader.commit().await.unwrap();
    assert!(!prune.await.unwrap().unwrap().is_empty());
    let after = okapi_store::history::totals(&mut b.pg.acquire().await.unwrap(), b.uid)
        .await
        .unwrap();
    assert_eq!(before, 9_000_000);
    assert_eq!(after.wallet, before);
}

#[tokio::test]
async fn retention_handles_calendar_boundaries_and_disabled_or_extreme_values() {
    let b = setup().await;
    for setting in [0, -1, i64::MAX] {
        sqlx::query("UPDATE settings SET value=to_jsonb($1::bigint) WHERE key='retention_months'")
            .bind(setting)
            .execute(&b.pg)
            .await
            .unwrap();
        assert!(b.prune().await.is_empty());
    }
    sqlx::query("UPDATE settings SET value='2' WHERE key='retention_months'")
        .execute(&b.pg)
        .await
        .unwrap();
    let feb = "2020-02-29T23:59:59Z".parse().unwrap();
    assert!(
        worker::drop_expired_partitions(&b.pg, feb)
            .await
            .unwrap()
            .is_empty()
    );
    let march = "2020-03-01T00:00:00Z".parse().unwrap();
    let dropped = worker::drop_expired_partitions(&b.pg, march).await.unwrap();
    assert_eq!(dropped.len(), 3);
}

#[tokio::test]
async fn zero_net_import_actor_still_blocks_reimport_after_retention() {
    let b = setup().await;
    b.credit(1000, "import:net-zero").await;
    b.credit(-1000, "import:net-zero").await;
    b.age().await;
    b.prune().await;
    okapi_ledger::operations::import_credit(
        &b.pg,
        Some(&b.ledger),
        b.uid,
        Money::from_micros(1000),
        "import:net-zero",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(b.ledger.balance(b.uid).await.unwrap().as_micros(), 0);
    let marker: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM billing_actor_totals WHERE user_id=$1 AND actor='import:net-zero'",
    )
    .bind(b.uid)
    .fetch_one(&b.pg)
    .await
    .unwrap();
    assert_eq!(marker, 1);
}

#[tokio::test]
async fn cold_balance_and_missing_settlement_receipt_include_carried_subscription() {
    use fred::interfaces::KeysInterface;
    let b = setup().await;
    b.credit(9_000_000, "test").await;
    b.subscribe().await;
    b.age().await;
    b.prune().await;
    b.redis
        .del::<(), _>(format!("bal:{{{}}}", b.uid))
        .await
        .unwrap();
    // Accepted top-up rebuilds a missing whole hash, including the old subscription.
    b.credit(1000, "test:new").await;
    assert_eq!(
        b.ledger.balance(b.uid).await.unwrap().as_micros(),
        9_001_000
    );
    assert_eq!(
        b.ledger.sub_balance(b.uid).await.unwrap().0.as_micros(),
        3_000_000
    );
    let mut bill = b.bill();
    bill.pool = Pool::Subscription;
    // No Redis reservation receipt: durable settlement must reconstruct both pools.
    assert!(
        okapi_ledger::sync::record(&b.pg, &b.ledger, bill)
            .await
            .unwrap()
    );
    assert_eq!(
        b.ledger.balance(b.uid).await.unwrap().as_micros(),
        9_001_000
    );
    assert_eq!(
        b.ledger.sub_balance(b.uid).await.unwrap().0.as_micros(),
        2_999_760
    );
}

#[tokio::test]
async fn conflicting_financial_receipt_prevents_deleting_the_live_bill() {
    let b = setup().await;
    let bill = b.settle().await;
    b.age().await;
    sqlx::query("INSERT INTO billing_record_receipts(request_id,user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,status,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,node,pool,pricing_snapshot,usage_details,created_at)
        SELECT request_id,user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,status,amount_micro+1,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,node,pool,pricing_snapshot,usage_details,created_at FROM billing_records WHERE request_id=$1")
        .bind(bill.request_id).execute(&b.pg).await.unwrap();
    let failure = worker::drop_expired_partitions(&b.pg, Utc::now()).await;
    let live: i64 =
        sqlx::query_scalar("SELECT amount_micro FROM billing_records WHERE request_id=$1")
            .bind(bill.request_id)
            .fetch_one(&b.pg)
            .await
            .unwrap();
    let conflicting: i64 =
        sqlx::query_scalar("SELECT amount_micro FROM billing_record_receipts WHERE request_id=$1")
            .bind(bill.request_id)
            .fetch_one(&b.pg)
            .await
            .unwrap();
    // Remove only this test's injected corrupt receipt before retrying maintenance.
    sqlx::query("DELETE FROM billing_record_receipts WHERE request_id=$1")
        .bind(bill.request_id)
        .execute(&b.pg)
        .await
        .unwrap();
    assert!(failure.is_err());
    assert_eq!(live, 240);
    assert_eq!(conflicting, 241);
    b.prune().await;
    let carried: i64 =
        sqlx::query_scalar("SELECT amount_micro FROM billing_record_receipts WHERE request_id=$1")
            .bind(bill.request_id)
            .fetch_one(&b.pg)
            .await
            .unwrap();
    assert_eq!(carried, 240);
    assert_eq!(b.repair().await["data"][0]["redis_after_micro"], 8_999_760);
}

#[tokio::test]
async fn opposing_lifetime_totals_do_not_overflow_before_net_balance_is_calculated() {
    let b = setup().await;
    // Each historical change is a valid Redis integer; lifetime credits and
    // debits individually exceed bigint, while the real balance remains 500.
    for (actor, amount) in [
        ("test:credit", 9_007_199_254_740_991_i64),
        ("test:debit", -9_007_199_254_740_991_i64),
    ] {
        sqlx::query("INSERT INTO billing_events(user_id,event_type,delta_micro,actor,pool) SELECT $1,'adjust',$2,$3,0 FROM generate_series(1,1025)")
            .bind(b.uid).bind(amount).bind(actor).execute(&b.pg).await.unwrap();
    }
    sqlx::query("INSERT INTO billing_events(user_id,event_type,delta_micro,actor,pool) VALUES($1,'adjust',500,'test:net',0)")
        .bind(b.uid).execute(&b.pg).await.unwrap();
    let before = okapi_store::history::totals(&mut b.pg.acquire().await.unwrap(), b.uid)
        .await
        .unwrap();
    b.age().await;
    b.prune().await;
    let after = okapi_store::history::totals(&mut b.pg.acquire().await.unwrap(), b.uid)
        .await
        .unwrap();
    assert_eq!(before.wallet, 500);
    assert_eq!(after.wallet, 500);
    let large:bool=sqlx::query_scalar("SELECT delta_micro > 9223372036854775807::numeric FROM billing_event_carry WHERE user_id=$1 AND actor='test:credit'")
        .bind(b.uid).fetch_one(&b.pg).await.unwrap();
    assert!(large);
    assert_eq!(b.repair().await["data"][0]["redis_after_micro"], 500);
}

#[tokio::test]
async fn delivery_cleanup_preserves_pending_recent_and_dlq_batches() {
    dotenvy::dotenv().ok();
    let bed = setup().await;
    let mut ids = Vec::new();
    for (status, age, dlq) in [
        (1_i16, 14, false),
        (0, 14, false),
        (1, 0, false),
        (1, 14, true),
    ] {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO billing_ch_batches(id,status,event_count,rows,payloads,completed_at) VALUES($1,$2,1,'[{}]','[{}]',$3)").bind(id).bind(status).bind(Utc::now()-chrono::Duration::days(age)).execute(&bed.pg).await.unwrap();
        sqlx::query("INSERT INTO billing_ch_events(event_key,batch_id) VALUES($1,$2)")
            .bind(id.to_string())
            .bind(id)
            .execute(&bed.pg)
            .await
            .unwrap();
        sqlx::query("INSERT INTO billing_outbox(topic,payload,status,ch_batch_id) VALUES('billing.completed','{}',$1,$2)").bind(status).bind(id).execute(&bed.pg).await.unwrap();
        if dlq {
            sqlx::query("INSERT INTO billing_dlq(source,payload,error,retry_count,ch_batch_id) VALUES('chsink','{}','test',1,$1)").bind(id).execute(&bed.pg).await.unwrap();
        }
        ids.push(id);
    }
    okapi_store::history::prune_delivery(&bed.pg, Utc::now())
        .await
        .unwrap();
    for (index, id) in ids.iter().enumerate() {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_ch_batches WHERE id=$1")
            .bind(id)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
        assert_eq!(count, i64::from(index != 0));
    }
}

#[tokio::test]
async fn expiry_pg_failure_leaves_hot_funds_available_and_can_retry() {
    dotenvy::dotenv().ok();
    let bed = setup().await;
    bed.credit(10_000, "test").await;
    let now = Utc::now();
    sqlx::query("UPDATE users SET balance_expires_at=$2 WHERE id=$1")
        .bind(bed.uid)
        .bind(now - chrono::Duration::days(1))
        .execute(&bed.pg)
        .await
        .unwrap();
    sqlx::query("CREATE FUNCTION reject_expire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='expire' THEN RAISE EXCEPTION 'simulated expiry PG write failure'; END IF; RETURN NEW; END $$").execute(&bed.pg).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_expire BEFORE INSERT ON billing_events FOR EACH ROW EXECUTE FUNCTION reject_expire()").execute(&bed.pg).await.unwrap();
    assert!(
        okapi_ledger::operations::expire(&bed.pg, &bed.ledger, bed.uid, now)
            .await
            .is_err()
    );
    assert_eq!(
        bed.ledger.balance(bed.uid).await.unwrap().as_micros(),
        10_000
    );
    sqlx::query("DROP TRIGGER reject_expire ON billing_events")
        .execute(&bed.pg)
        .await
        .unwrap();
    assert_eq!(
        okapi_ledger::operations::expire(&bed.pg, &bed.ledger, bed.uid, now)
            .await
            .unwrap()
            .as_micros(),
        10_000
    );
    assert_eq!(bed.ledger.balance(bed.uid).await.unwrap().as_micros(), 0);
    assert_eq!(
        okapi_ledger::operations::expire(&bed.pg, &bed.ledger, bed.uid, now)
            .await
            .unwrap()
            .as_micros(),
        0
    );
}

#[tokio::test]
async fn default_partition_rows_are_pruned_without_losing_financial_facts() {
    let bed = setup().await;
    let request = Uuid::new_v4();
    sqlx::query("INSERT INTO billing_events(user_id,request_id,event_type,delta_micro,actor,created_at) VALUES($1,$2,'recharge',12345,'default-retention','2010-01-01')").bind(bed.uid).bind(request).execute(&bed.pg).await.unwrap();
    sqlx::query("INSERT INTO billing_records(request_id,user_id,api_key_id,model_name,status,amount_micro,original_amount_micro,created_at) VALUES($1,$2,$3,'default-fixture',20,234,234,'2010-01-01')").bind(request).bind(bed.uid).bind(bed.kid).execute(&bed.pg).await.unwrap();
    sqlx::query("INSERT INTO audit_logs(actor,action,created_at) VALUES('default-retention','fixture','2010-01-01')").execute(&bed.pg).await.unwrap();
    worker::drop_expired_partitions(&bed.pg, Utc::now())
        .await
        .unwrap();
    worker::drop_expired_partitions(&bed.pg, Utc::now())
        .await
        .unwrap();
    let carry:i64=sqlx::query_scalar("SELECT delta_micro::bigint FROM billing_event_carry WHERE user_id=$1 AND actor='default-retention'").bind(bed.uid).fetch_one(&bed.pg).await.unwrap();
    assert_eq!(carry, 12345, "repeat pruning must not carry twice");
    let receipt: i64 =
        sqlx::query_scalar("SELECT amount_micro FROM billing_record_receipts WHERE request_id=$1")
            .bind(request)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert_eq!(receipt, 234);
    let details: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE request_id=$1")
            .bind(request)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert_eq!(details, 0);
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE actor='default-retention'")
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert_eq!(audits, 0);
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('billing_records_default') IS NOT NULL")
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert!(exists);
}
