//! 批次 B 验收：一键原子轮换、自助收紧速率/并发与周期预算落闸、用户级告警偏好校验。
#[path = "../../../okapi-gateway/tests/support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi_console as console;
use okapi_domain::Money;
use okapi_gateway as gateway;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    axum::Json(json!({
        "id": "cmpl",
        "object": "chat.completion",
        "model": req["model"],
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}],
        "usage": {"prompt_tokens": 1000, "completion_tokens": 500}
    }))
    .into_response()
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    cookie: String,
    pg: PgPool,
    user_id: i64,
    model: String,
    gateway: SocketAddr,
    console: SocketAddr,
    super_token: String,
}

async fn setup() -> Bed {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let mock = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/chat/completions", post(mock_ok)),
            )
            .await
            .unwrap();
        });
        addr
    };
    let model = format!("gov-m-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &pg,
        &format!("gov-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("gov-u-{suffix}"))
        .await
        .unwrap();
    let super_id = okapi_store::provision::create_user(&pg, &format!("gov-s-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-gov-super-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-gov-s")
        .await
        .unwrap();

    published_pricing::publish(&pg, user_id).await;
    let state = gateway::build_state(&database_url, &redis_url, "gov-node", None, None)
        .await
        .unwrap();
    let sid = okapi_kernel::sched_redis::SchedulerRedis::web_session_sid(
        user_id,
        &Uuid::new_v4().simple().to_string(),
    );
    state.sched.web_session_set(&sid, user_id, None, None).await;
    state
        .ledger
        .credit(user_id, Money::from_micros(50_000_000))
        .await
        .unwrap();

    let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = gw_listener.local_addr().unwrap();
    let gw_app = gateway::router(state.clone());
    tokio::spawn(async move {
        axum::serve(gw_listener, gw_app).await.unwrap();
    });
    let cs_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = cs_listener.local_addr().unwrap();
    let cs_app = console::router(state);
    tokio::spawn(async move {
        axum::serve(cs_listener, cs_app).await.unwrap();
    });

    Bed {
        cookie: format!("okapi_session={sid}"),
        pg,
        user_id,
        model,
        gateway: gateway_addr,
        console: console_addr,
        super_token,
    }
}

async fn chat(bed: &Bed, token: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(token)
        .json(&json!({
            "model": bed.model,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

/// POST /api/me/keys/{id}/rotate：保留全部原配置并原子换密，旧明文立废（无 60s 缓存滞后）。
#[tokio::test]
async fn rotate_key_preserves_settings_and_revokes_old_token_immediately() {
    let bed = setup().await;
    let client = reqwest::Client::new();
    let created: Value = client
        .post(format!("http://{}/auth/keys", bed.console))
        .header("cookie", &bed.cookie)
        .json(&json!({
            "name": "app-sub",
            "rpm_limit": 30,
            "daily_quota_micro": 5_000_000,
            "model_allowlist": [bed.model],
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let key_id = created["key_id"].as_i64().unwrap();
    let old_token = created["api_key"].as_str().unwrap().to_owned();
    assert_eq!(chat(&bed, &old_token).await.0, 200);

    // 仅持 Bearer key 不带 Web 会话不得轮换
    let denied = client
        .post(format!(
            "http://{}/api/me/keys/{key_id}/rotate",
            bed.console
        ))
        .bearer_auth(&old_token)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);

    // 持主账户 Web 会话原子轮换
    let rotated = client
        .post(format!(
            "http://{}/api/me/keys/{key_id}/rotate",
            bed.console
        ))
        .header("cookie", &bed.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(rotated.status(), 200);
    assert_eq!(rotated.headers()["cache-control"], "no-store, private");
    let body: Value = rotated.json().await.unwrap();
    let new_token = body["api_key"].as_str().unwrap().to_owned();
    assert_ne!(new_token, old_token);
    assert_eq!(body["key_id"], key_id);

    // 旧明文立废，新明文立即可用且保留原有全部配置
    assert_eq!(chat(&bed, &old_token).await.0, 401);
    assert_eq!(chat(&bed, &new_token).await.0, 200);
    let me: Value = client
        .get(format!("http://{}/api/me", bed.console))
        .bearer_auth(&new_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["key_limits"]["rpm_limit"], 30);
    assert_eq!(me["key_limits"]["daily_quota_micro"], 5_000_000);
    assert_eq!(me["key_limits"]["model_allowlist"], json!([bed.model]));
}

/// 自助面允许创建/收紧速率上限，禁止放宽或清空既有速率上限；日周期消费上限结算后立刻生效。
#[tokio::test]
async fn self_service_rate_tightening_and_periodic_key_budget_enforcement() {
    let bed = setup().await;
    let client = reqwest::Client::new();
    let created: Value = client
        .post(format!("http://{}/auth/keys", bed.console))
        .header("cookie", &bed.cookie)
        .json(&json!({
            "name": "budgeted",
            "rpm_limit": 50,
            "daily_quota_micro": 1,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let key_id = created["key_id"].as_i64().unwrap();
    let token = created["api_key"].as_str().unwrap().to_owned();
    let portal_url = format!("http://{}/api/me/keys/{key_id}", bed.console);

    // 收紧 rpm_limit: 50 -> 20 允许；提高到 80 或置 null 必须 403 key_limit_tighten_only
    assert_eq!(
        client
            .patch(&portal_url)
            .header("cookie", &bed.cookie)
            .json(&json!({ "rpm_limit": 20 }))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for payload in [json!({ "rpm_limit": 80 }), json!({ "rpm_limit": null })] {
        let resp = client
            .patch(&portal_url)
            .header("cookie", &bed.cookie)
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
        let err: Value = resp.json().await.unwrap();
        assert_eq!(err["error"]["code"], "key_limit_tighten_only");
        assert_eq!(err["error"]["param"], "rpm_limit");
    }
    // 管理员仍可放宽或解除
    assert_eq!(
        client
            .patch(format!("http://{}/admin/keys/{key_id}", bed.console))
            .bearer_auth(&bed.super_token)
            .json(&json!({ "rpm_limit": null }))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // 日消费上限 1 micro：首笔成功落账后，第二笔立刻触发 429 key_quota_exceeded / daily_quota
    assert_eq!(chat(&bed, &token).await.0, 200);
    let (status, denied) = chat(&bed, &token).await;
    assert_eq!(status, 429, "{denied}");
    assert_eq!(denied["error"]["code"], "key_quota_exceeded", "{denied}");
    assert_eq!(denied["error"]["param"], "daily_quota", "{denied}");
}

async fn put_alert(bed: &Bed, payload: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .put(format!("http://{}/api/me/alerts", bed.console))
        .header("cookie", &bed.cookie)
        .json(&payload)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

/// GET/PUT /api/me/alerts：Web 会话门禁、SSRF 拦截、邮箱前置校验、secret 不回显且缺席保留。
#[tokio::test]
async fn user_alerts_preferences_validate_ssrf_email_and_redact_secret() {
    let bed = setup().await;
    let url = format!("http://{}/api/me/alerts", bed.console);

    let initial: Value = reqwest::Client::new()
        .get(&url)
        .header("cookie", &bed.cookie)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial["email_enabled"], false);
    assert_eq!(initial["webhook_secret_configured"], false);
    assert_eq!(initial["min_interval_secs"], 3600);

    // SSRF 拒绝私网/明文 http Webhook
    assert_eq!(
        put_alert(&bed, json!({ "webhook_url": "http://127.0.0.1:8080/hook" }))
            .await
            .0,
        400
    );
    // 未绑定邮箱时开启 email_enabled 被拒
    let (status, no_email) = put_alert(&bed, json!({ "email_enabled": true })).await;
    assert_eq!(status, 400);
    assert_eq!(no_email["error"]["param"], "email_required");

    // 绑定邮箱并保存合法告警偏好（含 secret）
    sqlx::query("UPDATE users SET email = 'u@example.com' WHERE id = $1")
        .bind(bed.user_id)
        .execute(&bed.pg)
        .await
        .unwrap();
    let (status, saved) = put_alert(
        &bed,
        json!({
            "low_balance_threshold_micro": 5_000_000,
            "key_expiry_warn_days": 7,
            "email_enabled": true,
            "webhook_url": "https://1.1.1.1/okapi-hook",
            "webhook_secret": "s3cr3t-value",
            "min_interval_secs": 1800,
        }),
    )
    .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["low_balance_threshold_micro"], 5_000_000);
    assert_eq!(saved["key_expiry_warn_days"], 7);
    assert_eq!(saved["email_enabled"], true);
    assert_eq!(saved["webhook_secret_configured"], true);
    assert!(saved.get("webhook_secret").is_none());

    // 缺席 webhook_secret 更新其它字段时保留原 secret；显式 null 清除
    let (status, kept) = put_alert(
        &bed,
        json!({
            "low_balance_threshold_micro": 2_000_000,
            "key_expiry_warn_days": 3,
            "email_enabled": true,
            "webhook_url": "https://1.1.1.1/okapi-hook",
        }),
    )
    .await;
    assert_eq!(status, 200, "{kept}");
    assert_eq!(kept["webhook_secret_configured"], true);

    let (status, cleared) = put_alert(
        &bed,
        json!({
            "email_enabled": false,
            "webhook_secret": null,
        }),
    )
    .await;
    assert_eq!(status, 200, "{cleared}");
    assert_eq!(cleared["webhook_secret_configured"], false);
}
