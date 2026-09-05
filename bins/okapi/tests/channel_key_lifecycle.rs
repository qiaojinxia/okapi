//! 渠道 key 生命周期验收（IMPLEMENTATION §11.21）：
//!
//! 1. 上游 403 不再一律判「凭证失效」——聚合型上游用 403 表达「这个模型你的套餐没开通」，
//!    此前一次调用未开通的模型就把该渠道**所有**模型打死（status=6，无冷却不自愈）；
//!    现在只有 body 明说凭证问题才判失效，否则按瞬时失败走冷却重试。
//! 2. 被打成 status=6 的 key 有了显式复活入口（PATCH status=1，连带清零失败计数/冷却）——
//!    此前控制面只认 weight/max_concurrency，传 status 被静默忽略还返回 ok:true。
//! 3. 测活可以真验模型：只探 /models 会对一个实际 403 的模型报 ok:true。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

/// 403 + 「模型没开通」——凭证本身有效（实测自真实聚合上游的原文形状）。
async fn mock_model_denied() -> axum::response::Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        axum::Json(json!({"error": {"code": "access_denied",
            "message": "Access restricted. Deposit required to unlock premium models."}})),
    )
        .into_response()
}

/// 403 + 「这把 key 不认」——真凭证失效。
async fn mock_key_rejected() -> axum::response::Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        axum::Json(json!({"error": {"code": "invalid_api_key",
            "message": "Incorrect API key provided."}})),
    )
        .into_response()
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    token: String,
    admin_token: String,
    model: String,
    gateway: SocketAddr,
    console: SocketAddr,
    /// (channel_id, channel_key_id) —— 上游回「模型没开通」
    denied: (i64, i64),
    /// (channel_id, channel_key_id) —— 上游回「凭证不认」
    rejected: (i64, i64),
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let denied_up = serve(
        Router::new().route("/v1/chat/completions", post(mock_model_denied)),
    )
    .await;
    let rejected_up = serve(
        Router::new().route("/v1/chat/completions", post(mock_key_rejected)),
    )
    .await;

    // 两个模型各自绑一条渠道：避免同一模型的候选互相 failover 干扰状态断言
    let model = format!("klc-m-{suffix}");
    let model2 = format!("klc-m2-{suffix}");
    for m in [&model, &model2] {
        okapi_store::provision::create_model_ratio(&pg, m, "1.0", "1.0", "1.0")
            .await
            .unwrap();
    }
    let mk = |name: String, base: String, models: Vec<String>| {
        let pg = pg.clone();
        async move {
            let refs: Vec<&str> = models.iter().map(String::as_str).collect();
            okapi_store::provision::create_channel(
                &pg, &name, "openai", &base, "cred", &refs, true, None,
            )
            .await
            .unwrap()
        }
    };
    let denied = mk(
        format!("klc-denied-{suffix}"),
        format!("http://{denied_up}/v1"),
        vec![model.clone()],
    )
    .await;
    let rejected = mk(
        format!("klc-rejected-{suffix}"),
        format!("http://{rejected_up}/v1"),
        vec![model2.clone()],
    )
    .await;

    let user_id = okapi_store::provision::create_user(&pg, &format!("klc-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-klc-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-klc")
        .await
        .unwrap();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("klc-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-klc-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-klc-adm")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(50_000_000))
        .await
        .unwrap();
    let gateway_addr = serve(gateway::router(state.clone())).await;
    let console_addr = serve(console::router(state)).await;

    Bed {
        pg,
        token,
        admin_token,
        model,
        gateway: gateway_addr,
        console: console_addr,
        denied,
        rejected: (rejected.0, rejected.1),
    }
}

async fn chat(bed: &Bed, model: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&json!({"model": model, "stream": false,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn key_state(pg: &PgPool, key_id: i64) -> (i16, i32) {
    let r = sqlx::query!(
        r#"SELECT status, failed_count FROM channel_keys WHERE id = $1"#,
        key_id
    )
    .fetch_one(pg)
    .await
    .unwrap();
    (r.status, r.failed_count)
}

/// 403 的两种含义分开处置：模型没开通 ≠ 凭证失效。
#[tokio::test]
async fn model_denied_403_does_not_kill_the_key() {
    let bed = setup().await;

    // ① 「模型没开通」：key 不该被打成 6（此前就是这么死的），仅计一次瞬时失败
    assert_eq!(chat(&bed, &bed.model).await, 502);
    let (status, failed) = key_state(&bed.pg, bed.denied.1).await;
    assert_ne!(status, 6, "模型级 403 不得判凭证失效（此前 status=6 且不自愈）");
    assert_eq!(status, 1, "首次瞬时失败保持可用，连续 3 次才转冷却");
    assert_eq!(failed, 1);

    // ② 「凭证不认」：仍然要立刻判失效
    let model2 = sqlx::query_scalar!(
        r#"SELECT m.model_name FROM models m
           JOIN channels c ON c.models ? m.model_name
           WHERE c.id = $1 LIMIT 1"#,
        bed.rejected.0
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(chat(&bed, &model2).await, 502);
    let (status, _) = key_state(&bed.pg, bed.rejected.1).await;
    assert_eq!(status, 6, "body 明说凭证问题 → 仍判失效");
}

/// 复活入口：PATCH status=1 把失效 key 拉回可用，并清零失败计数/冷却/last_error。
#[tokio::test]
async fn disabled_key_can_be_re_enabled() {
    let bed = setup().await;
    let (cid, kid) = bed.rejected;
    // 先打死它
    sqlx::query!(
        r#"UPDATE channel_keys SET status = 6, failed_count = 7, last_error = 'boom',
                                   cooldown_until = now() + interval '1 hour' WHERE id = $1"#,
        kid
    )
    .execute(&bed.pg)
    .await
    .unwrap();

    let client = reqwest::Client::new();
    // 非法状态位挡下：状态机自己的位（冷却/限流/配额/失效）不许手工写入
    let bad = client
        .patch(format!("http://{}/admin/channels/{cid}/keys/{kid}", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"status": 6}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    let ok = client
        .patch(format!("http://{}/admin/channels/{cid}/keys/{kid}", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"status": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let r = sqlx::query!(
        r#"SELECT status, failed_count, last_error, cooldown_until FROM channel_keys WHERE id = $1"#,
        kid
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(r.status, 1);
    assert_eq!(r.failed_count, 0, "复活即重新开始计数");
    assert!(r.last_error.is_none());
    assert!(r.cooldown_until.is_none());
}

/// 测活分两种范围：只验凭证 vs 真验模型。后者才能发现「凭证有效但模型 403」。
#[tokio::test]
async fn channel_test_can_probe_a_specific_model() {
    let bed = setup().await;
    let (cid, _) = bed.denied;
    let client = reqwest::Client::new();

    // 不带 model：上游没有 /models 路由 → 404，但 scope 标明只验了凭证面
    let cred: Value = client
        .post(format!("http://{}/admin/channels/{cid}/test", bed.console))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cred["scope"], "credential");
    assert!(cred["model"].is_null());

    // 带 model：真发一次最小补全 → 拿到上游的 403 与原文，运维一眼看出是模型没开通
    let probe: Value = client
        .post(format!("http://{}/admin/channels/{cid}/test", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"model": bed.model}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(probe["scope"], "model");
    assert_eq!(probe["model"], bed.model);
    assert_eq!(probe["ok"], false);
    assert_eq!(probe["http_status"], 403);
    assert!(
        probe["upstream_body"]
            .as_str()
            .unwrap_or_default()
            .contains("access_denied"),
        "失败要带上游原文：{probe}"
    );
}
