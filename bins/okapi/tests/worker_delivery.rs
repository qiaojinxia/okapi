//! Real PG/CH failure boundaries; configured dependencies must actually run.
use okapi::worker::{chsink, nats_relay};
use okapi_store::ChClient;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn setup() -> Option<(PgPool, ChClient)> {
    dotenvy::dotenv().ok();
    let Ok(ch_url) = std::env::var("OKAPI_CLICKHOUSE_URL") else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return None;
    };
    let pg = okapi_store::connect_pg(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let ch = ChClient::new(&ch_url, "okapi").unwrap();
    assert!(ch.ping().await);
    ch.ensure_schema().await.unwrap();
    drain(&pg, &ch).await;
    Some((pg, ch))
}

fn payload(user_id: i64, amount: i64) -> Value {
    json!({"request_id":Uuid::new_v4(),"user_id":user_id,"api_key_id":1,
        "group":"default","model":"delivery-fault-test","channel_id":1,
        "channel_key_id":1,"log_type":2,"status":20,"prompt_tokens":100,
        "completion_tokens":20,"cached_tokens":0,"reasoning_tokens":0,
        "amount_micro":amount,"original_amount_micro":amount+5,"discount_micro":5,
        "upstream_cost_micro":3,"upstream_cost_known":true,
        "pricing_epoch":1,"latency_ms":12,"ttft_ms":5,"is_stream":true,
        "retry_count":0,"failover_count":0,"node":"delivery-fault-test"})
}

async fn insert(pg: &PgPool, p: &Value) -> i64 {
    sqlx::query_scalar!(
        "INSERT INTO billing_outbox (topic, payload) VALUES ('billing.completed', $1) RETURNING id",
        p
    )
    .fetch_one(pg)
    .await
    .unwrap()
}

async fn drain(pg: &PgPool, ch: &ChClient) {
    for _ in 0..100 {
        if chsink::process_once(pg, ch).await.unwrap() == 0 {
            return;
        }
    }
    panic!("delivery queue did not drain");
}

fn number(v: &Value, key: &str) -> i64 {
    v[key]
        .as_i64()
        .or_else(|| v[key].as_str().and_then(|s| s.parse().ok()))
        .unwrap()
}

// Names/IDs come only from this fixture. The trigger cannot affect another event.
async fn install_mark_failure(pg: &PgPool, id: i64) -> String {
    let name = format!("delivery_fault_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'injected outbox mark failure'; END $$;
         CREATE TRIGGER {name} BEFORE UPDATE ON billing_outbox FOR EACH ROW
         WHEN (NEW.id={id} AND NEW.status=1) EXECUTE FUNCTION {name}();"
    )))
    .execute(pg)
    .await
    .unwrap();
    name
}

async fn remove_mark_failure(pg: &PgPool, name: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP TRIGGER {name} ON billing_outbox; DROP FUNCTION {name}();"
    )))
    .execute(pg)
    .await
    .unwrap();
}

async fn assert_totals(ch: &ChClient, user: i64, n: i64, amount: i64) {
    let raw = ch.query_json_each_row(&format!(
        "SELECT count() AS n,sum(prompt_tokens+completion_tokens) AS tokens,sum(amount_micro) AS amount,sum(original_amount_micro) AS original,sum(discount_micro) AS discount,sum(upstream_cost_micro) AS cost,min(pricing_epoch) AS epoch FROM request_log_raw WHERE user_id={user}"
    )).await.unwrap();
    assert_eq!(number(&raw[0], "n"), n, "raw event count");
    assert_eq!(number(&raw[0], "tokens"), n * 120, "raw Token total");
    assert_eq!(number(&raw[0], "amount"), amount, "raw amount");
    assert_eq!(number(&raw[0], "original"), amount + n * 5);
    assert_eq!(number(&raw[0], "discount"), n * 5);
    assert_eq!(number(&raw[0], "cost"), n * 3);
    assert_eq!(number(&raw[0], "epoch"), 1);
    let mv = ch.query_json_each_row(&format!(
        "SELECT countMerge(requests) AS n,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount FROM mv_user_day WHERE user_id={user}"
    )).await.unwrap();
    assert_eq!(number(&mv[0], "n"), n, "MV event count");
    assert_eq!(number(&mv[0], "tokens"), n * 120, "MV Token total");
    assert_eq!(number(&mv[0], "amount"), amount, "MV amount");
}

#[tokio::test]
async fn ch_success_pg_mark_failure_then_new_events_do_not_duplicate_statistics() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let bytes = *Uuid::new_v4().as_bytes();
    let user =
        4_000_000_000 + i64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    let first = insert(&pg, &payload(user, 10)).await;
    let trigger = install_mark_failure(&pg, first).await;
    let attempted = chsink::process_once(&pg, &ch).await;
    remove_mark_failure(&pg, &trigger).await;
    assert!(
        attempted.is_err(),
        "must reach the injected PG failure after CH write"
    );
    assert_totals(&ch, user, 1, 10).await;
    insert(&pg, &payload(user, 20)).await;
    drain(&pg, &ch).await;
    assert_totals(&ch, user, 2, 30).await;
}

fn user_id() -> i64 {
    let bytes = *Uuid::new_v4().as_bytes();
    5_000_000_000 + i64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

async fn nats(pg: &PgPool, ch: &ChClient) -> Option<async_nats::jetstream::Context> {
    let Ok(url) = std::env::var("OKAPI_NATS_URL") else {
        eprintln!("跳过：未配置 OKAPI_NATS_URL");
        return None;
    };
    let client = async_nats::connect(url).await.unwrap();
    let js = nats_relay::ensure_topology(&client).await.unwrap();
    for _ in 0..20 {
        if nats_relay::chsink_js_once(pg, &js, ch).await.unwrap() == 0 {
            return Some(js);
        }
    }
    panic!("JS did not drain");
}

async fn relay_payload(pg: &PgPool, id: i64) -> Value {
    let r = sqlx::query!(
        "SELECT event_id,created_at,payload FROM billing_outbox WHERE id=$1",
        id
    )
    .fetch_one(pg)
    .await
    .unwrap();
    let mut p = r.payload;
    p["ts"] = json!(r.created_at.format("%Y-%m-%d %H:%M:%S%.3f").to_string());
    p["_billing_event_id"] = json!(r.event_id);
    p
}

async fn force_due(pg: &PgPool, id: i64) {
    sqlx::query!(
        "UPDATE billing_ch_batches SET next_retry_at=now()-interval '1 second' WHERE id=(SELECT ch_batch_id FROM billing_outbox WHERE id=$1) AND status=0",
        id
    ).execute(pg).await.unwrap();
}

#[tokio::test]
async fn concurrent_workers_keep_full_batches_bounded_and_statistics_exact() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let user = user_id();
    let mut tx = pg.begin().await.unwrap();
    for _ in 0..1001 {
        sqlx::query!(
            "INSERT INTO billing_outbox (topic, payload) VALUES ('billing.completed', $1) RETURNING id",
            payload(user, 1)
        ).fetch_one(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let (a, b, c) = tokio::join!(
        chsink::process_once(&pg, &ch),
        chsink::process_once(&pg, &ch),
        chsink::process_once(&pg, &ch)
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    drain(&pg, &ch).await;
    assert_totals(&ch, user, 1001, 1001).await;
    let batches = sqlx::query!(
        r#"SELECT DISTINCT b.id,b.status,b.event_count,b.rows,b.payloads
           FROM billing_ch_batches b JOIN billing_outbox o ON o.ch_batch_id=b.id
           WHERE o.payload->>'user_id'=$1"#,
        user.to_string()
    )
    .fetch_all(&pg)
    .await
    .unwrap();
    assert_eq!(batches.iter().map(|b| b.event_count).sum::<i32>(), 1001);
    assert!(batches.iter().all(|b| b.status == 1
        && b.event_count <= 500
        && b.rows == json!([])
        && b.payloads == json!([])));
}

/// Both publish/PG and CH/PG ambiguous windows, then replay past the NATS ID window.
#[tokio::test]
async fn js_replays_after_pg_failures_and_outbox_retention_do_not_duplicate() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let Some(js) = nats(&pg, &ch).await else {
        return;
    };
    let user = user_id();
    let id = insert(&pg, &payload(user, 10)).await;
    let replay = relay_payload(&pg, id).await;
    let mut stream = js.get_stream("BILLING").await.unwrap();
    let _: async_nats::jetstream::consumer::PullConsumer = stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some("chsink".to_owned()),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ack_wait: std::time::Duration::from_secs(30),
            max_deliver: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    let before = stream.info().await.unwrap().state.messages;
    let trigger = install_mark_failure(&pg, id).await;
    let failed = nats_relay::relay_once(&pg, &js).await;
    remove_mark_failure(&pg, &trigger).await;
    assert!(failed.is_err(), "publish must precede injected PG failure");
    nats_relay::relay_once(&pg, &js).await.unwrap();
    assert_eq!(
        stream.info().await.unwrap().state.messages,
        before + 1,
        "publish ID must be stable"
    );
    let trigger = install_mark_failure(&pg, id).await;
    let failed = nats_relay::chsink_js_once(&pg, &js, &ch).await;
    remove_mark_failure(&pg, &trigger).await;
    assert!(
        failed.is_err(),
        "CH write must precede injected PG mark failure"
    );
    assert_totals(&ch, user, 1, 10).await;
    let backlog = okapi_store::delivery::backlog(&pg).await.unwrap();
    assert_eq!(
        backlog.pending_events, 1,
        "JS-acked CH backlog must remain visible"
    );
    assert_eq!(backlog.ch_pending_events, 1);
    let consumer: async_nats::jetstream::consumer::PullConsumer =
        stream.get_consumer("chsink").await.unwrap();
    assert_eq!(
        consumer.cached_info().config.max_deliver,
        -1,
        "upgrade existing consumer without resetting its cursor"
    );
    assert_eq!(
        consumer.cached_info().num_ack_pending,
        0,
        "PG has durably taken responsibility"
    );
    chsink::process_once(&pg, &ch).await.unwrap();
    assert_totals(&ch, user, 1, 10).await;
    insert(&pg, &payload(user, 20)).await;
    nats_relay::relay_once(&pg, &js).await.unwrap();
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    assert_totals(&ch, user, 2, 30).await;
    sqlx::query!("DELETE FROM billing_outbox WHERE id=$1", id)
        .execute(&pg)
        .await
        .unwrap();
    // No publish ID on purpose: create real new stream sequences for an old event.
    for _ in 0..2 {
        js.publish("billing.completed", replay.to_string().into())
            .await
            .unwrap()
            .await
            .unwrap();
    }
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    drain(&pg, &ch).await;
    assert_totals(&ch, user, 2, 30).await;
    stream.delete_consumer("chsink").await.unwrap();
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    assert_totals(&ch, user, 2, 30).await;
}

#[tokio::test]
async fn nats_mode_resumes_direct_batches_and_shares_completed_receipts() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let Some(js) = nats(&pg, &ch).await else {
        return;
    };
    let user = user_id();
    let id = insert(&pg, &payload(user, 10)).await;
    let replay = relay_payload(&pg, id).await;
    let bad = ChClient::new("http://127.0.0.1:9", "okapi").unwrap();
    chsink::process_once(&pg, &bad).await.unwrap();
    assert_eq!(
        nats_relay::relay_once(&pg, &js).await.unwrap(),
        0,
        "assigned batch cannot be republished"
    );
    force_due(&pg, id).await;
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    assert_totals(&ch, user, 1, 10).await;
    js.publish("billing.completed", replay.to_string().into())
        .await
        .unwrap()
        .await
        .unwrap();
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    assert_totals(&ch, user, 1, 10).await;
}

#[tokio::test]
async fn malformed_js_messages_are_durable_dlq_entries_without_fake_statistics() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let Some(js) = nats(&pg, &ch).await else {
        return;
    };
    let before =
        sqlx::query_scalar!(r#"SELECT count(*) AS "c!" FROM billing_dlq WHERE source='jetstream'"#)
            .fetch_one(&pg)
            .await
            .unwrap();
    let total = ch
        .query_json_each_row("SELECT count() AS n FROM request_log_raw")
        .await
        .unwrap();
    let user = user_id();
    for body in [
        "not-json".to_owned(),
        "null".to_owned(),
        payload(user, 10).to_string(),
    ] {
        js.publish("billing.completed", body.into())
            .await
            .unwrap()
            .await
            .unwrap();
    }
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    let after =
        sqlx::query_scalar!(r#"SELECT count(*) AS "c!" FROM billing_dlq WHERE source='jetstream'"#)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(after - before, 3);
    let now = ch
        .query_json_each_row("SELECT count() AS n FROM request_log_raw")
        .await
        .unwrap();
    assert_eq!(number(&now[0], "n"), number(&total[0], "n"));
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    let after_replay =
        sqlx::query_scalar!(r#"SELECT count(*) AS "c!" FROM billing_dlq WHERE source='jetstream'"#)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(after_replay, after);
}

#[tokio::test]
async fn consume_and_refund_with_same_request_id_remain_distinct_events() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let user = user_id();
    let consume = payload(user, 10);
    let mut refund = consume.clone();
    refund["log_type"] = json!(6);
    refund["status"] = json!(30);
    refund["prompt_tokens"] = json!(0);
    refund["completion_tokens"] = json!(0);
    for (key, value) in [
        ("amount_micro", -10),
        ("original_amount_micro", -15),
        ("discount_micro", -5),
        ("upstream_cost_micro", -3),
    ] {
        refund[key] = json!(value);
    }
    insert(&pg, &consume).await;
    insert(&pg, &refund).await;
    drain(&pg, &ch).await;
    let raw=ch.query_json_each_row(&format!(
        "SELECT count() AS n,uniqExact(request_id) AS request_ids,sum(prompt_tokens+completion_tokens) AS tokens,sum(amount_micro) AS amount,sum(original_amount_micro) AS original,sum(discount_micro) AS discount,sum(upstream_cost_micro) AS cost FROM request_log_raw WHERE user_id={user}"
    )).await.unwrap();
    assert_eq!(number(&raw[0], "n"), 2);
    assert_eq!(number(&raw[0], "request_ids"), 1);
    assert_eq!(number(&raw[0], "tokens"), 120);
    for key in ["amount", "original", "discount", "cost"] {
        assert_eq!(number(&raw[0], key), 0);
    }
    let mv=ch.query_json_each_row(&format!(
        "SELECT countMerge(requests) AS calls,countMerge(financial_records) AS records,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount,sumMerge(original) AS original,sumMerge(discount) AS discount,sumMerge(upstream_cost) AS cost FROM mv_user_day WHERE user_id={user}"
    )).await.unwrap();
    assert_eq!(
        number(&mv[0], "calls"),
        1,
        "refund is a financial adjustment, not a second call"
    );
    assert_eq!(
        number(&mv[0], "records"),
        2,
        "both financial events remain in the coverage denominator"
    );
    assert_eq!(number(&mv[0], "tokens"), 120);
    for key in ["amount", "original", "discount", "cost"] {
        assert_eq!(number(&mv[0], key), 0);
    }
}

#[tokio::test]
async fn direct_fallback_recovers_new_js_publications_without_replaying_legacy_history() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let Some(js) = nats(&pg, &ch).await else {
        return;
    };
    let legacy_user = user_id();
    let legacy = insert(&pg, &payload(legacy_user, 30)).await;
    let p = relay_payload(&pg, legacy).await;
    ch.insert_json_each_row(
        "request_log_raw",
        &[chsink::js_payload_to_ch_row(&p)],
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE billing_outbox SET status=1,published_at=now() WHERE id=$1",
        legacy
    )
    .execute(&pg)
    .await
    .unwrap();
    let user = user_id();
    insert(&pg, &payload(user, 10)).await;
    nats_relay::relay_once(&pg, &js).await.unwrap();
    let backlog = okapi_store::delivery::backlog(&pg).await.unwrap();
    assert_eq!(
        backlog.pending_events, 1,
        "published does not imply CH handoff"
    );
    assert_eq!(backlog.ch_pending_events, 0);
    chsink::process_once(&pg, &ch).await.unwrap();
    assert_totals(&ch, user, 1, 10).await;
    assert_totals(&ch, legacy_user, 1, 30).await;
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    assert_totals(&ch, user, 1, 10).await;
    assert_totals(&ch, legacy_user, 1, 30).await;
}

#[tokio::test]
async fn expired_js_message_is_recovered_from_original_outbox_and_late_replay_is_safe() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    let Some(js) = nats(&pg, &ch).await else {
        return;
    };
    let user = user_id();
    let id = insert(&pg, &payload(user, 10)).await;
    let replay = relay_payload(&pg, id).await;
    nats_relay::relay_once(&pg, &js).await.unwrap();
    let mut stream = js.get_stream("BILLING").await.unwrap();
    let seq = stream.info().await.unwrap().state.last_sequence;
    assert!(stream.delete_message(seq).await.unwrap());
    sqlx::query!(
        "UPDATE billing_outbox SET published_at=now()-interval '10 minutes' WHERE id=$1",
        id
    )
    .execute(&pg)
    .await
    .unwrap();
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    assert_totals(&ch, user, 1, 10).await;
    js.publish("billing.completed", replay.to_string().into())
        .await
        .unwrap()
        .await
        .unwrap();
    nats_relay::chsink_js_once(&pg, &js, &ch).await.unwrap();
    assert_totals(&ch, user, 1, 10).await;
}
