//! Isolated mock validation for shared channel controls; never read native CLI credentials.
#[path = "../../../../crates/okapi-gateway/tests/support/published_pricing.rs"]
mod published_pricing;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use okapi::{
    gateway,
    gateway::{account_control, state::AppState},
};
use okapi_store::{channels::KeyFailure, credential::OAuthCredential};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Mock {
    inference: Arc<AtomicUsize>,
    quota: Arc<AtomicUsize>,
    refresh: Arc<AtomicUsize>,
    profile: Arc<AtomicUsize>,
    /// 下一次刷新回 invalid_grant（refresh token 已被作废）。
    refuse_refresh: Arc<AtomicBool>,
}
async fn inference(State(mock): State<Mock>) -> Json<Value> {
    mock.inference.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"id":"mock","object":"chat.completion","model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":8,"completion_tokens":3,"total_tokens":11}}),
    )
}
async fn quota(State(mock): State<Mock>, headers: HeaderMap) -> Response {
    mock.quota.fetch_add(1, Ordering::SeqCst);
    let authorization = headers.get("authorization").unwrap().to_str().unwrap();
    if authorization.contains("revoked") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(
                json!({"type":"error","error":{"type":"authentication_error","message":"OAuth token has been revoked"}}),
            ),
        )
            .into_response();
    }
    let used = if authorization.contains("high") {
        95
    } else {
        10
    };
    Json(
        json!({"five_hour":{"utilization":used,"resets_at":"2099-01-01T00:00:00Z"},"seven_day":{"utilization":10,"resets_at":"2099-01-01T00:00:00Z"},"limits":[{"kind":"weekly_scoped","scope":{"model":{"display_name":"Fable"}},"percent":37,"resets_at":4_070_908_800_i64}],"plan_type":"prolite","rate_limit":{"primary_window":{"used_percent":used,"limit_window_seconds":604_800,"reset_at":4_070_908_800_i64},"secondary_window":{"used_percent":10,"limit_window_seconds":18000,"reset_at":4_070_908_800_i64}}}),
    )
    .into_response()
}
async fn profile(State(mock): State<Mock>, headers: HeaderMap) -> Json<Value> {
    mock.profile.fetch_add(1, Ordering::SeqCst);
    assert!(
        headers.get("anthropic-beta").is_none(),
        "CLI 取 profile 不带 beta"
    );
    assert_eq!(headers["cache-control"], "no-cache");
    Json(
        json!({"account":{"uuid":"a","email":"x@example.com","has_claude_max":true},"organization":{"uuid":"o","organization_type":"claude_max","rate_limit_tier":"default_claude_max_20x"}}),
    )
}
async fn count_tokens(State(mock): State<Mock>) -> Json<Value> {
    mock.inference.fetch_add(1, Ordering::SeqCst);
    Json(json!({"object":"response.input_tokens","input_tokens":4242}))
}
async fn refresh(State(mock): State<Mock>) -> Response {
    mock.refresh.fetch_add(1, Ordering::SeqCst);
    if mock.refuse_refresh.load(Ordering::SeqCst) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant","error_description":"revoked"})),
        )
            .into_response();
    }
    Json(
        json!({"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600}),
    )
    .into_response()
}

struct Env {
    state: AppState,
    channel: i64,
    key: i64,
    user: i64,
    model: String,
    base: String,
    mock: Mock,
    client_token: String,
}
async fn setup(provider: &str, settings: Value) -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let user = okapi_store::provision::create_user(&pg, &format!("controls-{suffix}"))
        .await
        .unwrap();
    let model = format!("controls-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let client_token = format!("sk-okapi-controls-{suffix}");
    let hash = hex::encode(Sha256::digest(client_token.as_bytes()));
    okapi_store::provision::create_api_key(&pg, user, &hash, "sk-okapi-con")
        .await
        .unwrap();
    let mock = Mock::default();
    let routes = Router::new()
        .route("/v1/chat/completions", post(inference))
        .route("/messages/count_tokens", post(count_tokens))
        .route("/v1/responses/input_tokens", post(count_tokens))
        .route("/api/oauth/usage", get(quota))
        .route("/api/oauth/profile", get(profile))
        .route("/backend-api/wham/usage", get(quota))
        .route("/api/codex/usage", get(quota))
        .route("/token", post(refresh))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let credential = OAuthCredential {
        access_token: "high-quota-access".into(),
        refresh_token: "mock-refresh".into(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        account_id: Some(suffix.clone()),
        account_label: None,
        scope: None,
    };
    let plain = if provider == "openai" {
        "mock-key".into()
    } else {
        credential.to_plaintext()
    };
    let (channel, key) = okapi_store::provision::create_channel(
        &pg,
        &model,
        provider,
        &if provider == "openai" {
            format!("{base}/v1")
        } else {
            base.clone()
        },
        &plain,
        &[&model],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1")
        .bind(channel)
        .bind(settings)
        .execute(&pg)
        .await
        .unwrap();
    if provider != "openai" {
        sqlx::query("UPDATE channel_keys SET credential_kind=1 WHERE id=$1")
            .bind(key)
            .execute(&pg)
            .await
            .unwrap();
    }
    published_pricing::publish(&pg, user).await;
    let state = gateway::build_state(&database, &redis, "controls-test", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user, okapi_domain::Money::from_micros(10_000_000))
        .await
        .unwrap();
    Env {
        state,
        channel,
        key,
        user,
        model,
        base,
        mock,
        client_token,
    }
}

async fn bill(env: &Env, tokens: i32, cost: Option<i64>, customer_charge: i64) {
    sqlx::query("INSERT INTO billing_records(request_id,log_type,user_id,model_name,channel_id,channel_key_id,status,prompt_tokens,completion_tokens,upstream_cost_micro,amount_micro) VALUES($1,2,$2,$3,$4,$5,20,$6,0,$7,$8)")
        .bind(Uuid::new_v4()).bind(env.user).bind(&env.model).bind(env.channel).bind(env.key).bind(tokens).bind(cost).bind(customer_charge).execute(&env.state.pg).await.unwrap();
}

#[tokio::test]
async fn local_token_limit_counts_history_and_survives_archiving_and_transaction_rollback() {
    let env = setup(
        "anthropic_max",
        json!({"account_control":{"local_tokens":{"cap":20}}}),
    )
    .await;
    bill(&env, 10, None, 0).await;
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok()
    );
    let mut tx = env.state.pg.begin().await.unwrap();
    sqlx::query("INSERT INTO billing_records(request_id,log_type,user_id,model_name,channel_id,status,prompt_tokens,completion_tokens,cached_tokens,reasoning_tokens) VALUES($1,2,$2,$3,$4,20,4,6,3,2)")
        .bind(Uuid::new_v4()).bind(env.user).bind(&env.model).bind(env.channel).execute(&mut *tx).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        okapi_store::channel_usage::local_usage(&env.state.pg, env.channel, "total")
            .await
            .unwrap()
            .tokens,
        10
    );
    // Cache/reasoning are subsets of the input/output totals, not extra tokens.
    sqlx::query("INSERT INTO billing_records(request_id,log_type,user_id,model_name,channel_id,status,prompt_tokens,completion_tokens,cached_tokens,reasoning_tokens) VALUES($1,2,$2,$3,$4,20,4,6,3,2)")
        .bind(Uuid::new_v4()).bind(env.user).bind(&env.model).bind(env.channel).execute(&env.state.pg).await.unwrap();
    assert_eq!(
        okapi_store::channel_usage::local_usage(&env.state.pg, env.channel, "total")
            .await
            .unwrap()
            .tokens,
        20
    );
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_err()
    );
    let mut tx = okapi_store::history::read(&env.state.pg).await.unwrap();
    // Archive a bill, retaining its normalized usage exactly as real receipts do.
    sqlx::query("WITH removed AS (DELETE FROM billing_records WHERE channel_id=$1 RETURNING *) INSERT INTO billing_record_receipts(request_id,user_id,model_name,channel_id,status,amount_micro,original_amount_micro,discount_micro,is_stream,pool,usage_details,created_at) SELECT request_id,user_id,model_name,channel_id,status,amount_micro,original_amount_micro,discount_micro,is_stream,pool,jsonb_build_object('tokens',jsonb_build_object('prompt_tokens',prompt_tokens,'completion_tokens',completion_tokens)),created_at FROM removed")
        .bind(env.channel).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    for period in ["total", "day", "week"] {
        assert_eq!(
            okapi_store::channel_usage::local_usage(&env.state.pg, env.channel, period)
                .await
                .unwrap()
                .tokens,
            20
        );
    }
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_err()
    );
    let other = setup(
        "anthropic_max",
        json!({"account_control":{"local_tokens":{"cap":20}}}),
    )
    .await;
    assert!(
        account_control::admit(&other.state, other.channel, Some(other.key))
            .await
            .is_ok()
    );
}

/// 渠道累计 token 与累计成本（channel_token_totals）由 billing_records 的插入触发器维护：只累计成功（2）与
/// 流式中断（5）的记录——token 是输入 + 输出，成本是 upstream_cost_micro；其他类型不计，成本未知（NULL）或为负的不加。
#[tokio::test]
async fn channel_token_totals_follow_settled_records() {
    okapi_store::test_support::assert_isolated();
    let pool = okapi_store::connect_pg(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    okapi_store::run_migrations(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let channel = i64::from(rand_u32()) + 1_000_000;
    for (log_type, prompt, completion, cost) in [
        (2_i16, 8, 2, Some(700_i64)),
        (6, 99, 99, Some(9999)),
        (5, 3, 4, Some(300)),
        (2, 0, 0, None),
        (2, 0, 0, Some(-50)),
    ] {
        sqlx::query(
            "INSERT INTO billing_records \
               (request_id, user_id, model_name, status, log_type, channel_id, prompt_tokens, completion_tokens, upstream_cost_micro) \
             VALUES ($1, 1, 'token-total-model', 20, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(log_type)
        .bind(channel)
        .bind(prompt)
        .bind(completion)
        .bind(cost)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    let (tokens, cost): (i64, i64) = sqlx::query_as(
        "SELECT tokens::bigint, cost_micro::bigint FROM channel_token_totals WHERE channel_id = $1",
    )
    .bind(channel)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        tokens, 17,
        "8+2（成功）+ 3+4（流式中断）；失败记录的 99+99 不计"
    );
    assert_eq!(
        cost, 1000,
        "700（成功）+ 300（流式中断）；失败记录的 9999、未知成本与负数都不计"
    );
    tx.rollback().await.unwrap();
}

fn local_block_reason(error: &okapi_providers::UpstreamError) -> &str {
    match error {
        okapi_providers::UpstreamError::Build(text) => {
            text.strip_prefix("channel_control:").unwrap_or(text)
        }
        other => panic!("不是渠道控制的拒绝：{other:?}"),
    }
}

/// 成本上限对任何渠道都生效（这里是 API key 渠道），数的是这条渠道已落账的上游成本：
/// 向用户收了多少不算，成本未知的请求不算；到了上限新请求被挡，原因是 local_cost；别的渠道不受影响。
#[tokio::test]
async fn local_cost_limit_gates_an_api_key_channel_on_recorded_upstream_cost() {
    let settings = json!({"account_control":{"local_cost":{"cap_micro":1000}}});
    let env = setup("openai", settings.clone()).await;
    bill(&env, 5, Some(400), 9_000_000).await;
    bill(&env, 500, None, 9_000_000).await;
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok()
    );
    let usage = okapi_store::channel_usage::local_usage(&env.state.pg, env.channel, "total")
        .await
        .unwrap();
    assert_eq!((usage.cost_micro, usage.tokens), (400, 505));
    assert!(usage.window_start.is_none() && usage.window_end.is_none());
    bill(&env, 5, Some(599), 0).await;
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok(),
        "999 < 1000：差一微美元也还没到"
    );
    bill(&env, 5, Some(1), 0).await;
    let error = account_control::admit(&env.state, env.channel, Some(env.key))
        .await
        .unwrap_err();
    assert_eq!(local_block_reason(&error), "local_cost");
    let other = setup("openai", settings).await;
    assert!(
        account_control::admit(&other.state, other.channel, Some(other.key))
            .await
            .is_ok()
    );
}

/// 按周期的上限只数本周期（机器时区的自然日 / 周 / 月）：更早的账单不占本周期，只进累计总量；
/// 账单归档成回执后读数不变。成本和 Token 两项各按自己的周期判，先到的那项挡。
#[tokio::test]
async fn periodic_local_caps_count_only_the_current_window_and_survive_archiving() {
    for period in ["day", "week", "month"] {
        let env = setup(
            "openai",
            json!({"account_control":{
                "local_cost":{"cap_micro":1000,"period":period},
                "local_tokens":{"cap":1_000_000}
            }}),
        )
        .await;
        bill(&env, 7, Some(5000), 0).await;
        sqlx::query(
            "UPDATE billing_records SET created_at=now()-interval '400 days' WHERE channel_id=$1",
        )
        .bind(env.channel)
        .execute(&env.state.pg)
        .await
        .unwrap();
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_ok(),
            "{period}：400 天前花掉的不占本周期"
        );
        let window = okapi_store::channel_usage::local_usage(&env.state.pg, env.channel, period)
            .await
            .unwrap();
        assert_eq!((window.cost_micro, window.tokens), (0, 0), "{period}");
        let now = chrono::Utc::now();
        assert!(window.window_start.unwrap() <= now && window.window_end.unwrap() > now);
        let total = okapi_store::channel_usage::local_usage(&env.state.pg, env.channel, "total")
            .await
            .unwrap();
        assert_eq!((total.cost_micro, total.tokens), (5000, 7), "{period}");

        bill(&env, 3, Some(1000), 0).await;
        let error = account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .unwrap_err();
        assert_eq!(local_block_reason(&error), "local_cost", "{period}");
        let mut tx = okapi_store::history::read(&env.state.pg).await.unwrap();
        sqlx::query("WITH removed AS (DELETE FROM billing_records WHERE channel_id=$1 AND created_at>now()-interval '1 hour' RETURNING *) INSERT INTO billing_record_receipts(request_id,user_id,model_name,channel_id,status,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,pool,usage_details,created_at) SELECT request_id,user_id,model_name,channel_id,status,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,pool,jsonb_build_object('tokens',jsonb_build_object('prompt_tokens',prompt_tokens,'completion_tokens',completion_tokens)),created_at FROM removed")
            .bind(env.channel).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let window = okapi_store::channel_usage::local_usage(&env.state.pg, env.channel, period)
            .await
            .unwrap();
        assert_eq!((window.cost_micro, window.tokens), (1000, 3), "{period}");
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_err(),
            "{period}"
        );

        // 两项都到了报成本；成本没到、Token 到了才按 Token 挡（它的周期是累计总量，400 天前的 7 个也算）
        for (cost_cap, reason) in [(1000, "local_cost"), (1001, "local_tokens")] {
            sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1")
                .bind(env.channel)
                .bind(json!({"account_control":{
                    "local_cost":{"cap_micro":cost_cap,"period":period},
                    "local_tokens":{"cap":10}
                }}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            env.state.invalidate_routing_caches();
            let error = account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .unwrap_err();
            assert_eq!(local_block_reason(&error), reason, "{period}");
        }
    }
}

/// 设了上限的渠道，用量读不出来就不放行，原因写明是读不到用量（而不是当成没到上限）；
/// 没设上限的渠道本来就不读用量，不受影响。
#[tokio::test]
async fn a_capped_channel_is_not_used_while_its_usage_cannot_be_read() {
    let capped = setup(
        "openai",
        json!({"account_control":{"local_cost":{"cap_micro":1_000_000}}}),
    )
    .await;
    let plain = setup("openai", json!({})).await;
    let mut candidates = Vec::new();
    for env in [&capped, &plain] {
        let found = okapi_store::channels::candidates_for_model(
            &env.state.pg,
            &env.model,
            &["default"],
            env.state.master_key.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 1);
        candidates.push(found);
        // 放行一次，策略进了缓存；之后库不可用时只剩「读用量」这一步会失败
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_ok()
        );
        env.state.pg.close().await;
    }
    let error = account_control::admit(&capped.state, capped.channel, Some(capped.key))
        .await
        .unwrap_err();
    assert_eq!(local_block_reason(&error), "local_usage_unavailable");
    account_control::retain_available(
        &capped.state,
        &mut candidates[0],
        account_control::QuotaOrder::Keep,
        None,
    )
    .await;
    assert!(candidates[0].is_empty(), "读不到用量的候选要被筛掉");

    assert!(
        account_control::admit(&plain.state, plain.channel, Some(plain.key))
            .await
            .is_ok()
    );
    account_control::retain_available(
        &plain.state,
        &mut candidates[1],
        account_control::QuotaOrder::Keep,
        None,
    )
    .await;
    assert_eq!(candidates[1].len(), 1);
}

/// 真实链路：请求结算后上游成本落账，下一条请求就被这条渠道的日成本上限挡下——没有别的渠道时回 503，
/// Retry-After 指到本周期结束，请求诊断里写明是 local_cost 和何时重置；调高上限后立刻恢复。
#[tokio::test]
async fn a_settled_request_trips_the_daily_cost_cap_and_the_client_is_told_when_it_resets() {
    let env = setup(
        "openai",
        json!({"account_control":{"local_cost":{"cap_micro":1,"period":"day"}}}),
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = gateway::router(env.state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let chat = || {
        client
            .post(format!("http://{addr}/v1/chat/completions"))
            .bearer_auth(&env.client_token)
            .json(&json!({"model":env.model,"max_tokens":10,"messages":[{"role":"user","content":"Hello"}]}))
            .send()
    };
    let response = chat().await.unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap();
    let mut cost = None;
    for _ in 0..100 {
        cost = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT upstream_cost_micro FROM billing_records WHERE channel_id=$1 AND log_type=2",
        )
        .bind(env.channel)
        .fetch_optional(&env.state.pg)
        .await
        .unwrap();
        if cost.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        cost.flatten().is_some_and(|micro| micro >= 1),
        "结算要给这条请求记下上游成本：{cost:?}"
    );

    let response = chat().await.unwrap();
    assert_eq!(response.status(), 503);
    let retry_after: i64 = response.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    // 自然日最长 25 小时（夏令时回拨那天）
    assert!((0..=90_000).contains(&retry_after), "{retry_after}");
    let reply: Value = response.json().await.unwrap();
    assert_eq!(reply["error"]["code"], "no_available_channel", "{reply}");
    assert_eq!(env.mock.inference.load(Ordering::SeqCst), 1);
    let mut excluded = Value::Null;
    for _ in 0..100 {
        excluded = sqlx::query_scalar::<_, Option<Value>>(
            "SELECT usage_details->'diagnostics'->'excluded' FROM billing_records
             WHERE model_name=$1 AND log_type<>2 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&env.model)
        .fetch_optional(&env.state.pg)
        .await
        .unwrap()
        .flatten()
        .unwrap_or(Value::Null);
        if !excluded.is_null() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let now = chrono::Utc::now().timestamp();
    let resets_at = excluded[0]["resets_at"].as_i64().unwrap_or_default();
    assert_eq!(
        excluded,
        json!([{"channel_id": env.channel, "channel_key_id": env.key, "reason": "local_cost", "resets_at": resets_at}]),
    );
    assert!(resets_at > now && resets_at <= now + 90_000, "{excluded}");

    sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1")
        .bind(env.channel)
        .bind(json!({"account_control":{"local_cost":{"cap_micro":1_000_000_000,"period":"day"}}}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state.invalidate_routing_caches();
    let response = chat().await.unwrap();
    assert_eq!(response.status(), 200);
}

fn rand_u32() -> u32 {
    let bytes = Uuid::new_v4().into_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000_000
}

#[tokio::test]
async fn either_upstream_window_can_pause_a_subscription_with_no_legacy_threshold() {
    use fred::interfaces::KeysInterface;
    for provider in ["anthropic_max", "codex"] {
        let env = setup(provider, json!({"account_control":{"quota_limits":{"18000":80,"604800":90},"refresh_mode":"external"}})).await;
        let row = okapi_store::oauth_credentials::target(&env.state.pg, env.channel, env.key)
            .await
            .unwrap()
            .unwrap();
        let plain = okapi_store::credential::open(None, &row.credential_ciphertext).unwrap();
        account_control::quota::poll(&env.state, &row, &plain).await;
        let cache_key = format!("quota:ck:{}", env.key);
        let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
            .await
            .unwrap();
        let raw: String = redis.get(&cache_key).await.unwrap();
        let mut cached: Value = serde_json::from_str(&raw).unwrap();
        for window in cached["snapshot"]["windows"].as_array_mut().unwrap() {
            window["used_percent"] = json!(if window["window_secs"] == 604_800 {
                90
            } else {
                10
            });
        }
        let _: () = redis
            .set(&cache_key, cached.to_string(), None, None, false)
            .await
            .unwrap();
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_err()
        );
        for window in cached["snapshot"]["windows"].as_array_mut().unwrap() {
            window["used_percent"] = json!(if window["window_secs"] == 18000 {
                80
            } else {
                10
            });
        }
        let _: () = redis
            .set(&cache_key, cached.to_string(), None, None, false)
            .await
            .unwrap();
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_err()
        );
        for window in cached["snapshot"]["windows"].as_array_mut().unwrap() {
            if window["window_secs"] == 18000 {
                window["resets_at"] = json!(1);
            }
        }
        let _: () = redis
            .set(&cache_key, cached.to_string(), None, None, false)
            .await
            .unwrap();
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn historical_request_caps_no_longer_gate_or_count_inference_attempts() {
    let env = setup(
        "openai",
        json!({"account_control":{"usage":{"requests":3}}}),
    )
    .await;
    let futures = (0..20).map(|_| account_control::admit(&env.state, env.channel, Some(env.key)));
    let results = futures::future::join_all(futures).await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 20);
    let snapshot = okapi_store::channel_usage::snapshot(&env.state.pg, env.channel, "day")
        .await
        .unwrap();
    assert_eq!(snapshot.requests, 0);
    let status: i16 = sqlx::query_scalar("SELECT status FROM channel_keys WHERE id=$1")
        .bind(env.key)
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(status, 1);
}

#[tokio::test]
async fn historical_cost_caps_are_inactive_even_with_unknown_cost() {
    let env = setup(
        "openai",
        json!({"account_control":{"usage":{"cost_micro":1000}}}),
    )
    .await;
    bill(&env, 11, Some(500), 9_000_000).await;
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok()
    );
    bill(&env, 11, Some(500), 0).await;
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok()
    );
    let other = setup(
        "openai",
        json!({"account_control":{"usage":{"cost_micro":1000}}}),
    )
    .await;
    bill(&other, 0, None, 0).await;
    assert!(
        account_control::admit(&other.state, other.channel, Some(other.key))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn historical_token_caps_are_inactive_and_do_not_create_calendar_counters() {
    let env = setup(
        "openai",
        json!({"account_control":{"usage":{"tokens":10,"requests":2}}}),
    )
    .await;
    bill(&env, 11, Some(0), 0).await;
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok()
    );
    let other = setup(
        "openai",
        json!({"account_control":{"usage":{"requests":2}}}),
    )
    .await;
    sqlx::query("INSERT INTO channel_usage_windows(channel_id,period,window_start,window_end,requests) VALUES($1,'day',now()-interval '2 days',now()-interval '1 day',999)").bind(other.channel).execute(&other.state.pg).await.unwrap();
    assert!(
        account_control::admit(&other.state, other.channel, Some(other.key))
            .await
            .is_ok()
    );
    assert_eq!(
        okapi_store::channel_usage::snapshot(&other.state.pg, other.channel, "day")
            .await
            .unwrap()
            .requests,
        0
    );
    for period in ["hour", "day", "week", "month"] {
        let snapshot = okapi_store::channel_usage::snapshot(&other.state.pg, other.channel, period)
            .await
            .unwrap();
        assert!(snapshot.window_start <= chrono::Utc::now());
        assert!(snapshot.window_end > chrono::Utc::now());
    }
}

#[tokio::test]
async fn configured_cooldown_threshold_and_retry_after_take_effect() {
    let env=setup("openai",json!({"account_control":{"failure_threshold":2,"failure_cooldown_secs":13,"rate_limit_cooldown_secs":17}})).await;
    for expected in [1i16, 2] {
        okapi_store::channels::mark_key_failure(
            &env.state.pg,
            env.key,
            "upstream_error",
            KeyFailure::Transient,
        )
        .await
        .unwrap();
        let status: i16 = sqlx::query_scalar("SELECT status FROM channel_keys WHERE id=$1")
            .bind(env.key)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
        assert_eq!(status, expected);
    }
    let remaining:i64=sqlx::query_scalar("SELECT floor(extract(epoch FROM cooldown_until-now()))::bigint FROM channel_keys WHERE id=$1").bind(env.key).fetch_one(&env.state.pg).await.unwrap();
    assert!((10..=13).contains(&remaining));
    for (retry, expected) in [(None, 17), (Some(7), 7)] {
        sqlx::query("UPDATE channel_keys SET status=1,cooldown_until=NULL WHERE id=$1")
            .bind(env.key)
            .execute(&env.state.pg)
            .await
            .unwrap();
        okapi_store::channels::mark_key_failure(
            &env.state.pg,
            env.key,
            "upstream_status",
            KeyFailure::RateLimited {
                retry_after_secs: retry,
            },
        )
        .await
        .unwrap();
        let remaining:i64=sqlx::query_scalar("SELECT floor(extract(epoch FROM cooldown_until-now()))::bigint FROM channel_keys WHERE id=$1").bind(env.key).fetch_one(&env.state.pg).await.unwrap();
        assert!((expected - 2..=expected).contains(&remaining));
    }
}

#[tokio::test]
async fn subscription_plan_rides_the_quota_probe_once_a_day_and_never_follows_a_new_account() {
    use fred::interfaces::KeysInterface;
    let env = setup(
        "anthropic_max",
        json!({"account_control":{"quota_aware":true,"refresh_mode":"external"}}),
    )
    .await;
    let row = okapi_store::oauth_credentials::target(&env.state.pg, env.channel, env.key)
        .await
        .unwrap()
        .unwrap();
    let plaintext = okapi_store::credential::open(None, &row.credential_ciphertext).unwrap();
    account_control::quota::poll(&env.state, &row, &plaintext).await;
    assert_eq!(env.mock.profile.load(Ordering::SeqCst), 1);
    assert_eq!(
        account_control::quota::plan(&env.state, env.key, "anthropic_max", &plaintext)
            .await
            .as_deref(),
        Some("max_20x")
    );
    // 额度探测照常每 5 分钟一次，profile 不跟着查
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let _: i64 = redis
        .del(format!("quota:poll:ck:{}", env.key))
        .await
        .unwrap();
    account_control::quota::poll(&env.state, &row, &plaintext).await;
    assert_eq!(env.mock.quota.load(Ordering::SeqCst), 2);
    assert_eq!(env.mock.profile.load(Ordering::SeqCst), 1);
    let mut credential = OAuthCredential::parse(&plaintext).unwrap();
    credential.account_id = Some("another-account".into());
    assert!(
        account_control::quota::plan(
            &env.state,
            env.key,
            "anthropic_max",
            &credential.to_plaintext()
        )
        .await
        .is_none(),
        "换了账号不显示旧档位"
    );
}

/// Codex 的档位与额度窗口出自同一个用量接口（`plan_type`），控制台据此显示订阅类型。
/// 这个接口限流很严，档位必须随额度那一次请求带回，不能再单独打一次。
#[tokio::test]
async fn codex_plan_rides_the_quota_response_without_a_second_request() {
    let env = setup(
        "codex",
        json!({"account_control":{"quota_aware":true,"refresh_mode":"external"}}),
    )
    .await;
    let row = okapi_store::oauth_credentials::target(&env.state.pg, env.channel, env.key)
        .await
        .unwrap()
        .unwrap();
    let plaintext = okapi_store::credential::open(None, &row.credential_ciphertext).unwrap();
    account_control::quota::poll(&env.state, &row, &plaintext).await;
    assert_eq!(env.mock.quota.load(Ordering::SeqCst), 1);
    assert!(
        account_control::quota::read(&env.state, env.key)
            .await
            .is_some(),
        "额度观测照常落库"
    );
    assert_eq!(
        account_control::quota::plan(&env.state, env.key, "codex", &plaintext)
            .await
            .as_deref(),
        Some("prolite")
    );
}

#[tokio::test]
async fn quota_observation_blocks_at_threshold_and_cannot_follow_a_reauthorized_account() {
    for provider in ["anthropic_max", "codex"] {
        let env=setup(provider,json!({"account_control":{"quota_aware":true,"quota_threshold_pct":90,"refresh_mode":"external"}})).await;
        if provider == "codex" {
            sqlx::query("UPDATE channels SET api_base=$2 WHERE id=$1")
                .bind(env.channel)
                .bind(format!("{}/backend-api/codex", env.base))
                .execute(&env.state.pg)
                .await
                .unwrap();
        }
        let row = okapi_store::oauth_credentials::target(&env.state.pg, env.channel, env.key)
            .await
            .unwrap()
            .unwrap();
        let plaintext = okapi_store::credential::open(None, &row.credential_ciphertext).unwrap();
        account_control::quota::poll(&env.state, &row, &plaintext).await;
        assert_eq!(env.mock.quota.load(Ordering::SeqCst), 1);
        assert_eq!(
            account_control::quota::read(&env.state, env.key)
                .await
                .unwrap()
                .headroom(chrono::Utc::now().timestamp()),
            Some(5)
        );
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_err()
        );
        let mut credential = OAuthCredential::parse(&plaintext).unwrap();
        credential.account_id = Some("another-account".into());
        credential.access_token = "low-quota-access".into();
        sqlx::query("UPDATE channel_keys SET credential_ciphertext=$2 WHERE id=$1")
            .bind(env.key)
            .bind(credential.to_plaintext().into_bytes())
            .execute(&env.state.pg)
            .await
            .unwrap();
        assert!(
            account_control::quota::read(&env.state, env.key)
                .await
                .is_none()
        );
        assert!(
            account_control::admit(&env.state, env.channel, Some(env.key))
                .await
                .is_ok()
        );
        assert_eq!(env.mock.refresh.load(Ordering::SeqCst), 0);
    }
}

/// 这把 key 现在存着的凭证行与明文（按 state 的主密钥解封：刷新落库的凭证是封装过的）。
async fn stored_credential(env: &Env) -> (okapi_store::oauth_credentials::KeyRow, String) {
    let row = okapi_store::oauth_credentials::target(&env.state.pg, env.channel, env.key)
        .await
        .unwrap()
        .unwrap();
    let plaintext =
        okapi_store::credential::open(env.state.master_key.as_deref(), &row.credential_ciphertext)
            .unwrap();
    (row, plaintext)
}

/// 把存着的 access token 换成上游会回 401 的那个，并放开本轮额度探测租约。
async fn revoke_access_token(env: &Env) {
    use fred::interfaces::KeysInterface;
    let mut credential = OAuthCredential::parse(&stored_credential(env).await.1).unwrap();
    credential.access_token = "revoked-access".into();
    let sealed = okapi_store::credential::seal_or_plain(
        env.state.master_key.as_deref(),
        &credential.to_plaintext(),
    )
    .unwrap();
    sqlx::query("UPDATE channel_keys SET credential_ciphertext=$2 WHERE id=$1")
        .bind(env.key)
        .bind(sealed)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let _: i64 = env
        .state
        .sched
        .client()
        .del(format!("quota:poll:ck:{}", env.key))
        .await
        .unwrap();
}

/// 2026-11-21 线上：codex 渠道 5 十多天没有请求，额度探测每小时被 401 一次，key 却一直显示正常。
/// 推理遇到 401 会强制刷新一次再试，额度探测现在也这样做：刷新成功就接着读到额度；
/// 六小时内再被 401 不重复轮换 token；刷新被 invalid_grant 拒绝就把 key 置为失效。
#[tokio::test]
async fn a_quota_probe_rejected_with_401_refreshes_the_token_once() {
    use fred::interfaces::KeysInterface;
    let env = setup("anthropic_max", json!({})).await;
    sqlx::query("UPDATE channels SET settings = settings || $2 WHERE id = $1")
        .bind(env.channel)
        .bind(json!({"oauth_token_url": format!("{}/token", env.base)}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let poll = || async {
        let (row, plaintext) = stored_credential(&env).await;
        account_control::quota::poll(&env.state, &row, &plaintext).await;
    };
    let key_state = || async {
        sqlx::query_as::<_, (i16, Option<String>)>(
            "SELECT status, last_error FROM channel_keys WHERE id = $1",
        )
        .bind(env.key)
        .fetch_one(&env.state.pg)
        .await
        .unwrap()
    };

    revoke_access_token(&env).await;
    poll().await;
    assert_eq!(env.mock.refresh.load(Ordering::SeqCst), 1, "401 后刷新一次");
    assert_eq!(env.mock.quota.load(Ordering::SeqCst), 2, "刷新后再探一次");
    assert!(
        account_control::quota::read(&env.state, env.key)
            .await
            .is_some(),
        "换上新 token 读到了额度"
    );
    let stored = OAuthCredential::parse(&stored_credential(&env).await.1).unwrap();
    assert_eq!(stored.access_token, "rotated-access", "新凭证已落库");
    assert_eq!(key_state().await.0, 1);

    // 六小时内又被 401：只退避，不再轮换 token
    revoke_access_token(&env).await;
    poll().await;
    assert_eq!(env.mock.quota.load(Ordering::SeqCst), 3);
    assert_eq!(env.mock.refresh.load(Ordering::SeqCst), 1);
    assert_eq!(key_state().await.0, 1);

    // refresh token 也被作废了：刷新回 invalid_grant，key 置为失效
    let _: i64 = env
        .state
        .sched
        .client()
        .del(format!("quota:reauth:ck:{}", env.key))
        .await
        .unwrap();
    env.mock.refuse_refresh.store(true, Ordering::SeqCst);
    revoke_access_token(&env).await;
    poll().await;
    assert_eq!(env.mock.refresh.load(Ordering::SeqCst), 2);
    assert_eq!(
        key_state().await,
        (6, Some("oauth_invalid_grant".into())),
        "账号失效要在控制台看得见"
    );
}

#[tokio::test]
async fn external_refresh_mode_never_consumes_a_shared_refresh_token() {
    let env = setup(
        "anthropic_max",
        json!({"account_control":{"refresh_mode":"external"}}),
    )
    .await;
    let row = okapi_store::oauth_credentials::target(&env.state.pg, env.channel, env.key)
        .await
        .unwrap()
        .unwrap();
    let plaintext = okapi_store::credential::open(None, &row.credential_ciphertext).unwrap();
    let url = format!("{}/token", env.base);
    let key = gateway::credentials::oauth::OAuthKey {
        channel_key_id: env.key,
        provider: "anthropic_max",
        token_url: Some(&url),
        proxy_url: None,
    };
    assert!(
        gateway::credentials::oauth::fresh_credential_for(&env.state, &key, &plaintext)
            .await
            .is_ok()
    );
    assert!(
        gateway::credentials::oauth::force_refresh_for(&env.state, &key, &plaintext)
            .await
            .is_err()
    );
    assert_eq!(env.mock.refresh.load(Ordering::SeqCst), 0);
}

/// 计数端点不计费，但打上游同样占用账号：被账号准入挡住（这里是本地 token 上限）的账号
/// 不拿去数。Messages 计数退本地估算，Responses 计数按无可用渠道拒。
#[tokio::test]
async fn token_counting_honours_account_admission() {
    for (provider, path, body) in [
        (
            "anthropic_max",
            "/v1/messages/count_tokens",
            json!({"messages":[{"role":"user","content":"Hello"}]}),
        ),
        (
            "openai",
            "/v1/responses/input_tokens",
            json!({"input":"Hello"}),
        ),
    ] {
        let env = setup(
            provider,
            json!({"account_control":{"local_tokens":{"cap":20}}}),
        )
        .await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = gateway::router(env.state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut body = body;
        body["model"] = json!(env.model);
        let count = || {
            reqwest::Client::new()
                .post(format!("http://{addr}{path}"))
                .bearer_auth(&env.client_token)
                .json(&body)
                .send()
        };
        let response = count().await.unwrap();
        assert_eq!(response.status(), 200, "{provider}");
        assert_eq!(
            response.json::<Value>().await.unwrap()["input_tokens"],
            4242,
            "{provider}"
        );
        assert_eq!(env.mock.inference.load(Ordering::SeqCst), 1, "{provider}");

        bill(&env, 20, None, 0).await;
        let response = count().await.unwrap();
        let status = response.status();
        let reply: Value = response.json().await.unwrap();
        if provider == "anthropic_max" {
            assert_eq!(status, 200, "{reply}");
            assert_ne!(reply["input_tokens"], 4242, "{reply}");
        } else {
            assert_eq!(status, 503, "{reply}");
            assert_eq!(reply["error"]["code"], "no_available_channel", "{reply}");
        }
        assert_eq!(
            env.mock.inference.load(Ordering::SeqCst),
            1,
            "被挡住的账号不该再被拿去数 token：{provider}"
        );
    }
}

#[tokio::test]
async fn generic_api_uses_normal_fallback_after_channel_pause() {
    let env = setup(
        "openai",
        json!({"account_control":{"usage":{"requests":1}}}),
    )
    .await;
    sqlx::query("UPDATE channels SET priority=100 WHERE id=$1")
        .bind(env.channel)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let (fallback, _) = okapi_store::provision::create_channel(
        &env.state.pg,
        "fallback",
        "openai",
        &format!("{}/v1", env.base),
        "mock-key",
        &[&env.model],
        false,
        None,
    )
    .await
    .unwrap();
    env.state.invalidate_routing_caches();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = gateway::router(env.state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let mut ids = Vec::new();
    for attempt in 0..2 {
        if attempt == 1 {
            sqlx::query(
                "UPDATE channel_keys SET cooldown_until=now()+interval '1 minute' WHERE id=$1",
            )
            .bind(env.key)
            .execute(&env.state.pg)
            .await
            .unwrap();
            env.state.invalidate_routing_caches();
        }
        let response=client.post(format!("http://{addr}/v1/chat/completions")).bearer_auth(&env.client_token)
            .json(&json!({"model":env.model,"max_tokens":10,"messages":[{"role":"user","content":"Hello"}]})).send().await.unwrap();
        assert_eq!(
            response.status(),
            200,
            "{}",
            response.text().await.unwrap_or_default()
        );
        ids.push(
            response.headers()["x-okapi-request-id"]
                .to_str()
                .unwrap()
                .to_owned(),
        );
        response.bytes().await.unwrap();
    }
    for _ in 0..100 {
        let rows:Vec<i64>=sqlx::query_scalar("SELECT channel_id FROM billing_records WHERE request_id::text=ANY($1) ORDER BY created_at").bind(&ids).fetch_all(&env.state.pg).await.unwrap();
        if rows.len() == 2 {
            assert_eq!(rows, vec![env.channel, fallback]);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("mock settlements did not finish");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Exercise validation and redaction against one authenticated admin fixture.
async fn channel_admin_validates_controls_and_usage_does_not_expose_credentials_or_foreign_channels()
 {
    let env = setup("openai", json!({})).await;
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(env.user)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = okapi::console::router(env.state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::new();
    let anonymous = client
        .get(format!("{base}/admin/channels/providers"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let metadata = client
        .get(format!("{base}/admin/channels/providers"))
        .bearer_auth(&env.client_token)
        .send()
        .await
        .unwrap();
    assert_eq!(metadata.status(), 200);
    let metadata = metadata.json::<Value>().await.unwrap();
    let providers = metadata["data"].as_array().unwrap();
    assert_eq!(providers.len(), okapi_providers::registry::BUILT_INS.len());
    for descriptor in okapi_providers::registry::BUILT_INS {
        let row = providers
            .iter()
            .find(|row| row["id"] == descriptor.id)
            .unwrap();
        assert_eq!(
            row["account"],
            serde_json::to_value(descriptor.account_capabilities()).unwrap()
        );
        assert!(row.get("credential").is_none());
    }
    for value in [
        json!({"usage":{"requests":0}}),
        json!({"quota_threshold_pct":101}),
        json!({"refresh_mode":"typo"}),
        json!({"local_cost":{"cap_micro":0}}),
        json!({"local_cost":{"cap_micro":1,"period":"hour"}}),
    ] {
        let response = client
            .patch(format!("{base}/admin/channels/{}", env.channel))
            .bearer_auth(&env.client_token)
            .json(&json!({"settings":{"account_control":value}}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["param"],
            "account_control"
        );
    }
    let response = client.post(format!("{base}/admin/channels")).bearer_auth(&env.client_token)
        .json(&json!({"name":"invalid-concurrency","api_base":"https://api.openai.com/v1","credential":"mock-key","models":[env.model],"max_concurrency":0})).send().await.unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["param"],
        "max_concurrency"
    );
    let settings =
        json!({"account_control":{"usage":{"period":"hour","requests":2}},"responses_native":true});
    let response = client
        .patch(format!("{base}/admin/channels/{}", env.channel))
        .bearer_auth(&env.client_token)
        .json(&json!({"settings":settings}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok()
    );
    let url = format!("{base}/admin/channels/{}/usage", env.channel);
    let response = client
        .get(&url)
        .bearer_auth(&env.client_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert!(body.get("usage").is_none());
    assert!(body["policy"].get("usage").is_none());
    assert!(body["quotas"].is_array());
    assert!(!body.to_string().contains("mock-key"));
    // 没设上限也给出本地已记录的用量（累计总量，没有窗口）；两项各是各的形状
    assert_eq!(
        body["token_usage"],
        json!({"tokens": 0, "window_start": null, "window_end": null})
    );
    assert_eq!(
        body["cost_usage"],
        json!({"cost_micro": 0, "window_start": null, "window_end": null})
    );
    // 存一个按月的成本上限：策略原样读回，用量缺省按它的周期给；查询参数可以预览别的周期
    let settings =
        json!({"account_control":{"local_cost":{"cap_micro":2_500_000,"period":"month"}}});
    let response = client
        .patch(format!("{base}/admin/channels/{}", env.channel))
        .bearer_auth(&env.client_token)
        .json(&json!({"settings":settings}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    bill(&env, 40, Some(1_250_000), 0).await;
    let body: Value = client
        .get(&url)
        .bearer_auth(&env.client_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["policy"]["local_cost"],
        json!({"cap_micro": 2_500_000, "period": "month"})
    );
    assert_eq!(body["cost_usage"]["cost_micro"], 1_250_000, "{body}");
    assert!(body["cost_usage"]["window_end"].is_string(), "{body}");
    assert_eq!(
        body["token_usage"],
        json!({"tokens": 40, "window_start": null, "window_end": null})
    );
    let body: Value = client
        .get(format!("{url}?cost_period=total&token_period=month"))
        .bearer_auth(&env.client_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["cost_usage"],
        json!({"cost_micro": 1_250_000, "window_start": null, "window_end": null})
    );
    assert_eq!(body["token_usage"]["tokens"], 40);
    assert!(body["token_usage"]["window_end"].is_string(), "{body}");
    let role:Value=client.post(format!("{base}/admin/roles")).bearer_auth(&env.client_token)
        .json(&json!({"role_code":format!("control-reader-{}",Uuid::new_v4().simple()),"display_name":"Channel reader","permissions":["channel.read.own"]})).send().await.unwrap().json().await.unwrap();
    let reader = okapi_store::provision::create_user(
        &env.state.pg,
        &format!("reader-{}", Uuid::new_v4().simple()),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE users SET role=10,admin_role_id=$2 WHERE id=$1")
        .bind(reader)
        .bind(role["admin_role_id"].as_i64().unwrap())
        .execute(&env.state.pg)
        .await
        .unwrap();
    let token = format!("mock-reader-{}", Uuid::new_v4().simple());
    okapi_store::provision::create_api_key(
        &env.state.pg,
        reader,
        &hex::encode(Sha256::digest(token.as_bytes())),
        "mock-reader",
    )
    .await
    .unwrap();
    assert_eq!(
        client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    okapi_store::admin::set_channel_owner(&env.state.pg, env.channel, reader)
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn maintenance_scan_uses_registered_provider_selection_for_api_key_channels_too() {
    let env = setup("openai", json!({})).await;
    let rows = okapi_store::oauth_credentials::scan(&env.state.pg, 0, 1000, &["openai"])
        .await
        .unwrap();
    assert!(rows.iter().any(|row| row.id == env.key));
    let rows = okapi_store::oauth_credentials::scan(&env.state.pg, 0, 1000, &["codex"])
        .await
        .unwrap();
    assert!(!rows.iter().any(|row| row.id == env.key));
    assert!(
        okapi_store::oauth_credentials::scan(&env.state.pg, 0, 1000, &[])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn subscription_quota_is_observed_without_a_configured_percentage_cap() {
    use fred::interfaces::KeysInterface;
    let env = setup(
        "anthropic_max",
        json!({"account_control":{"refresh_mode":"external"}}),
    )
    .await;
    let mut after = env.key - 1;
    okapi::worker::oauth_refresh::refresh_once(
        &env.state,
        okapi::kernel::policies::RefreshPolicy {
            enabled: false,
            batch_size: 1,
            ..Default::default()
        },
        &mut after,
    )
    .await
    .unwrap();
    assert_eq!(env.mock.quota.load(Ordering::SeqCst), 1);
    assert_eq!(env.mock.inference.load(Ordering::SeqCst), 0);
    assert_eq!(env.mock.refresh.load(Ordering::SeqCst), 0);
    let snapshot = account_control::quota::read(&env.state, env.key)
        .await
        .unwrap();
    assert_eq!(snapshot.threshold_window.as_deref(), Some("five_hour"));
    // 用量应答里按模型分的 Fable 周窗口：跟着观测一起落库，只展示
    assert_eq!(
        snapshot.scoped,
        [okapi_providers::account::quota::ScopedWindow {
            model: "Fable".into(),
            used_percent: 37,
            resets_at: Some(4_070_908_800),
            window_secs: Some(604_800),
        }]
    );
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok()
    );
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let cache_key = format!("quota:ck:{}", env.key);
    let raw: String = redis.get(&cache_key).await.unwrap();
    let mut cached: Value = serde_json::from_str(&raw).unwrap();
    // Fable 周限额用尽不影响放行：别的模型照旧可用，不能把账号摘掉
    cached["snapshot"]["scoped"][0]["used_percent"] = json!(100);
    let _: () = redis
        .set(&cache_key, cached.to_string(), None, None, false)
        .await
        .unwrap();
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_ok(),
        "a spent Fable week must not stop the account"
    );
    cached["snapshot"]["windows"][0]["used_percent"] = json!(100);
    let _: () = redis
        .set(&cache_key, cached.to_string(), None, None, false)
        .await
        .unwrap();
    assert!(
        account_control::admit(&env.state, env.channel, Some(env.key))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn fresh_managed_oauth_uses_the_cached_policy_without_reading_postgres() {
    let env = setup("anthropic_max", json!({})).await;
    let candidates =
        okapi_store::channels::candidates_for_model(&env.state.pg, &env.model, &["default"], None)
            .await
            .unwrap();
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.channel_key_id == env.key)
        .unwrap();
    account_control::policy(&env.state, env.channel)
        .await
        .unwrap();
    env.state.pg.close().await;
    let fresh = gateway::credentials::oauth::fresh_credential(&env.state, candidate)
        .await
        .unwrap();
    assert_eq!(fresh.access_token, "high-quota-access");
    assert_eq!(env.mock.refresh.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_busy_channel_blocks_every_shared_wire_surface_without_polling_the_transport() {
    use okapi::engine::channel_permit::ChannelPermit;
    let env = setup("openai", json!({})).await;
    let mut candidates =
        okapi_store::channels::candidates_for_model(&env.state.pg, &env.model, &["default"], None)
            .await
            .unwrap();
    let mut candidate = candidates.pop().unwrap();
    candidate.max_concurrency = Some(1);
    let permit = ChannelPermit::acquire(&env.state.sched, &candidate)
        .await
        .unwrap()
        .unwrap();
    let polled = AtomicUsize::new(0);
    for endpoint in [
        "/v1/chat/completions",
        "/v1/embeddings",
        "/v1/audio/speech",
        "/v1/audio/transcriptions",
        "/v1/images/generations",
        "/v1/videos",
        "/pass",
    ] {
        let wire = async {
            polled.fetch_add(1, Ordering::SeqCst);
            Ok::<_, okapi_providers::UpstreamError>((
                200u16,
                String::new(),
                axum::body::Bytes::new(),
            ))
        };
        let error = account_control::execute(&env.state, &candidate, &env.model, endpoint, wire)
            .await
            .err()
            .unwrap();
        assert_eq!(error.error_code(), "no_available_channel");
    }
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    permit.release().await;
    let result = account_control::execute(
        &env.state,
        &candidate,
        &env.model,
        "/v1/audio/speech",
        async {
            Ok((
                200u16,
                String::new(),
                axum::body::Bytes::from_static(b"synthetic"),
            ))
        },
    )
    .await
    .unwrap();
    assert_eq!(result.2.as_ref(), b"synthetic");
}
