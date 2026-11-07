//! /v1/embeddings 验收：prompt-only 计费、failover、透传。
//! 依赖 .env 的 DATABASE_URL / OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi_domain::Money;
use okapi_gateway as gateway;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

#[path = "support/published_pricing.rs"]
mod published_pricing;

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert!(req["model"].as_str().unwrap().starts_with("m-"));
    axum::Json(json!({
        "object": "list",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
        "model": req["model"],
        "usage": {"prompt_tokens": if req["input"] == "overflow-settlement" { 2_000_000_000_u32 } else { 120 }, "total_tokens": if req["input"] == "overflow-settlement" { 2_000_000_000_u32 } else { 120 }}
    }))
    .into_response()
}

async fn mock_fail() -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"error": {"message": "boom"}})),
    )
        .into_response()
}

async fn mock_rerank(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["query"], "best llm gateway", "请求体原样透传");
    axum::Json(json!({
        "results": [{"index": 0, "relevance_score": 0.9}, {"index": 1, "relevance_score": 0.1}],
        "usage": {"prompt_tokens": 120, "total_tokens": 120}
    }))
    .into_response()
}

/// 慢上游：让请求在途一会儿，看得到在途租约。
async fn mock_slow(body: axum::body::Bytes) -> axum::response::Response {
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    mock_ok(body).await
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/v1/embeddings", post(mock_ok))
        .route("/ok/v1/rerank", post(mock_rerank))
        .route("/slow/v1/embeddings", post(mock_slow))
        .route("/fail/v1/embeddings", post(mock_fail));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    state: gateway::state::AppState,
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(channels: &[(&str, i32)]) -> TestEnv {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-test-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    for (i, (path, priority)) in channels.iter().enumerate() {
        let (channel_id, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("ch{i}-{suffix}"),
            "openai",
            &format!("http://{mock}{path}"),
            "mock-credential",
            &[model.as_str()],
            false,
            None,
        )
        .await
        .unwrap();
        sqlx::query!(
            "UPDATE channels SET priority = $2 WHERE id = $1",
            channel_id,
            priority
        )
        .execute(&pg)
        .await
        .unwrap();
    }

    published_pricing::publish(&pg, user_id).await;
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();

    let app = gateway::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // 带对端地址：结算记 client_ip 的断言要用
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    TestEnv {
        state,
        pg,
        gateway: addr,
        token,
        user_id,
        model,
    }
}

/// A price overflow after a successful upstream response refunds and logs the failure.
#[tokio::test]
async fn pricing_failure_refunds_and_records_failed_terminal() {
    let env = setup(&[("/ok/v1", 0)]).await;
    sqlx::query("UPDATE model_pricing SET model_ratio=999999 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&env.model).execute(&env.pg).await.unwrap();
    assert!(
        okapi_store::admin::set_user_multiplier(&env.pg, env.user_id, "9999")
            .await
            .unwrap()
    );
    published_pricing::publish(&env.pg, env.user_id).await;
    env.state.pricebook.replace(
        okapi_kernel::pricing_loader::load_pricebook(&env.pg)
            .await
            .unwrap(),
    );
    env.state
        .ledger
        .credit(env.user_id, Money::from_micros(10_000_000_000_000))
        .await
        .unwrap();
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.model,"input":"overflow-settlement"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 500, "{body}");
    assert_eq!(body["error"]["code"], okapi_api::codes::INTERNAL_ERROR);
    let mut failed = None;
    for _ in 0..50 {
        failed = sqlx::query_as::<_,(i16,i64,String)>("SELECT status,amount_micro,error_code FROM billing_records WHERE user_id=$1 AND log_type=5")
            .bind(env.user_id).fetch_optional(&env.pg).await.unwrap();
        if failed.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        failed,
        Some((40, 0, okapi_api::codes::INTERNAL_ERROR.to_owned()))
    );
    assert_eq!(env.state.ledger.balance(env.user_id).await.unwrap(), before);
}

async fn post_embeddings(env: &TestEnv) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "input": ["hello world", "second doc"]}))
        .send()
        .await
        .unwrap()
}

async fn wait_record(pg: &PgPool, user_id: i64) -> (i16, i64, i32, i16) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens, failover_count
               FROM billing_records WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.status, r.amount_micro, r.prompt_tokens, r.failover_count);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

/// 闭环：prompt-only 计费（120 × 1 × $2/1M = 240 micro）。
#[tokio::test]
async fn embeddings_bills_prompt_only() {
    let env = setup(&[("/ok/v1", 0)]).await;
    let resp = post_embeddings(&env).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "list");
    assert_eq!(body["usage"]["prompt_tokens"], 120);

    let (status, amount, prompt, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt, 120);
    assert_eq!(amount, 240);
    let balance = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events WHERE user_id = $1 AND event_type = 'commit'"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(balance >= 1);
}

/// rerank：同一泛化链路（Jina/Cohere 形状），prompt-only 计费。
#[tokio::test]
async fn rerank_bills_prompt_only() {
    let env = setup(&[("/ok/v1", 0)]).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/rerank", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "query": "best llm gateway",
            "documents": ["okapi is a gateway", "bananas are yellow"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["usage"]["prompt_tokens"], 120, "上游响应原样");

    let (status, amount, prompt, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt, 120);
    assert_eq!(amount, 240);
}

/// 5xx failover 到次优先级渠道，客户端无感。
#[tokio::test]
async fn embeddings_failover_on_transient() {
    let env = setup(&[("/fail/v1", 10), ("/ok/v1", 0)]).await;
    let resp = post_embeddings(&env).await;
    assert_eq!(resp.status(), 200);
    let (status, amount, _, failover) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
    assert_eq!(failover, 1);
}

/// 候选只认注册表里声明了 embeddings 的 provider，渠道还能用 `capabilities.embeddings = false`
/// 关掉这一面：排在前面的那条不会被选上（也不算一次 failover），请求直接落到后面能接的渠道。
#[tokio::test]
async fn embeddings_skip_channels_that_do_not_serve_the_surface() {
    for (provider, capabilities) in [
        ("gemini", json!({})),
        ("custom_pass", json!({})),
        ("openai", json!({"embeddings": false})),
    ] {
        let env = setup(&[("/fail/v1", 10), ("/ok/v1", 0)]).await;
        sqlx::query(
            "UPDATE channels SET provider = $2, capabilities = $3 WHERE models @> $1 AND priority = 10",
        )
        .bind(json!([env.model]))
        .bind(provider)
        .bind(&capabilities)
        .execute(&env.pg)
        .await
        .unwrap();
        let resp = post_embeddings(&env).await;
        assert_eq!(resp.status(), 200, "{provider} {capabilities}");
        let (status, amount, _, failover) = wait_record(&env.pg, env.user_id).await;
        assert_eq!(
            (status, amount, failover),
            (20, 240, 0),
            "{provider} {capabilities}: 不该先打到 /fail 那条"
        );
    }
}

#[tokio::test]
async fn embeddings_failover_uses_the_successful_channel_cost() {
    for (ratio, cost) in [(2250, 540), (0, 0)] {
        let env = setup(&[("/fail/v1", 10), ("/ok/v1", 0)]).await;
        sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE models @> $1")
            .bind(json!([env.model]))
            .bind(json!({"relative_cost_milli":9000}))
            .execute(&env.pg)
            .await
            .unwrap();
        let channel:i64=sqlx::query_scalar("UPDATE channels SET upstream_unit_cost=$2 WHERE models @> $1 AND api_base LIKE '%/ok/v1' RETURNING id")
            .bind(json!([env.model])).bind(json!({"relative_cost_milli":ratio})).fetch_one(&env.pg).await.unwrap();
        let response = post_embeddings(&env).await;
        assert_eq!(response.status(), 200);
        let (_, amount, _, failover) = wait_record(&env.pg, env.user_id).await;
        assert_eq!((amount, failover), (240, 1));
        let row:(i64,i64,i64,Option<i64>,Value,Uuid)=sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,request_id FROM billing_records WHERE user_id=$1 AND log_type=2")
            .bind(env.user_id).fetch_one(&env.pg).await.unwrap();
        assert_eq!((row.0, row.1, row.2, row.3), (240, 240, 0, Some(cost)));
        assert_eq!(
            row.4["upstream_cost_basis"],
            json!({"version":1,"source":"selected_channel","channel_id":channel,"relative_cost_milli":ratio,"list_price_micro":240})
        );
        let event:Value=sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
            .bind(row.5.to_string()).fetch_one(&env.pg).await.unwrap();
        assert_eq!(event["upstream_cost_known"], true);
        assert_eq!(event["upstream_cost_micro"], cost);
        assert_eq!(
            serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
            row.4
        );
        assert_eq!(
            env.state
                .ledger
                .balance(env.user_id)
                .await
                .unwrap()
                .as_micros(),
            10_000_000 - 240
        );
    }
}

async fn key_of(pg: &PgPool, model: &str, priority: i32) -> i64 {
    sqlx::query_scalar(
        "SELECT k.id FROM channel_keys k JOIN channels c ON c.id = k.channel_id
         WHERE c.models @> $1 AND c.priority = $2",
    )
    .bind(json!([model]))
    .bind(priority)
    .fetch_one(pg)
    .await
    .unwrap()
}

async fn spend_today(redis: &fred::clients::Client, key: i64) -> Option<i64> {
    use fred::interfaces::KeysInterface;
    let day = chrono::Utc::now().format("%Y%m%d");
    redis.get(format!("spend:ck:{key}:{day}")).await.unwrap()
}

/// 渠道 key 的当日消费上限对 embeddings 也生效：到顶的高优先级 key 被跳过（不算一次 failover），
/// 请求落到下一把；这次的花费记进那把 key 的当日消费，账上记来源 IP。
#[tokio::test]
async fn embeddings_skip_a_key_over_its_daily_spend_cap_and_count_the_spend() {
    let env = setup(&[("/ok/v1", 10), ("/ok/v1", 0)]).await;
    let capped = key_of(&env.pg, &env.model, 10).await;
    let spare = key_of(&env.pg, &env.model, 0).await;
    sqlx::query("UPDATE channel_keys SET daily_spend_cap_micro = 100 WHERE id = $1")
        .bind(capped)
        .execute(&env.pg)
        .await
        .unwrap();
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    {
        use fred::interfaces::KeysInterface;
        let day = chrono::Utc::now().format("%Y%m%d");
        let _: () = redis
            .set(
                format!("spend:ck:{capped}:{day}"),
                100_i64,
                Some(fred::types::Expiration::EX(600)),
                None,
                false,
            )
            .await
            .unwrap();
    }

    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "input": ["hello world", "second doc"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let (status, amount, _, failover) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(
        (status, amount, failover),
        (20, 240, 0),
        "被闸挡下的候选不算一次尝试"
    );
    let (served, ip): (Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT channel_key_id, host(client_ip) FROM billing_records WHERE user_id = $1 AND log_type = 2",
    )
    .bind(env.user_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(served, Some(spare));
    assert_eq!(ip.as_deref(), Some("127.0.0.1"), "媒体端点也记来源 IP");
    let mut spent = None;
    for _ in 0..50 {
        spent = spend_today(&redis, spare).await;
        if spent.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(spent, Some(240), "embeddings 的花费计入渠道 key 当日消费");
    assert_eq!(
        spend_today(&redis, capped).await,
        Some(100),
        "没用上的 key 不动"
    );
}

/// 所有候选都被 key 闸挡下：503 no_available_channel（与 chat 同口径），预扣退回。
#[tokio::test]
async fn embeddings_answer_503_when_every_key_is_over_its_cap() {
    let env = setup(&[("/ok/v1", 0)]).await;
    let only = key_of(&env.pg, &env.model, 0).await;
    sqlx::query("UPDATE channel_keys SET rpm_limit = 1 WHERE id = $1")
        .bind(only)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(post_embeddings(&env).await.status(), 200);
    let resp = post_embeddings(&env).await;
    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_available_channel");
    let mut held = usize::MAX;
    for _ in 0..50 {
        held = env
            .state
            .ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .len();
        if held == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(held, 0, "被挡下的请求不留预扣");
}

/// 本用例模型的全部渠道 key（按 id 升序）。
async fn keys_of(pg: &PgPool, model: &str) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT k.id FROM channel_keys k JOIN channels c ON c.id = k.channel_id
         WHERE c.models @> $1 ORDER BY k.id",
    )
    .bind(json!([model]))
    .fetch_all(pg)
    .await
    .unwrap()
}

/// 把本用例的渠道放进一个新池（选路策略 `strategy`），API key 钉在这个池上。
async fn pin_to_pool(env: &TestEnv, strategy: &str) {
    let pool = format!("p-{}", &Uuid::new_v4().simple().to_string()[..10]);
    okapi_store::admin::upsert_channel_pool(&env.pg, &pool, "t", strategy, None)
        .await
        .unwrap();
    let channels: Vec<i64> = sqlx::query_scalar("SELECT id FROM channels WHERE models @> $1")
        .bind(json!([env.model]))
        .fetch_all(&env.pg)
        .await
        .unwrap();
    for channel in channels {
        let member = okapi_store::admin::PoolMember {
            pool_code: pool.clone(),
            priority_override: None,
            weight_override: None,
        };
        okapi_store::admin::set_channel_pools(&env.pg, channel, &[member])
            .await
            .unwrap();
    }
    sqlx::query("UPDATE api_keys SET pool_override = $1 WHERE user_id = $2")
        .bind(&pool)
        .bind(env.user_id)
        .execute(&env.pg)
        .await
        .unwrap();
}

/// 某把 key 未到期的在途租约数（与网关读数同一口径）。
async fn in_flight(redis: &fred::clients::Client, key: i64) -> i64 {
    use fred::interfaces::SortedSetsInterface;
    #[allow(clippy::cast_precision_loss)]
    let now = chrono::Utc::now().timestamp_millis() as f64;
    redis
        .zcount(format!("conc:ck:{{{key}}}:v2"), now, f64::MAX)
        .await
        .unwrap()
}

/// 媒体端点按池的选路策略排 failover 序：least_usage 池里当日消费多的 key 让路，
/// 同一层的请求都落到消费少的那把（此前媒体一律加权随机）。
#[tokio::test]
async fn embeddings_follow_a_least_usage_pool() {
    use fred::interfaces::KeysInterface;
    let env = setup(&[("/ok/v1", 0), ("/ok/v1", 0)]).await;
    pin_to_pool(&env, "least_usage").await;
    let keys = keys_of(&env.pg, &env.model).await;
    let (heavy, light) = (keys[0], keys[1]);
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let day = chrono::Utc::now().format("%Y%m%d");
    let _: () = redis
        .set(
            format!("spend:ck:{heavy}:{day}"),
            1_000_000_000_i64,
            Some(fred::types::Expiration::EX(600)),
            None,
            false,
        )
        .await
        .unwrap();
    for _ in 0..4 {
        assert_eq!(post_embeddings(&env).await.status(), 200);
    }
    let mut served: Vec<Option<i64>> = Vec::new();
    for _ in 0..50 {
        served = sqlx::query_scalar(
            "SELECT channel_key_id FROM billing_records WHERE user_id = $1 AND log_type = 2",
        )
        .bind(env.user_id)
        .fetch_all(&env.pg)
        .await
        .unwrap();
        if served.len() == 4 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(served, vec![Some(light); 4]);
}

/// least_concurrency 池：媒体请求和对话一样登记在途（同一份并发租约），结束后退回。
/// 这把 key 没设并发上限，只计数。
#[tokio::test]
async fn embeddings_count_in_flight_in_a_least_concurrency_pool() {
    let env = setup(&[("/slow/v1", 0)]).await;
    pin_to_pool(&env, "least_concurrency").await;
    let key = keys_of(&env.pg, &env.model).await[0];
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let request = tokio::spawn({
        let (gateway, token, model) = (env.gateway, env.token.clone(), env.model.clone());
        async move {
            reqwest::Client::new()
                .post(format!("http://{gateway}/v1/embeddings"))
                .bearer_auth(token)
                .json(&json!({"model": model, "input": "hello"}))
                .send()
                .await
                .unwrap()
                .status()
        }
    });
    // 一直看到请求结束：上游要睡 800ms，这期间租约必须在
    let mut seen = 0;
    while seen == 0 && !request.is_finished() {
        seen = in_flight(&redis, key).await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(seen, 1, "请求在途时这把 key 应登记一份在途");
    assert_eq!(request.await.unwrap(), 200);
    for _ in 0..50 {
        if in_flight(&redis, key).await == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("请求结束后在途租约应退回");
}
