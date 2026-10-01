//! M2 chsink 验收：outbox → ClickHouse 管道、MV 聚合、批次幂等、DLQ 终态。
//! 需要 OKAPI_CLICKHOUSE_URL（scripts/dev-deps.sh up）；未配置时软跳过。

use okapi::worker::chsink;
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
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let ch = ChClient::new(&ch_url, "okapi").unwrap();
    assert!(ch.ping().await, "ClickHouse 不可达：{ch_url}");
    ch.ensure_schema().await.unwrap();
    Some((pg, ch))
}

#[tokio::test]
async fn scalar_query_parameters_round_trip_special_text_without_sql_interpretation() {
    let Some((_, ch)) = setup().await else {
        return;
    };
    for value in [
        "",
        "model'\\",
        "literal\\t\\n\\N",
        "模型\tname\nline\rreturn\0end",
        "' OR 1 = 1 --",
        "x&param_value=other%20value+",
    ] {
        let rows = ch
            .query_with_params(
                "SELECT {value:String} AS value, {number:UInt32} AS number",
                &[("value", value), ("number", "42")],
            )
            .await
            .unwrap();
        assert_eq!(rows[0]["value"], value);
        assert_eq!(rows[0]["number"], 42);
    }
}

fn outbox_payload(user_id: i64, request_id: Uuid, amount: i64) -> Value {
    json!({
        "request_id": request_id,
        "user_id": user_id,
        "api_key_id": 1,
        "group": "default",
        "model": "m-ch-test",
        "channel_id": 1,
        "channel_key_id": 1,
        "log_type": 2,
        "status": 20,
        "prompt_tokens": 100,
        "cached_tokens": 0,
        "completion_tokens": 20,
        "reasoning_tokens": 0,
        "amount_micro": amount,
        "original_amount_micro": amount,
        "discount_micro": 0,
        "pricing_epoch": 1,
        "latency_ms": 12,
        "ttft_ms": 5,
        "is_stream": true,
        "retry_count": 0,
        "failover_count": 0,
        "error_code": null,
        "upstream_status": 200,
        "upstream_request_id": "up-1",
        "node": "test-node"
    })
}

async fn insert_outbox(pg: &PgPool, payload: &Value) -> i64 {
    sqlx::query_scalar!(
        r#"INSERT INTO billing_outbox (topic, payload) VALUES ('billing.completed', $1) RETURNING id"#,
        payload
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
    panic!("outbox 100 批未排空");
}

/// drain 到本用例自己那行进入 CH 为止。
///
/// outbox 是全局队列且 `process_once` 用 `FOR UPDATE SKIP LOCKED`：并行的其它
/// 测试二进制持锁时，本用例的 drain 会提前收敛到 0，此刻自己播的行还没进 CH。
/// 故必须"drain + 查询"重试，而不是 drain 一次就断言（同 console_stats::poll_row）。
async fn drain_until_visible(pg: &PgPool, ch: &ChClient, user_id: i64) -> i64 {
    for _ in 0..50 {
        drain(pg, ch).await;
        let n = ch_count(ch, user_id).await;
        if n > 0 {
            return n;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    0
}

async fn ch_count(ch: &ChClient, user_id: i64) -> i64 {
    let rows = ch
        .query_json_each_row(&format!(
            "SELECT count() AS c FROM request_log_raw WHERE user_id = {user_id}"
        ))
        .await
        .unwrap();
    rows.first()
        .and_then(|r| r.get("c"))
        .and_then(|v| {
            v.as_str()
                .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
        })
        .unwrap_or(0)
}

fn old_speech_payload(user_id: i64) -> Value {
    let mut payload = outbox_payload(user_id, Uuid::new_v4(), 22);
    for (field, value) in [
        ("endpoint", json!("/v1/audio/speech")),
        ("upstream_endpoint", json!("/v1/audio/speech")),
        ("is_stream", json!(false)),
        ("prompt_tokens", json!(11)),
        ("completion_tokens", json!(0)),
        ("pricing_epoch", json!(41)),
        ("original_amount_micro", json!(30)),
        ("discount_micro", json!(8)),
        ("upstream_cost_micro", json!(7)),
        ("upstream_cost_known", json!(true)),
    ] {
        payload[field] = value;
    }
    let snapshot = json!({"epoch":41,"mode":"ratio","model_ratio":1,
        "completion_ratio":1,"cache_ratio":1,"group":"default","group_ratio":1,
        "user_multiplier":1,"rules":[],"final_unit_price_input_per_1m_usd":2});
    payload["ratio_snapshot"] = json!(snapshot.to_string());
    payload
}

fn assert_old_speech_projection(payload: &Value) -> Value {
    let normalized = chsink::js_payload_to_ch_row(payload);
    assert_eq!(normalized["prompt_tokens"], 0);
    assert_eq!(normalized["input_unit"], "characters");
    assert_eq!(normalized["input_characters"], 11);
    assert_eq!(normalized["historical_prompt_units"], 11);
    assert_eq!(
        normalized["input_unit_basis"],
        okapi_store::legacy_speech::BASIS
    );
    assert_eq!(normalized["ratio_snapshot"], payload["ratio_snapshot"]);
    for (field, expected) in [
        ("amount_micro", 22),
        ("original_amount_micro", 30),
        ("discount_micro", 8),
        ("upstream_cost_micro", 7),
        ("pricing_epoch", 41),
    ] {
        assert_eq!(normalized[field], expected, "{field}");
    }
    normalized
}

async fn assert_old_speech_materialized(ch: &ChClient, user_id: i64, normalized: &Value) {
    let raw = ch.query_json_each_row(&format!(
        "SELECT prompt_tokens,input_unit,input_characters,historical_prompt_units,input_unit_basis,ratio_snapshot,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_epoch FROM request_log_raw WHERE user_id={user_id}"
    )).await.unwrap();
    for field in [
        "prompt_tokens",
        "input_unit",
        "input_characters",
        "historical_prompt_units",
        "input_unit_basis",
        "ratio_snapshot",
    ] {
        assert_eq!(raw[0][field], normalized[field], "{field}");
    }
    for field in [
        "amount_micro",
        "original_amount_micro",
        "discount_micro",
        "upstream_cost_micro",
        "pricing_epoch",
    ] {
        assert_eq!(number(&raw[0], field), number(normalized, field), "{field}");
    }
    let totals = ch.query_json_each_row(&format!(
        "SELECT sumMerge(tokens) AS tokens,countMerge(requests) AS requests,sumMerge(amount) AS amount FROM mv_user_day WHERE user_id={user_id}"
    )).await.unwrap();
    assert_eq!(number(&totals[0], "tokens"), 0);
    assert_eq!(number(&totals[0], "requests"), 1);
    assert_eq!(number(&totals[0], "amount"), 22);
    let units = ch.query_json_each_row(&format!(
        "SELECT sumIfMerge(unit_characters) AS characters,countIfMerge(unit_character_n) AS known FROM mv_input_units_5min WHERE user_id={user_id}"
    )).await.unwrap();
    assert_eq!(number(&units[0], "characters"), 11);
    assert_eq!(number(&units[0], "known"), 1);
    let rate = ch.query_json_each_row(&format!(
        "SELECT countIfMerge(known_units) AS known,countIfMerge(token_requests) AS tokens,countIfMerge(samples) AS samples FROM mv_output_rate_5min WHERE user_id={user_id}"
    )).await.unwrap();
    assert_eq!(number(&rate[0], "known"), 1);
    assert_eq!(number(&rate[0], "tokens"), 0);
    assert_eq!(number(&rate[0], "samples"), 0);
}

fn number(row: &Value, field: &str) -> i64 {
    row[field]
        .as_i64()
        .or_else(|| row[field].as_str().and_then(|v| v.parse().ok()))
        .unwrap()
}

#[tokio::test]
async fn old_speech_outbox_replay_separates_characters_before_materialized_views() {
    let Some((pg, ch)) = setup().await else {
        return;
    };
    drain(&pg, &ch).await;
    let user_id = 2_000_000_000 + i64::from(rand_suffix());
    let mut payload = old_speech_payload(user_id);
    let normalized = assert_old_speech_projection(&payload);
    let id = insert_outbox(&pg, &payload).await;
    assert_eq!(drain_until_visible(&pg, &ch, user_id).await, 1);
    drain(&pg, &ch).await;
    assert_eq!(ch_count(&ch, user_id).await, 1);
    let delivered = sqlx::query!(
        r#"SELECT status, retry_count FROM billing_outbox WHERE id = $1"#,
        id
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(delivered.status, 1);
    assert_old_speech_materialized(&ch, user_id, &normalized).await;
    payload["input_unit"] = json!("tokens");
    let explicit = chsink::js_payload_to_ch_row(&payload);
    assert_eq!(explicit["prompt_tokens"], 11);
    assert!(explicit["historical_prompt_units"].is_null());
}

/// 管道端到端：outbox 行进入 CH 明细与 MV；写失败退避重试并最终入 DLQ。
#[tokio::test]
async fn chsink_pipeline_then_dlq() {
    let Some((pg, ch)) = setup().await else {
        return;
    };

    // —— 阶段 1：正常管道 ——
    drain(&pg, &ch).await;
    let user_id = 1_000_000_000 + i64::from(rand_suffix());
    let request_id = Uuid::new_v4();
    insert_outbox(&pg, &outbox_payload(user_id, request_id, 4242)).await;

    assert_eq!(
        drain_until_visible(&pg, &ch, user_id).await,
        1,
        "明细应恰好一行"
    );
    let mv = ch
        .query_json_each_row(&format!(
            "SELECT sumMerge(amount) AS a, countMerge(requests) AS r \
             FROM mv_user_day WHERE user_id = {user_id} GROUP BY user_id, day"
        ))
        .await
        .unwrap();
    let amount = mv
        .first()
        .and_then(|r| r.get("a"))
        .and_then(|v| {
            v.as_str()
                .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
        })
        .unwrap_or(0);
    assert_eq!(amount, 4242, "mv_user_day 聚合金额必须一致");

    // —— 阶段 2：CH 不可达 → 退避重试 → DLQ 终态 ——
    let bad = ChClient::new("http://127.0.0.1:9", "okapi").unwrap();
    let dead_request = Uuid::new_v4();
    let dead_id = insert_outbox(&pg, &outbox_payload(user_id, dead_request, 1)).await;
    for _ in 0..5 {
        let _ = chsink::process_once(&pg, &bad).await.unwrap();
        // 消除退避等待，直接允许下一次重试
        sqlx::query!(
            r#"UPDATE billing_outbox SET next_retry_at = now() - interval '1 second' WHERE id = $1 AND status = 0"#,
            dead_id
        )
        .execute(&pg)
        .await
        .unwrap();
        sqlx::query!(
            "UPDATE billing_ch_batches SET next_retry_at=now()-interval '1 second' WHERE id=(SELECT ch_batch_id FROM billing_outbox WHERE id=$1) AND status=0",
            dead_id
        ).execute(&pg).await.unwrap();
    }
    let row = sqlx::query!(
        r#"SELECT status, retry_count FROM billing_outbox WHERE id = $1"#,
        dead_id
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(row.status, 2, "重试超限应转终态");
    assert!(row.retry_count >= 5);

    let dlq = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_dlq
           WHERE source = 'chsink' AND payload->>'request_id' = $1"#,
        dead_request.to_string()
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(dlq >= 1, "DLQ 必须留痕");

    // 收尾：把本用例造的死信删掉。它是故意打 127.0.0.1:9 造出来的，留在共享开发库里
    // 运维页会一直挂着"N 条待处理"，而站长根本无从判断那是真故障还是测试残渣。
    sqlx::query!(
        r#"DELETE FROM billing_dlq
           WHERE source = 'chsink' AND payload->>'request_id' = $1"#,
        dead_request.to_string()
    )
    .execute(&pg)
    .await
    .unwrap();
    // 对应的 outbox 行也删掉：它停在 status=2 终态，运维看到的是一条永远发不出去的
    // 积压行，而它其实只是本用例故意造的
    sqlx::query!(
        r#"DELETE FROM billing_outbox WHERE payload->>'request_id' = $1"#,
        dead_request.to_string()
    )
    .execute(&pg)
    .await
    .unwrap();
    // 把阶段 2 波及的其他 pending 行交还真实 CH 排空，不影响并行测试
    drain(&pg, &ch).await;
}

/// 批次幂等：同 dedup_token 重复写入被 CH 去重（含 MV 传导）。
#[tokio::test]
async fn ch_dedup_by_token() {
    let Some((_pg, ch)) = setup().await else {
        return;
    };
    let user_id = 2_000_000_000 + i64::from(rand_suffix());
    let row = json!({
        "ts": chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        "request_id": Uuid::new_v4(),
        "user_id": user_id,
        "amount_micro": 777,
        "log_type": 2
    });
    let token = format!("dedup-test-{user_id}");
    ch.insert_json_each_row("request_log_raw", std::slice::from_ref(&row), &token)
        .await
        .unwrap();
    ch.insert_json_each_row("request_log_raw", std::slice::from_ref(&row), &token)
        .await
        .unwrap();
    assert_eq!(ch_count(&ch, user_id).await, 1, "同 token 批次必须去重");
}

fn rand_suffix() -> u32 {
    // 测试内轻量随机（不引入 rand dev 依赖）：取 uuid 前 4 字节
    let bytes = *Uuid::new_v4().as_bytes();
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000
}
