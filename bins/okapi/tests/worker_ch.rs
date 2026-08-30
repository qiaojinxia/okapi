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
    drain(&pg, &ch).await;

    assert_eq!(ch_count(&ch, user_id).await, 1, "明细应恰好一行");
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

    // 收尾：把阶段 2 波及的其他 pending 行交还真实 CH 排空，不影响并行测试
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
