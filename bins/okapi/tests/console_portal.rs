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
        // cached 40/100：让门户 Token 构成与缓存命中率有非零值可断言
        // （cache_ratio=1 时金额不受影响，既有 240/笔 断言照旧成立）
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":40}}}),
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
    // 员工 key 起名：日志汇总视角靠 key 名分辨"这笔是谁发的"
    for (id, name) in [(key_a, "emp-a"), (key_b, "emp-b")] {
        sqlx::query!("UPDATE api_keys SET name = $2 WHERE id = $1", id, name)
            .execute(&pg)
            .await
            .unwrap();
    }
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
    let me = get(token_b.clone(), "/api/me".into()).await;
    assert_eq!(me["balance_micro"], 10_000_000 - 720);
    // 余额有效期：未设置为 null；设置后用户必须能在 /api/me 看到（独有机制不可隐形）
    assert!(me["balance_expires_at"].is_null(), "{me}");
    sqlx::query!(
        "UPDATE users SET balance_expires_at = now() + interval '3 days' WHERE id = $1",
        partner
    )
    .execute(&pg)
    .await
    .unwrap();
    let me_exp = get(token_b.clone(), "/api/me".into()).await;
    assert!(
        me_exp["balance_expires_at"]
            .as_str()
            .is_some_and(|s| s.contains('T')),
        "应为 RFC3339 时间：{me_exp}"
    );

    // 门户看板单一数据源（mv_key_model_day）：key 视角员工 A 两笔，
    // token 四轴按笔累加（100 prompt 含 40 cached / 20 completion），缓存命中 40%
    let bd_a = get(token_a.clone(), "/api/me/stats/breakdown?days=2".into()).await;
    assert_eq!(bd_a["scope"], "key");
    let total = &bd_a["total"];
    assert_eq!(total["requests"], 2, "{bd_a}");
    assert_eq!(total["prompt_tokens"], 200);
    assert_eq!(total["cached_tokens"], 80);
    assert_eq!(total["completion_tokens"], 40);
    assert_eq!(total["tokens"], 240, "tokens = prompt + completion");
    assert_eq!(total["amount_micro"], 480);
    assert_eq!(total["cache_hit_bp"], 4_000, "80/200 = 40%");
    // 2 笔 / 2880 分钟 = 694 micro-RPM：百万分位才不会被整数截断成 0
    assert_eq!(total["avg_rpm_micro"], 2_000_000 / 2_880);
    // 当前速率来自限流器计数器（reserve 时 INCR）：两笔刚发出，本分钟或已跨分钟，
    // 但当日 RPD 计数一定 ≥ 2；上限列随 key 配置（此处未配 → null）
    let live = &bd_a["live"];
    assert!(
        live["rpd"].as_i64().unwrap() >= 2,
        "限流器当日计数应含两笔：{live}"
    );
    assert!(live["rpm_limit"].is_null(), "未配上限应为 null 而非 0");
    let rows = bd_a["data"].as_array().unwrap();
    assert!(
        rows.iter()
            .all(|r| r["model"].as_str() == Some(model.as_str())),
        "行粒度 = (day, model)：{rows:?}"
    );

    // user 视角：合作商三笔汇总；员工 B 的 key 视角只有自己一笔
    let bd_user = get(
        token_a.clone(),
        "/api/me/stats/breakdown?days=2&scope=user".into(),
    )
    .await;
    assert_eq!(bd_user["total"]["requests"], 3, "{bd_user}");
    assert_eq!(bd_user["total"]["amount_micro"], 720);
    assert!(bd_user["live"].is_null(), "汇总视角没有单一 key 上限可对照");
    let bd_b = get(token_b.clone(), "/api/me/stats/breakdown?days=2".into()).await;
    assert_eq!(bd_b["total"]["requests"], 1, "{bd_b}");
    // 钱包级窗口消费不随 scope 变：员工 A/B 的 key 视角与 user 视角都是合作商三笔 720——
    // "余额还能撑几天"必须按整个钱包算，否则员工按自己那把 key 会把寿命高估三倍
    for bd in [&bd_a, &bd_b, &bd_user] {
        assert_eq!(bd["wallet_window_spend_micro"], 720, "{bd}");
    }

    // 日志页与 usage/breakdown 同一套 scope 语义：员工 B 缺省只见自己那一笔
    // （此前只按 user_id 过滤，员工能翻到同一钱包下所有人的请求）
    let logs_b = get(token_b.clone(), "/api/me/logs".into()).await;
    assert_eq!(logs_b["scope"], "key");
    let rows_b = logs_b["data"].as_array().unwrap();
    assert_eq!(rows_b.len(), 1, "员工 B key 视角只见自己：{logs_b}");
    assert_eq!(rows_b[0]["api_key_id"], key_b);
    assert_eq!(rows_b[0]["key_name"], "emp-b", "key 名回填");
    assert_eq!(rows_b[0]["usage"]["cached_tokens"], 40);
    assert!(
        rows_b[0]["ttft_ms"].is_number(),
        "流式请求应带首字耗时：{}",
        rows_b[0]
    );
    // 合作商汇总视角三笔全见，且能按 key 名分辨是谁发的
    let logs_all = get(token_b.clone(), "/api/me/logs?scope=user".into()).await;
    let names: Vec<&str> = logs_all["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["key_name"].as_str())
        .collect();
    assert_eq!(names.len(), 3, "{logs_all}");
    assert_eq!(names.iter().filter(|n| **n == "emp-a").count(), 2);
    // 过滤：只看失败 → 全成功应为空；按模型精确匹配 → 命中
    let none = get(
        token_b.clone(),
        "/api/me/logs?scope=user&errors_only=true".into(),
    )
    .await;
    assert_eq!(none["data"].as_array().unwrap().len(), 0);
    let by_model = get(token_b, format!("/api/me/logs?scope=user&model={model}")).await;
    assert_eq!(by_model["data"].as_array().unwrap().len(), 3);
}
