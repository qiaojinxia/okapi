//! 用户门户验收（合作商轻量子账户模式）：
//! 一个钱包主体（合作商）+ 两把员工 key → 各自请求 → key 视角只见自己、
//! user 视角见汇总、/api/me/keys 分账正确。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::worker::chsink;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    use std::fmt::Write as _;
    let chunks = [
        json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
        json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":0}}}),
    ];
    let mut body = String::new();
    for c in chunks {
        let _ = write!(body, "data: {c}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

// 端到端场景单测：分阶段拆函数反而割裂脚本语义
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn partner_employee_keys_see_own_usage() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let Ok(ch_url) = std::env::var("OKAPI_CLICKHOUSE_URL") else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    };

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 合作商钱包主体 + 两把员工 key
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-p-{}", &suffix[..10]);
    let partner = okapi_store::provision::create_user(&pg, &format!("p-{suffix}"))
        .await
        .unwrap();
    let token_a = format!("sk-okapi-emp-a-{suffix}");
    let token_b = format!("sk-okapi-emp-b-{suffix}");
    let key_a = okapi_store::provision::create_api_key(&pg, partner, &hash(&token_a), "sk-a")
        .await
        .unwrap();
    let key_b = okapi_store::provision::create_api_key(&pg, partner, &hash(&token_b), "sk-b")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let mock = serve(Router::new().route("/ok/v1/chat/completions", post(mock_ok))).await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("p-{suffix}"),
        "openai",
        &format!("http://{mock}/ok/v1"),
        "mock",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", Some(&ch_url), None)
        .await
        .unwrap();
    state
        .ledger
        .credit(partner, Money::from_micros(10_000_000))
        .await
        .unwrap();
    okapi_ledger::pg::record_credit(
        &pg,
        partner,
        Money::from_micros(10_000_000),
        "recharge",
        "test",
        json!({}),
    )
    .await
    .unwrap();

    let gw = serve(gateway::router(state.clone())).await;
    let con = serve(console::router(state.clone())).await;
    let ch = okapi_store::ChClient::new(&ch_url, "okapi").unwrap();
    ch.ensure_schema().await.unwrap();

    // 员工 A 两笔、员工 B 一笔
    for (token, n) in [(&token_a, 2), (&token_b, 1)] {
        for _ in 0..n {
            let resp = reqwest::Client::new()
                .post(format!("http://{gw}/v1/chat/completions"))
                .bearer_auth(token)
                .json(&json!({
                    "model": model, "stream": true, "max_tokens": 32,
                    "messages": [{"role":"user","content":"hello portal"}]
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }
    }
    // 等结算落 outbox，再手动泵入 CH（模拟 worker）
    tokio::time::sleep(Duration::from_millis(500)).await;
    for _ in 0..100 {
        if chsink::process_once(&pg, &ch).await.unwrap() == 0 {
            break;
        }
    }

    let get = |token: String, path: String| async move {
        reqwest::Client::new()
            .get(format!("http://{con}{path}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    };

    // key 视角：员工 A 只见自己两笔（2×240=480），员工 B 只见一笔（240）
    let usage_a = get(token_a.clone(), "/api/me/usage?days=2".into()).await;
    assert_eq!(usage_a["scope"], "key");
    assert_eq!(usage_a["total_amount_micro"], 480, "{usage_a}");
    let usage_b = get(token_b.clone(), "/api/me/usage?days=2".into()).await;
    assert_eq!(usage_b["total_amount_micro"], 240, "{usage_b}");

    // user 视角：合作商汇总三笔 720
    let usage_user = get(token_a.clone(), "/api/me/usage?days=2&scope=user".into()).await;
    assert_eq!(usage_user["total_amount_micro"], 720, "{usage_user}");

    // key 分账列表
    let keys = get(token_a.clone(), "/api/me/keys".into()).await;
    let arr = keys["data"].as_array().unwrap();
    let amount_of = |id: i64| {
        arr.iter()
            .find(|k| k["id"].as_i64() == Some(id))
            .map_or(0, |k| {
                k["amount_micro"]
                    .as_i64()
                    .or_else(|| k["amount_micro"].as_str().and_then(|s| s.parse().ok()))
                    .unwrap_or(0)
            })
    };
    assert_eq!(amount_of(key_a), 480);
    assert_eq!(amount_of(key_b), 240);

    // 余额：10_000_000 − 720
    let me = get(token_b, "/api/me".into()).await;
    assert_eq!(me["balance_micro"], 10_000_000 - 720);
}
