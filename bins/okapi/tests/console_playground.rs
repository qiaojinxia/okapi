//! Playground 同源中继 + 站点预设端点验收（IMPLEMENTATION §11.39）。
//!
//! 中继在 console 进程内直接调用数据面处理器：鉴权 / 限流 / 计费与真实 SDK 调用一致。
//! 覆盖：强制流式（body 里 stream:false 也回 SSE）、SSE 逐块透出、记账落在同一把 key、
//! 无 key 401、超 1MB 413；`GET /api/playground/presets` 白名单收口；
//! 请求头 `x-okapi-playground-key` 选用同一用户的另一把令牌（计费 / 白名单按被选令牌走，
//! 别人的令牌 404、没存加密副本 409、缺主密钥 503、头不合法 400）。
//! 依赖 .env（scripts/dev-deps.sh up）。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

/// 测试用主密钥（32 字节十六进制）。
const MASTER: &str = "0909090909090909090909090909090909090909090909090909090909090909";

/// mock OpenAI 上游：只认流式（中继会强制 stream:true），逐块回内容 + usage + [DONE]。
async fn mock_stream(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["stream"], true, "中继必须把请求改成流式");
    if req["model"] == "kimi-k3" {
        assert!(req.get("temperature").is_none());
        assert!(req.get("top_p").is_none());
        assert_eq!(req["reasoning_effort"], "max");
        assert_eq!(req["max_completion_tokens"], 256);
        assert!(req.get("max_tokens").is_none());
    }
    if req["top_p"] == 1 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "error": {
                    "type": "invalid_request_error",
                    "message": "invalid top_p: only 0.95 is allowed for this model; credential mock-credential"
                },
                "request": req,
            })),
        )
            .into_response();
    }
    let chunks = [
        json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-mock",
               "choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}),
        json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-mock",
               "choices":[{"index":0,"delta":{"content":" playground"},"finish_reason":"stop"}]}),
        json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-mock",
               "choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}}),
    ];
    let mut out = String::new();
    for c in chunks {
        out.push_str("data: ");
        out.push_str(&c.to_string());
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        out,
    )
        .into_response()
}

struct TestEnv {
    pg: PgPool,
    state: gateway::state::AppState,
    addr: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("pg-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("pg-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-pg-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &hash, "sk-okapi-pg")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock_app = axum::Router::new().route("/v1/chat/completions", post(mock_stream));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_app).await.unwrap();
    });
    okapi_store::provision::create_channel(
        &pg,
        &format!("pg-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    published_pricing::publish(&pg, user_id).await;
    let mut state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    // 选用令牌需要主密钥解开加密副本；固定值只在本测试进程里用
    state.master_key = Some(Arc::from(MASTER));
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
    // 站点预设经进程缓存注入（不写共享库，不干扰并行用例）
    state
        .settings_cache
        .insert(
            "playground_presets".to_owned(),
            Arc::new(Some(json!([
                {"name": " Reviewer ", "model": "gpt-4o", "system": "be terse",
                 "temperature": 9, "max_tokens": 0, "top_p": -1, "secret": "x"},
                {"name": "", "model": "gpt-4o"},
                {"model": "no-name"},
                {"name": "Plain", "model": "claude-3", "max_tokens": 512}
            ]))),
        )
        .await;

    let app = console::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        state,
        addr,
        token,
        user_id,
        model,
    }
}

async fn wait_committed(pg: &PgPool, user_id: i64) -> (i16, i64, i32) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens
               FROM billing_records WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.status, r.amount_micro, r.prompt_tokens);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

#[tokio::test]
async fn relay_persists_upstream_error_details_and_redacts_credentials() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "top_p": 1,
            "messages": [{"role": "user", "content": "private prompt"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let request_id: Uuid = resp.headers()["x-okapi-request-id"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let response: Value = resp.json().await.unwrap();
    assert_eq!(response["error"]["type"], "invalid_request_error");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("only 0.95")
    );

    let record: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('status', status, 'amount', amount_micro, 'upstream_status', upstream_status, 'error_code', error_code, 'diagnostics', usage_details->'diagnostics') FROM billing_records WHERE request_id=$1",
    )
    .bind(request_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(record["status"], 40);
    assert_eq!(record["amount"], 0);
    assert_eq!(record["upstream_status"], 400);
    assert_eq!(record["error_code"], "upstream_status_400");
    let diagnostics = &record["diagnostics"];
    let message = "invalid top_p: only 0.95 is allowed for this model; credential [redacted]";
    assert_eq!(diagnostics["error_message"], message);
    assert_eq!(diagnostics["error_phase"], "upstream");
    let attempts = diagnostics["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["status"], 400);
    assert_eq!(attempts[0]["outcome"], "failure");
    assert_eq!(attempts[0]["error_message"], message);
    assert!(attempts[0]["duration_ms"].is_u64());
    assert!(!diagnostics.to_string().contains("mock-credential"));
    assert!(!diagnostics.to_string().contains("private prompt"));

    let outbox: Value =
        sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1")
            .bind(request_id.to_string())
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert_eq!(outbox["diagnostics"], *diagnostics);

    let logs: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/api/me/logs?request_id={request_id}",
            env.addr
        ))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(logs["data"][0]["diagnostics"]["error_message"], message);
    assert!(logs["data"][0]["diagnostics"]["attempts"].is_null());
}

/// 中继：body 里 stream:false 也回 SSE，逐块透出内容与 [DONE]，记账落在同一把 key。
#[tokio::test]
async fn relay_forces_stream_and_bills() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "stream": false,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
        "非流式请求应被强制为流式"
    );
    let text = resp.text().await.unwrap();
    let content: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|c| {
            c["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(content, "Hello playground");
    assert!(text.contains("data: [DONE]"));

    // 倍率全 1 → 基价 $0.002/1K = 2 micro/token：(100 prompt + 20 completion) × 2 = 240 micro
    let (status, amount, prompt) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(status, 20, "committed");
    assert_eq!(prompt, 100);
    assert_eq!(amount, 240);
}

/// 无凭证：与真实数据面一致 401（中继不绕过鉴权）。
#[tokio::test]
async fn relay_requires_key() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

/// 请求体超 1MB：413（在改写流式之前就拦）。
#[tokio::test]
async fn relay_rejects_oversized_body() {
    let env = setup().await;
    let big = "x".repeat(1024 * 1024 + 16);
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": big}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["param"], "body_too_large");
}

/// 站点预设端点：公开只读、白名单收口（缺 name/model 丢弃、越界夹取、多余字段不出）。
#[tokio::test]
async fn presets_endpoint_whitelists() {
    let env = setup().await;
    // 公开端点：不带任何凭证也能读
    let resp = reqwest::Client::new()
        .get(format!("http://{}/api/playground/presets", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "缺 name / model 的两条被丢弃：{body}");
    assert_eq!(data[0]["name"], "Reviewer", "首尾空白去掉");
    assert_eq!(data[0]["temperature"], 2.0, "温度夹到 2");
    assert!(data[0]["max_tokens"].is_null(), "0 视为未设置");
    assert_eq!(data[0]["top_p"], 0.0, "top_p 夹到 0");
    assert!(data[0].get("secret").is_none(), "多余字段不出");
    assert_eq!(data[1]["name"], "Plain");
    assert_eq!(data[1]["max_tokens"], 512);
    // state 已被 setup 用掉一次；这里只是防止未使用告警
    let _ = &env.state;
}

fn sha256_hex(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn parameter_rules(env: &TestEnv, selected: Option<i64>) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .get(format!(
            "http://{}/api/me/playground/parameters?model={}",
            env.addr, env.model
        ))
        .bearer_auth(&env.token);
    if let Some(id) = selected {
        req = req.header("X-Okapi-Playground-Key", id);
    }
    req.send().await.unwrap()
}

#[tokio::test]
async fn parameter_rules_use_mapped_upstream_and_reject_before_billing() {
    let env = setup().await;
    sqlx::query("UPDATE channels SET model_mapping=jsonb_build_object($1::text,'kimi-k3') WHERE models @> jsonb_build_array($1::text)")
        .bind(&env.model).execute(&env.pg).await.unwrap();
    let rules: Value = parameter_rules(&env, None).await.json().await.unwrap();
    assert_eq!(rules["efforts"], json!(["low", "high", "max"]));
    assert_eq!(rules["temperature_max"], Value::Null);
    assert_eq!(rules["top_p"], false);
    assert_eq!(rules["preserve_reasoning"], true);
    assert!(!rules.to_string().contains("mock-credential"));
    for (field, value) in [
        ("top_p", json!(1)),
        ("temperature", json!(1)),
        ("reasoning_effort", json!("medium")),
    ] {
        let mut body = json!({"model":env.model, "messages":[{"role":"user","content":"hi"}]});
        body[field] = value;
        let response = reqwest::Client::new()
            .post(format!("http://{}/api/me/playground/chat", env.addr))
            .bearer_auth(&env.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["param"],
            field
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1")
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "invalid parameters and metadata must not be billed"
    );
    let response = reqwest::Client::new().post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token).json(&json!({"model":env.model,"reasoning_effort":"max","max_tokens":256,"messages":[{"role":"user","content":"hi"}]}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("Hello"));
}

#[tokio::test]
async fn parameter_rules_enforce_selected_key_ownership_and_allowlist() {
    let env = setup().await;
    let (blocked, _) = extra_key(&env.pg, env.user_id, true, Some(json!(["another-model"]))).await;
    assert_eq!(parameter_rules(&env, Some(blocked)).await.status(), 403);
    let other_user = okapi_store::provision::create_user(
        &env.pg,
        &format!("parameter-other-{}", Uuid::new_v4()),
    )
    .await
    .unwrap();
    let (foreign, _) = extra_key(&env.pg, other_user, true, None).await;
    assert_eq!(parameter_rules(&env, Some(foreign)).await.status(), 404);
    let (own, _) = extra_key(&env.pg, env.user_id, true, None).await;
    assert_eq!(parameter_rules(&env, Some(own)).await.status(), 200);
}

#[tokio::test]
async fn parameter_rules_intersect_failover_models_and_channel_controls() {
    let env = setup().await;
    sqlx::query("UPDATE channels SET model_mapping=jsonb_build_object($1::text,'kimi-k3') WHERE models @> jsonb_build_array($1::text)")
        .bind(&env.model).execute(&env.pg).await.unwrap();
    let (channel, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("parameter-fallback-{}", Uuid::new_v4()),
        "openai",
        "http://127.0.0.1:1/v1",
        "mock-fallback",
        &[env.model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE channels SET model_mapping=jsonb_build_object($2::text,'gpt-6-sol') WHERE id=$1",
    )
    .bind(channel)
    .bind(&env.model)
    .execute(&env.pg)
    .await
    .unwrap();
    let common: Value = parameter_rules(&env, None).await.json().await.unwrap();
    assert_eq!(common["efforts"], json!(["low", "high", "max"]));
    assert_eq!(common["top_p"], false);
    assert!(common["default_effort"].is_null());
    sqlx::query("UPDATE channels SET settings=jsonb_build_object('strip_request_fields',jsonb_build_array('reasoning_effort')) WHERE id=$1")
        .bind(channel).execute(&env.pg).await.unwrap();
    let controlled: Value = parameter_rules(&env, None).await.json().await.unwrap();
    assert_eq!(controlled["efforts"], json!([]));
}

/// 给 `user_id` 再建一把令牌；`saved` 决定是否写入可恢复的加密副本（旧令牌没有），
/// `allowlist` 写入模型白名单。返回 (id, 明文)。
async fn extra_key(
    pg: &PgPool,
    user_id: i64,
    saved: bool,
    allowlist: Option<Value>,
) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let token = format!("sk-okapi-sel-{suffix}");
    let hash = sha256_hex(&token);
    let id = okapi_store::provision::create_api_key(pg, user_id, &hash, "sk-okapi-sel")
        .await
        .unwrap();
    if saved {
        let sealed = okapi_store::api_key_secret::seal(MASTER, user_id, &hash, &token).unwrap();
        sqlx::query("UPDATE api_keys SET key_ciphertext = $2 WHERE id = $1")
            .bind(id)
            .bind(sealed)
            .execute(pg)
            .await
            .unwrap();
    }
    if let Some(list) = allowlist {
        sqlx::query("UPDATE api_keys SET model_allowlist = $2 WHERE id = $1")
            .bind(id)
            .bind(list)
            .execute(pg)
            .await
            .unwrap();
    }
    (id, token)
}

fn relay(env: &TestEnv, selected: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token)
        .header("x-okapi-playground-key", selected)
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": "hi"}]}))
}

async fn error_code(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    (
        status,
        body["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

/// 选用令牌：请求按被选令牌计费（api_key_id 落在它身上，而不是登录 key），内容照常流式透出。
#[tokio::test]
async fn relay_bills_the_selected_key() {
    let env = setup().await;
    let (id, _) = extra_key(&env.pg, env.user_id, true, None).await;
    let resp = relay(&env, &id.to_string()).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("Hello"), "{text}");
    wait_committed(&env.pg, env.user_id).await;
    let billed: i64 = sqlx::query_scalar(
        "SELECT api_key_id FROM billing_records WHERE user_id = $1 AND log_type = 2",
    )
    .bind(env.user_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(billed, id, "计费必须落在选用的令牌上");
}

/// 被选令牌自己的模型白名单照常生效：不含该模型就被数据面拒绝（与 SDK 直调一致）。
#[tokio::test]
async fn relay_applies_the_selected_keys_allowlist() {
    let env = setup().await;
    let (id, _) = extra_key(
        &env.pg,
        env.user_id,
        true,
        Some(json!(["some-other-model"])),
    )
    .await;
    let (status, code) = error_code(relay(&env, &id.to_string()).send().await.unwrap()).await;
    assert_eq!((status, code.as_str()), (403, "model_not_allowed"));
    // 不选令牌（登录 key 无白名单）仍可调
    let ok = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
}

/// 别人的令牌 id：一律 404（不泄露存在性），也不会用对方的明文发请求。
#[tokio::test]
async fn relay_rejects_another_users_key() {
    let env = setup().await;
    let other =
        okapi_store::provision::create_user(&env.pg, &format!("other-{}", Uuid::new_v4().simple()))
            .await
            .unwrap();
    let (foreign, _) = extra_key(&env.pg, other, true, None).await;
    let (status, code) = error_code(relay(&env, &foreign.to_string()).send().await.unwrap()).await;
    assert_eq!((status, code.as_str()), (404, "not_found"));
    let (status, code) = error_code(relay(&env, "999999999").send().await.unwrap()).await;
    assert_eq!(
        (status, code.as_str()),
        (404, "not_found"),
        "不存在的 id 同样 404"
    );
}

/// 没存加密副本的旧令牌解不出明文：409 key_copy_not_saved（与"复制完整 Token"同口径）。
#[tokio::test]
async fn relay_selected_key_without_ciphertext_is_conflict() {
    let env = setup().await;
    let (id, _) = extra_key(&env.pg, env.user_id, false, None).await;
    let (status, code) = error_code(relay(&env, &id.to_string()).send().await.unwrap()).await;
    assert_eq!((status, code.as_str()), (409, "key_copy_not_saved"));
}

/// 缺主密钥时 503（不回退成用登录 key，免得用户以为在测被选令牌）。
#[tokio::test]
async fn relay_selected_key_needs_master_key() {
    let env = setup().await;
    let (id, _) = extra_key(&env.pg, env.user_id, true, None).await;
    let mut state = env.state.clone();
    state.master_key = None;
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/me/playground/chat"))
        .bearer_auth(&env.token)
        .header("x-okapi-playground-key", id.to_string())
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        error_code(resp).await,
        (503, "key_copy_unavailable".to_owned())
    );
}

/// 令牌 id 头不合法（非数字 / 非正数）：400，不去查库。登录 key 缺失时仍是 401。
#[tokio::test]
async fn relay_selected_key_header_is_validated() {
    let env = setup().await;
    for bad in ["abc", "0", "-3", ""] {
        let resp = relay(&env, bad).send().await.unwrap();
        assert_eq!(resp.status(), 400, "头值 {bad:?}");
    }
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .header("x-okapi-playground-key", "1")
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "没有登录 key 不能借头选令牌");
}
