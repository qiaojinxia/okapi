//! PG 记账契约直测（IMPLEMENTATION §2.2 步骤 13 / §5.3）：`record_settlement` 一个事务里
//! 落 billing_records + billing_events + users 快照列 + api_keys 用量 + outbox 五处，
//! 四金额列与 pool 三处同写；`admin_refund` 按 status 闸幂等并逐项回冲；
//! 订阅池结算与订阅事件不动钱包快照。依赖 .env 的 DATABASE_URL（scripts/dev-deps.sh up）。

use okapi_domain::{BillingState, Money, TokenUsage};
use okapi_ledger::pg::{UsageDimensions, admin_refund, record_credit};
use okapi_ledger::{Pool, SettlementInput, record_settlement, record_sub_event};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Bed {
    pg: PgPool,
    user_id: i64,
    key_id: i64,
}

async fn bed() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(&pg, &format!("pgc-{suffix}"))
        .await
        .unwrap();
    let key_id = okapi_store::provision::create_api_key(
        &pg,
        user_id,
        &format!("hash-{suffix}"),
        "sk-okapi-pgc",
    )
    .await
    .unwrap();
    Bed {
        pg,
        user_id,
        key_id,
    }
}

impl Bed {
    /// 一笔标准消费：官方价 300、折后 240、让利 60、上游成本 150，钱包付。
    fn committed(&self, request_id: Uuid) -> SettlementInput<'static> {
        SettlementInput {
            dimensions: UsageDimensions::new(
                "gpt-5-alias",
                "gpt-5-2026",
                "/v1/chat/completions",
                "/v1/chat/completions",
            ),
            request_id,
            log_type: 2,
            user_id: self.user_id,
            api_key_id: self.key_id,
            group_code: "default",
            model_name: "gpt-5",
            channel_id: Some(1),
            channel_key_id: Some(11),
            state: BillingState::Committed,
            usage: TokenUsage {
                prompt_tokens: 100,
                cached_tokens: 20,
                cache_write_tokens: 10,
                completion_tokens: 30,
                reasoning_tokens: 5,
                ..TokenUsage::default()
            },
            amount: Money::from_micros(240),
            original: Money::from_micros(300),
            discount: Money::from_micros(60),
            list_price: Money::from_micros(300),
            upstream_cost: Some(Money::from_micros(150)),
            pricing_epoch: Some(7),
            pricing_snapshot: Some(json!({ "mode": "ratio", "model_ratio": "1" })),
            latency_ms: 1234,
            ttft_ms: Some(210),
            is_stream: true,
            retry_count: 1,
            failover_count: 0,
            upstream_status: Some(200),
            error_code: None,
            upstream_request_id: Some("up-req-1"),
            node: "test-node",
            sticky_layer: 2,
            client_type: "claude-code",
            client_ip: Some("203.0.113.9"),
            delta_micro: -240,
            balance_after: Some(Money::from_micros(9_760)),
            event_type: "commit",
            pool: Pool::Wallet,
        }
    }

    async fn wallet_snapshot(&self) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT balance_micro AS "v!" FROM users WHERE id = $1"#,
            self.user_id
        )
        .fetch_one(&self.pg)
        .await
        .unwrap()
    }

    async fn key_used(&self) -> (i64, bool) {
        let row = sqlx::query!(
            r#"SELECT used_micro, last_used_at FROM api_keys WHERE id = $1"#,
            self.key_id
        )
        .fetch_one(&self.pg)
        .await
        .unwrap();
        (row.used_micro, row.last_used_at.is_some())
    }

    async fn events(
        &self,
        request_id: Uuid,
    ) -> Vec<(String, i64, Option<i64>, String, i16, Value)> {
        sqlx::query!(
            r#"SELECT event_type, delta_micro, balance_after_micro, actor, pool, payload
               FROM billing_events WHERE request_id = $1 ORDER BY event_id"#,
            request_id
        )
        .fetch_all(&self.pg)
        .await
        .unwrap()
        .into_iter()
        .map(|r| {
            (
                r.event_type,
                r.delta_micro,
                r.balance_after_micro,
                r.actor,
                r.pool,
                r.payload.unwrap_or(Value::Null),
            )
        })
        .collect()
    }

    async fn outbox(&self, request_id: Uuid) -> Vec<(String, Value)> {
        sqlx::query!(
            r#"SELECT topic, payload FROM billing_outbox
               WHERE payload->>'request_id' = $1 ORDER BY id"#,
            request_id.to_string()
        )
        .fetch_all(&self.pg)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.topic, r.payload))
        .collect()
    }
}

/// `billing_records` 行逐列对照 `Bed::committed` 的输入。
async fn assert_committed_record(bed: &Bed, rid: Uuid) {
    let rec = sqlx::query!(
        r#"SELECT log_type, status, prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
                  amount_micro, original_amount_micro, discount_micro, upstream_cost_micro,
                  pricing_epoch, pricing_snapshot, latency_ms, ttft_ms, is_stream, retry_count,
                  failover_count, upstream_status, error_code, upstream_request_id, node,
                  sticky_layer, client_type, host(client_ip) AS client_ip, pool, channel_id, channel_key_id
           FROM billing_records WHERE request_id = $1"#,
        rid
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(rec.log_type, 2);
    assert_eq!(rec.status, 20);
    assert_eq!(
        (
            rec.prompt_tokens,
            rec.cached_tokens,
            rec.completion_tokens,
            rec.reasoning_tokens
        ),
        (100, 20, 30, 5)
    );
    assert_eq!(
        (
            rec.amount_micro,
            rec.original_amount_micro,
            rec.discount_micro,
            rec.upstream_cost_micro
        ),
        (240, 300, 60, Some(150)),
        "四金额列"
    );
    assert_eq!(rec.pricing_epoch, Some(7));
    assert_eq!(rec.pricing_snapshot.unwrap()["mode"], "ratio");
    assert_eq!(
        (rec.latency_ms, rec.ttft_ms, rec.is_stream),
        (Some(1234), Some(210), true)
    );
    assert_eq!((rec.retry_count, rec.failover_count), (1, 0));
    assert_eq!(rec.upstream_status, Some(200));
    assert!(rec.error_code.is_none());
    assert_eq!(rec.upstream_request_id.as_deref(), Some("up-req-1"));
    assert_eq!(rec.node.as_deref(), Some("test-node"));
    assert_eq!(rec.sticky_layer, 2);
    assert_eq!(rec.client_type.as_deref(), Some("claude-code"));
    assert_eq!(
        rec.client_ip.as_deref(),
        Some("203.0.113.9"),
        "INET 列真的落了"
    );
    assert_eq!(rec.pool, 0);
    assert_eq!((rec.channel_id, rec.channel_key_id), (Some(1), Some(11)));
}

/// 五处同事务落地，四金额列 / pool / 维度在 records、events、outbox 三处一致。
#[tokio::test]
async fn settlement_lands_record_event_snapshot_key_usage_and_outbox_consistently() {
    let bed = bed().await;
    record_credit(
        &bed.pg,
        bed.user_id,
        Money::from_micros(10_000),
        "recharge",
        "system:test",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(bed.wallet_snapshot().await, 10_000);

    let rid = Uuid::new_v4();
    record_settlement(&bed.pg, bed.committed(rid))
        .await
        .unwrap();
    assert_committed_record(&bed, rid).await;

    let events = bed.events(rid).await;
    assert_eq!(events.len(), 1);
    let (kind, delta, after, actor, pool, payload) = &events[0];
    assert_eq!(kind, "commit");
    assert_eq!(*delta, -240);
    assert_eq!(*after, Some(9_760));
    assert_eq!(actor, "system:gateway");
    assert_eq!(*pool, 0);
    assert_eq!(payload["billing_type"], "ratio");
    assert_eq!(payload["upstream_cost_known"], true);
    assert_eq!(payload["amount_micro"], 240);
    assert_eq!(payload["requested_model"], "gpt-5-alias");
    assert_eq!(payload["upstream_model"], "gpt-5-2026");

    assert_eq!(
        bed.wallet_snapshot().await,
        10_000 - 240,
        "钱包快照列随 delta 走"
    );
    let (used, touched) = bed.key_used().await;
    assert_eq!(used, 240);
    assert!(touched, "last_used_at 应被刷新");

    let outbox = bed.outbox(rid).await;
    assert_eq!(outbox.len(), 1);
    let (topic, payload) = &outbox[0];
    assert_eq!(topic, "billing.completed");
    assert_eq!(payload["log_type"], 2);
    assert_eq!(payload["status"], 20);
    assert_eq!(payload["amount_micro"], 240);
    assert_eq!(payload["original_amount_micro"], 300);
    assert_eq!(payload["discount_micro"], 60);
    assert_eq!(payload["upstream_cost_micro"], 150);
    assert_eq!(payload["upstream_cost_known"], true);
    assert_eq!(payload["cache_write_tokens"], 10);
    assert_eq!(payload["pool"], 0);
    assert_eq!(payload["client_ip"], "203.0.113.9");
    assert_eq!(payload["billing_type"], "ratio");
    assert_eq!(
        payload["ratio_snapshot"]
            .as_str()
            .map(|s| s.contains("\"mode\":\"ratio\"")),
        Some(true)
    );
}

/// 幂等：同一 request_id 重放（settle_write 在 COMMIT 成功但回包丢失后的重试）不得再写
/// 任何一处——记录仍一行、事件仍一条、快照与 key 用量不再变、outbox 不再多一条。
#[tokio::test]
async fn replaying_a_settled_request_writes_nothing() {
    let bed = bed().await;
    record_credit(
        &bed.pg,
        bed.user_id,
        Money::from_micros(10_000),
        "recharge",
        "system:test",
        json!({}),
    )
    .await
    .unwrap();
    let rid = Uuid::new_v4();
    record_settlement(&bed.pg, bed.committed(rid))
        .await
        .unwrap();

    // 重放同一笔，哪怕金额被改也不能再动账：以第一次落地的为准
    let mut replay = bed.committed(rid);
    replay.amount = Money::from_micros(999_999);
    replay.delta_micro = -999_999;
    record_settlement(&bed.pg, replay).await.unwrap();

    let records: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "c!" FROM billing_records WHERE request_id = $1"#,
        rid
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(records, 1, "重放不得再插记录行");
    assert_committed_record(&bed, rid).await;
    assert_eq!(bed.events(rid).await.len(), 1, "重放不得再记事件");
    assert_eq!(bed.outbox(rid).await.len(), 1, "重放不得再进 outbox");
    assert_eq!(bed.wallet_snapshot().await, 10_000 - 240, "快照只扣一次");
    assert_eq!(bed.key_used().await.0, 240, "key 用量只加一次");
}

/// 事务原子性：第二条语句失败（events.event_type 是 VARCHAR(16)，塞超长值）→
/// 已执行成功的第一条 INSERT 必须随事务回滚，五处一处都不落。
#[tokio::test]
async fn failing_statement_rolls_back_every_table() {
    let bed = bed().await;
    let rid = Uuid::new_v4();
    let before = bed.wallet_snapshot().await;
    let mut input = bed.committed(rid);
    input.event_type = "commit_but_this_event_type_is_far_too_long";
    let err = record_settlement(&bed.pg, input).await;
    assert!(err.is_err(), "超长 event_type 必须写失败");

    let records: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "c!" FROM billing_records WHERE request_id = $1"#,
        rid
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(records, 0, "记录行不得残留");
    assert!(bed.events(rid).await.is_empty(), "事件不得残留");
    assert!(bed.outbox(rid).await.is_empty(), "outbox 不得残留");
    assert_eq!(bed.wallet_snapshot().await, before);
    assert_eq!(bed.key_used().await.0, 0, "key 用量不得被半途累加");
}

/// 订阅池付的请求：records / events / outbox 都记 pool=1，钱包快照列不动，key 用量照记。
#[tokio::test]
async fn subscription_pool_settlement_leaves_wallet_snapshot_untouched() {
    let bed = bed().await;
    let rid = Uuid::new_v4();
    let mut input = bed.committed(rid);
    input.pool = Pool::Subscription;
    input.balance_after = Some(Money::from_micros(60));
    record_settlement(&bed.pg, input).await.unwrap();

    let pool: i16 = sqlx::query_scalar!(
        r#"SELECT pool AS "p!" FROM billing_records WHERE request_id = $1"#,
        rid
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(pool, 1);
    assert_eq!(bed.events(rid).await[0].4, 1);
    assert_eq!(bed.outbox(rid).await[0].1["pool"], 1);
    assert_eq!(bed.wallet_snapshot().await, 0, "订阅池结算不碰钱包快照");
    assert_eq!(bed.key_used().await.0, 240, "key 用量与付款池无关");

    // 订阅事件同理：只记事件（pool=1、request_id 空），钱包快照不动
    record_sub_event(
        &bed.pg,
        bed.user_id,
        Money::from_micros(300),
        Money::from_micros(300),
        "sub_grant",
        "system:test",
        json!({ "plan": "basic" }),
    )
    .await
    .unwrap();
    let sub_events: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "c!" FROM billing_events
           WHERE user_id = $1 AND event_type = 'sub_grant' AND pool = 1 AND request_id IS NULL"#,
        bed.user_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(sub_events, 1);
    assert_eq!(bed.wallet_snapshot().await, 0);
}

/// 失败请求：零金额、error_code 落列、事件 delta 0、成本未知；钱包与 key 用量不动。
#[tokio::test]
async fn failed_request_records_error_with_zero_money_movement() {
    let bed = bed().await;
    let rid = Uuid::new_v4();
    let mut input = bed.committed(rid);
    input.log_type = 5;
    input.state = BillingState::Failed;
    input.amount = Money::ZERO;
    input.original = Money::ZERO;
    input.discount = Money::ZERO;
    input.list_price = Money::ZERO;
    input.upstream_cost = None;
    input.upstream_status = Some(502);
    input.error_code = Some("upstream_error");
    input.delta_micro = 0;
    input.balance_after = None;
    input.event_type = "refund";
    record_settlement(&bed.pg, input).await.unwrap();

    let rec = sqlx::query!(
        r#"SELECT log_type, status, amount_micro, upstream_cost_micro, error_code, upstream_status
           FROM billing_records WHERE request_id = $1"#,
        rid
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!((rec.log_type, rec.status), (5, 40));
    assert_eq!(rec.amount_micro, 0);
    assert!(rec.upstream_cost_micro.is_none());
    assert_eq!(rec.error_code.as_deref(), Some("upstream_error"));
    assert_eq!(rec.upstream_status, Some(502));

    let events = bed.events(rid).await;
    assert_eq!(
        (events[0].0.as_str(), events[0].1, events[0].2),
        ("refund", 0, None)
    );
    let outbox = bed.outbox(rid).await;
    assert_eq!(outbox[0].1["upstream_cost_micro"], 0);
    assert_eq!(outbox[0].1["upstream_cost_known"], false);
    assert_eq!(outbox[0].1["error_code"], "upstream_error");
    assert_eq!(bed.wallet_snapshot().await, 0);
    assert_eq!(bed.key_used().await.0, 0);
}

/// 管理员按日志退款：只对 committed 生效且只生效一次；状态翻 30、退款事件、快照与 key 用量回冲、
/// outbox 负额冲销行四金额取反；重复退款与对失败记录退款都返回 None 且不再写任何行。
#[tokio::test]
async fn admin_refund_reverses_once_and_is_idempotent() {
    let bed = bed().await;
    record_credit(
        &bed.pg,
        bed.user_id,
        Money::from_micros(10_000),
        "recharge",
        "system:test",
        json!({}),
    )
    .await
    .unwrap();
    let rid = Uuid::new_v4();
    record_settlement(&bed.pg, bed.committed(rid))
        .await
        .unwrap();
    assert_eq!(bed.wallet_snapshot().await, 9_760);

    let first = admin_refund(&bed.pg, rid, "duplicate charge", "admin:42")
        .await
        .unwrap()
        .expect("committed 记录应可退");
    assert_eq!(first.user_id, bed.user_id);
    assert_eq!(first.amount.as_micros(), 240);
    assert_eq!(first.pool, Pool::Wallet);

    let status: i16 = sqlx::query_scalar!(
        r#"SELECT status AS "s!" FROM billing_records WHERE request_id = $1"#,
        rid
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(status, 30);
    assert_eq!(bed.wallet_snapshot().await, 10_000, "快照回补到结算前");
    assert_eq!(bed.key_used().await.0, 0, "key 用量回冲");

    let events = bed.events(rid).await;
    assert_eq!(events.len(), 2);
    let (kind, delta, _, actor, pool, payload) = &events[1];
    assert_eq!(kind, "refund");
    assert_eq!(*delta, 240);
    assert_eq!(actor, "admin:42");
    assert_eq!(*pool, 0);
    assert_eq!(payload["reason"], "duplicate charge");
    assert_eq!(payload["tags"], json!(["admin_refund"]));

    let outbox = bed.outbox(rid).await;
    assert_eq!(outbox.len(), 2);
    let (topic, payload) = &outbox[1];
    assert_eq!(topic, "billing.refunded");
    assert_eq!(payload["log_type"], 6);
    assert_eq!(payload["status"], 30);
    assert_eq!(payload["amount_micro"], -240);
    assert_eq!(payload["original_amount_micro"], -300);
    assert_eq!(payload["discount_micro"], -60);
    assert_eq!(payload["upstream_cost_micro"], -150);
    assert_eq!(payload["prompt_tokens"], 0, "token 事实不冲");

    // 幂等：第二次退款拿不到行，事件 / outbox / 快照 / 用量都不再变
    let again = admin_refund(&bed.pg, rid, "again", "admin:42")
        .await
        .unwrap();
    assert!(again.is_none());
    assert_eq!(bed.events(rid).await.len(), 2);
    assert_eq!(bed.outbox(rid).await.len(), 2);
    assert_eq!(bed.wallet_snapshot().await, 10_000);
    assert_eq!(bed.key_used().await.0, 0);

    // 失败记录（status 40）不可退
    let failed = Uuid::new_v4();
    let mut input = bed.committed(failed);
    input.log_type = 5;
    input.state = BillingState::Failed;
    input.amount = Money::ZERO;
    input.delta_micro = 0;
    input.event_type = "refund";
    record_settlement(&bed.pg, input).await.unwrap();
    assert!(
        admin_refund(&bed.pg, failed, "x", "admin:42")
            .await
            .unwrap()
            .is_none()
    );
}
