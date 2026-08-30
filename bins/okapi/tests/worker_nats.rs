//! M2 NATS 传输拆分验收：outbox → relay → JetStream → chsink(JS) → ClickHouse，
//! 以及 pricing.epoch 广播即时热更（30s 轮询兜底之上的主通道）。
//! 需要 OKAPI_NATS_URL + OKAPI_CLICKHOUSE_URL（scripts/dev-deps.sh up）；未配置时软跳过。
//! 注：CI 的 GitHub services 无法给 nats 镜像传 `-js` 参数，因此 CI 不设 OKAPI_NATS_URL，
//! 本文件在 CI 软跳过，由本地/dev 环境覆盖。

use okapi::worker::nats_relay;
use okapi_store::ChClient;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct NatsEnv {
    pg: PgPool,
    ch: ChClient,
    js: async_nats::jetstream::Context,
    nats_url: String,
    redis_url: String,
    database_url: String,
}

async fn setup() -> Option<NatsEnv> {
    dotenvy::dotenv().ok();
    let Ok(nats_url) = std::env::var("OKAPI_NATS_URL") else {
        eprintln!("跳过：未配置 OKAPI_NATS_URL");
        return None;
    };
    let Ok(ch_url) = std::env::var("OKAPI_CLICKHOUSE_URL") else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return None;
    };
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url =
        std::env::var("OKAPI_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_owned());
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let ch = ChClient::new(&ch_url, "okapi").unwrap();
    assert!(ch.ping().await, "ClickHouse 不可达：{ch_url}");
    ch.ensure_schema().await.unwrap();
    let client = match async_nats::connect(&nats_url).await {
        Ok(c) => c,
        Err(err) => {
            eprintln!("跳过：NATS 不可达（{err}）");
            return None;
        }
    };
    let js = nats_relay::ensure_topology(&client).await.unwrap();
    Some(NatsEnv {
        pg,
        ch,
        js,
        nats_url,
        redis_url,
        database_url,
    })
}

fn outbox_payload(user_id: i64, request_id: Uuid, amount: i64) -> Value {
    json!({
        "request_id": request_id,
        "user_id": user_id,
        "api_key_id": 1,
        "group": "default",
        "model": "m-nats-test",
        "channel_id": 1,
        "channel_key_id": 1,
        "log_type": 2,
        "status": 20,
        "prompt_tokens": 50,
        "cached_tokens": 0,
        "completion_tokens": 10,
        "reasoning_tokens": 0,
        "amount_micro": amount,
        "original_amount_micro": amount,
        "discount_micro": 0,
        "pricing_epoch": 1,
        "latency_ms": 8,
        "ttft_ms": 3,
        "is_stream": false,
        "retry_count": 0,
        "failover_count": 0,
        "error_code": null,
        "upstream_status": 200,
        "upstream_request_id": "up-nats-1",
        "node": "test-node"
    })
}

async fn ch_rows(ch: &ChClient, user_id: i64) -> Vec<Value> {
    ch.query_json_each_row(&format!(
        "SELECT request_id, amount_micro, model FROM request_log_raw WHERE user_id = {user_id}"
    ))
    .await
    .unwrap()
}

/// 端到端：outbox pending → relay 发布并标记 → JS 消费者批写 CH（seq 区间 dedup）。
#[tokio::test]
async fn relay_pipeline_outbox_to_clickhouse() {
    let Some(env) = setup().await else {
        return;
    };
    let user_id = 3_000_000_000 + i64::from(rand_suffix());
    let request_id = Uuid::new_v4();
    let outbox_id = sqlx::query_scalar!(
        r#"INSERT INTO billing_outbox (topic, payload) VALUES ('billing.completed', $1) RETURNING id"#,
        outbox_payload(user_id, request_id, 9900)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();

    // relay：直到本行被发布（并行测试可能同时投递其他行，均属正常）
    let mut relayed = false;
    for _ in 0..20 {
        nats_relay::relay_once(&env.pg, &env.js).await.unwrap();
        let status = sqlx::query_scalar!(
            r#"SELECT status FROM billing_outbox WHERE id = $1"#,
            outbox_id
        )
        .fetch_one(&env.pg)
        .await
        .unwrap();
        if status == 1 {
            relayed = true;
            break;
        }
    }
    assert!(relayed, "outbox 行应被 relay 标记 published");

    // JS 消费者：直到该 user 的行落入 CH
    let mut found = Vec::new();
    for _ in 0..30 {
        nats_relay::chsink_js_once(&env.pg, &env.js, &env.ch)
            .await
            .unwrap();
        found = ch_rows(&env.ch, user_id).await;
        if !found.is_empty() {
            break;
        }
    }
    assert_eq!(found.len(), 1, "CH 应恰好一行（seq 区间 dedup 幂等）");
    let row = &found[0];
    assert_eq!(
        row.get("request_id").and_then(Value::as_str),
        Some(request_id.to_string().as_str())
    );
    assert_eq!(
        row.get("model").and_then(Value::as_str),
        Some("m-nats-test")
    );

    // 再空转几轮：不得重复写入（消息已 ack；且同批 token 幂等）
    for _ in 0..3 {
        nats_relay::chsink_js_once(&env.pg, &env.js, &env.ch)
            .await
            .unwrap();
    }
    assert_eq!(ch_rows(&env.ch, user_id).await.len(), 1, "ack 后不得重复");
}

/// pricing.epoch 广播：订阅者收到消息后立即热更 PriceBook（不等 30s 轮询）。
#[tokio::test]
async fn epoch_broadcast_hot_reload() {
    let Some(env) = setup().await else {
        return;
    };
    let state = okapi::gateway::build_state(
        &env.database_url,
        &env.redis_url,
        "test-node",
        None,
        Some(&env.nats_url),
    )
    .await
    .unwrap();
    assert!(state.nats.is_some(), "NATS 应已连接");
    okapi::gateway::spawn_epoch_subscriber(state.clone());
    // 订阅建立是异步的，稍等避免消息早于订阅
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let suffix = rand_suffix();
    let admin_id = okapi_store::provision::create_user(&env.pg, &format!("nats-admin-{suffix}"))
        .await
        .unwrap();
    let epoch =
        okapi_store::admin::publish_epoch(&env.pg, admin_id, &json!({"reason": "nats-test"}))
            .await
            .unwrap();
    assert!(epoch > state.pricebook.epoch(), "新 epoch 应大于当前");

    // 模拟 console 广播（与 console publish_pricing 的 best-effort 发布一致）
    let client = state.nats.clone().unwrap();
    client
        .publish("pricing.epoch", epoch.to_string().into())
        .await
        .unwrap();

    let mut reloaded = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if state.pricebook.epoch() >= epoch {
            reloaded = true;
            break;
        }
    }
    assert!(reloaded, "订阅者应在广播后 4s 内热更（无需 30s 轮询）");
}

fn rand_suffix() -> u32 {
    let bytes = *Uuid::new_v4().as_bytes();
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000
}
