//! 模型级降级链验收（DESIGN §3.4.1 / IMPLEMENTATION §11.9 开放项）：
//! 零可用候选 → 按序改投 fallback_models，按实际服务模型计费，快照记 requested_model；
//! 上游 4xx 不触发；单跳不递归；key 白名单同样约束降级模型。
//! 依赖 .env（scripts/dev-deps.sh up）。
//!
//! 注意时序：PriceBook 在 build_state 时全量编译，模型/定价必须建在网关启动之前
//! （测试内不等 30s epoch 轮询）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

/// 健康 mock 上游：回显请求 model，固定 usage 100/20。
async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion","model": req["model"],
        "choices":[{"index":0,"message":{"role":"assistant","content":"hi"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":20,
                 "prompt_tokens_details":{"cached_tokens":0}}
    }))
    .into_response()
}

/// 故障 mock 上游：恒回 400（用户参数类错误，按 §3.6 直接透传不重试）。
async fn mock_bad_request() -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({"error":{"message":"bad prompt","type":"invalid_request_error"}})),
    )
        .into_response()
}

/// PG 侧测试床（网关启动前的拓扑构造期）。
struct TestBed {
    pg: PgPool,
    user_id: i64,
    token: String,
    suffix: String,
}

impl TestBed {
    fn model(&self, name: &str) -> String {
        format!("{name}-{}", self.suffix)
    }

    async fn priced_model(&self, name: &str, ratio: &str) -> String {
        let full = self.model(name);
        okapi_store::provision::create_model_ratio(&self.pg, &full, ratio, "1.0", "1.0")
            .await
            .unwrap();
        full
    }

    /// 建一条服务 `models` 的健康渠道，指向给定 mock。
    async fn channel_for(&self, tag: &str, mock: SocketAddr, models: &[&str]) {
        okapi_store::provision::create_channel(
            &self.pg,
            &format!("fb-ch-{tag}-{}", self.suffix),
            "openai",
            &format!("http://{mock}/v1"),
            "mock-credential",
            models,
            true,
            None,
        )
        .await
        .unwrap();
    }

    /// 设置降级链（直写库，绕过 console 校验以便构造"链上有幽灵模型"的场景）。
    async fn set_fallbacks(&self, model: &str, chain: &[String]) {
        sqlx::query!(
            r#"UPDATE models SET fallback_models = $2 WHERE model_name = $1"#,
            model,
            json!(chain)
        )
        .execute(&self.pg)
        .await
        .unwrap();
    }
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

async fn setup_bed() -> TestBed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let suffix = Uuid::new_v4().simple().to_string()[..12].to_owned();

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("fb-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-fb-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-fb")
        .await
        .unwrap();

    TestBed {
        pg,
        user_id,
        token,
        suffix,
    }
}

/// 拓扑建完后启动网关（此刻编译 PriceBook）并注资。
async fn start_gateway(bed: &TestBed) -> SocketAddr {
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(bed.user_id, Money::from_micros(50_000_000))
        .await
        .unwrap();
    serve(gateway::router(state)).await
}

async fn chat(gw: SocketAddr, token: &str, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .bearer_auth(token)
        .json(&json!({
            "model": model, "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap()
}

/// 首条 committed 记录 (model_name, amount_micro, pricing_snapshot)。
async fn wait_committed(pg: &PgPool, user_id: i64) -> (String, i64, Value) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT model_name, amount_micro, pricing_snapshot
               FROM billing_records WHERE user_id = $1 AND status = 20"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (
                r.model_name,
                r.amount_micro,
                r.pricing_snapshot.unwrap_or_default(),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("未等到 committed 记录");
}

/// 零候选 → 按序降级：按实际服务模型（2.0 倍率）计费，
/// 快照记 requested_model，响应体 model 为实际模型。
#[tokio::test]
async fn falls_back_and_bills_actual_model_on_zero_candidates() {
    let bed = setup_bed().await;
    // A 有定价但无任何渠道；B 定价 2.0 且有健康渠道
    let a = bed.priced_model("fb-a", "1.0").await;
    let b = bed.priced_model("fb-b", "2.0").await;
    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    bed.channel_for("ok", mock, &[b.as_str()]).await;
    bed.set_fallbacks(&a, std::slice::from_ref(&b)).await;
    let gw = start_gateway(&bed).await;

    let resp = chat(gw, &bed.token, &a).await;
    assert_eq!(resp.status(), 200, "零候选应降级成功而非 503");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["model"].as_str().unwrap(),
        b,
        "响应体 model 应为实际服务的模型（OpenAI 兼容语义）"
    );

    let (model_name, amount, snapshot) = wait_committed(&bed.pg, bed.user_id).await;
    assert_eq!(model_name, b, "应按实际服务模型记账");
    // 1.0 倍率基线 240 micro（100 prompt + 20 completion）；2.0 倍率 = 480
    assert_eq!(amount, 480, "应按降级模型 2.0 倍率计费，两个方向都不算错钱");
    assert_eq!(
        snapshot["requested_model"].as_str().unwrap(),
        a,
        "快照应记原请求模型（账单可解释）"
    );
}

/// 上游 4xx：请求模型有候选、打过了没打通 → 原样透传，不降级不藏错。
#[tokio::test]
async fn upstream_4xx_does_not_trigger_fallback() {
    let bed = setup_bed().await;
    let a = bed.priced_model("4xx-a", "1.0").await;
    let b = bed.priced_model("4xx-b", "1.0").await;
    let bad = serve(Router::new().route("/v1/chat/completions", post(mock_bad_request))).await;
    let ok = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    bed.channel_for("bad", bad, &[a.as_str()]).await;
    bed.channel_for("ok", ok, &[b.as_str()]).await;
    bed.set_fallbacks(&a, std::slice::from_ref(&b)).await;
    let gw = start_gateway(&bed).await;

    let resp = chat(gw, &bed.token, &a).await;
    assert_eq!(resp.status(), 400, "4xx 应原样透传给客户端");
    // 结算是后台任务，给它落盘窗口再断言"没有任何 committed 记录"
    tokio::time::sleep(Duration::from_millis(500)).await;
    let committed = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_records
           WHERE user_id = $1 AND status = 20"#,
        bed.user_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(committed, 0, "4xx 不该降级到 B 产生任何消费");
}

/// 单跳：A→[B]，B→[C]，A/B 都零候选、C 健康 → 不递归穿透 B 的链，最终 503。
#[tokio::test]
async fn fallback_is_single_hop() {
    let bed = setup_bed().await;
    let a = bed.priced_model("hop-a", "1.0").await;
    let b = bed.priced_model("hop-b", "1.0").await;
    let c = bed.priced_model("hop-c", "1.0").await;
    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    bed.channel_for("ok", mock, &[c.as_str()]).await;
    bed.set_fallbacks(&a, std::slice::from_ref(&b)).await;
    bed.set_fallbacks(&b, std::slice::from_ref(&c)).await;
    let gw = start_gateway(&bed).await;

    let resp = chat(gw, &bed.token, &a).await;
    assert_eq!(resp.status(), 503, "B 的链不该被递归消费，A 应以零候选收场");
}

/// 链上的幽灵条目（不存在的模型）与无价条目被跳过，后续可用环仍生效。
#[tokio::test]
async fn chain_skips_ghost_and_unpriced_entries() {
    let bed = setup_bed().await;
    let a = bed.priced_model("skip-a", "1.0").await;
    let c = bed.priced_model("skip-c", "1.0").await;
    // 有模型行、有渠道、但无定价 → 降级不该投它（结算会 fail-closed）
    let unpriced = bed.model("skip-unpriced");
    sqlx::query!(r#"INSERT INTO models (model_name) VALUES ($1)"#, &unpriced)
        .execute(&bed.pg)
        .await
        .unwrap();
    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    bed.channel_for("ok", mock, &[unpriced.as_str(), c.as_str()])
        .await;
    bed.set_fallbacks(
        &a,
        &[format!("ghost-{}", bed.suffix), unpriced.clone(), c.clone()],
    )
    .await;
    let gw = start_gateway(&bed).await;

    let resp = chat(gw, &bed.token, &a).await;
    assert_eq!(resp.status(), 200);
    let (model_name, amount, _) = wait_committed(&bed.pg, bed.user_id).await;
    assert_eq!(model_name, c, "应跳过幽灵与无价条目，投链上首个可投模型");
    assert_eq!(amount, 240);
}

/// key 模型白名单同样约束降级模型：白名单只有 A 时，降级到 B 等于绕过白名单。
#[tokio::test]
async fn key_allowlist_gates_fallback_targets() {
    let bed = setup_bed().await;
    let a = bed.priced_model("al-a", "1.0").await;
    let b = bed.priced_model("al-b", "1.0").await;
    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    bed.channel_for("ok", mock, &[b.as_str()]).await;
    bed.set_fallbacks(&a, std::slice::from_ref(&b)).await;
    sqlx::query!(
        r#"UPDATE api_keys SET model_allowlist = $2 WHERE user_id = $1"#,
        bed.user_id,
        json!([a])
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    let gw = start_gateway(&bed).await;

    let resp = chat(gw, &bed.token, &a).await;
    assert_eq!(
        resp.status(),
        503,
        "降级模型不在 key 白名单内时不可投，降级不是白名单后门"
    );
}
