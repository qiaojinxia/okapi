//! Request extensions use the ordinary gateway and settlement path, with local mocks only.
use super::*;

fn settings() -> Value {
    json!({"extensions":{"client_profile":{"name":"claude-code","mode":"auto","revision":"2.1.290"}}})
}

async fn patch(env: &Env, channel: i64, value: &Value) {
    let resp = reqwest::Client::new()
        .patch(format!("http://{}/admin/channels/{channel}", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"settings":value}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}

pub(super) async fn assert_settlement(env: &Env, channel: i64, before: Money) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let amounts: Vec<i64> = sqlx::query_scalar("SELECT amount_micro FROM billing_records WHERE user_id=$1 AND channel_id=$2 AND status=20 AND log_type=2")
                .bind(env.user_id).bind(channel).fetch_all(&env.pg).await.unwrap();
            if !amounts.is_empty() {
                assert_eq!(amounts.len(), 1, "one successful request produces one settled bill");
                let after = env.state.ledger.balance(env.user_id).await.unwrap();
                assert_eq!(before.as_micros()-after.as_micros(), amounts[0]);
                assert!(amounts[0]>0);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    }).await.unwrap();
}

#[tokio::test]
async fn oauth_auto_profile_preserves_native_client_system_and_ordinary_settlement() {
    let env = setup().await;
    let (channel, _) = login_channel(&env, "anthropic_max").await;
    patch(&env, channel, &settings()).await;
    let system = json!([{"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."},{"type":"text","text":"native client system","cache_control":{"type":"ephemeral","ttl":"1h"}}]);
    let body = json!({"model":env.model,"max_tokens":100,"system":system,
        "metadata":{"user_id":json!({"device_id":"a".repeat(64),"account_uuid":"","session_id":"12345678-1234-4234-8234-123456789012"}).to_string()},
        "messages":[{"role":"user","content":"hello"}]});
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .bearer_auth(&env.user_token)
        .header("user-agent", "claude-cli/2.1.287 (external, cli)")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let seen = env.mock_state.last_body().unwrap();
    assert_eq!(seen["system"], system);
    assert_eq!(seen["metadata"], body["metadata"]);
    assert_eq!(
        env.mock_state.seen("user-agent").as_deref(),
        Some("claude-cli/2.1.287 (external, cli)")
    );
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 1);
    assert_settlement(&env, channel, before).await;
}

#[tokio::test]
async fn static_key_profile_uses_api_key_auth_and_never_enters_oauth_lifecycle() {
    let revision = "2.1.290";
    let env = setup().await;
    let configured = settings();
    let captured = Arc::new(std::sync::Mutex::new(None));
    let mock_capture = Arc::clone(&captured);
    let router = Router::new().route("/v1/messages", post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
        let captured = Arc::clone(&mock_capture);
        async move {
            let req: Value = serde_json::from_slice(&body).unwrap();
            *captured.lock().unwrap() = Some((headers, req.clone()));
            axum::Json(json!({"id":"msg_profile","type":"message","role":"assistant","model":req["model"],
                "content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":50}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let resp = reqwest::Client::new().post(format!("http://{}/admin/channels", env.console))
        .bearer_auth(&env.admin_token).json(&json!({"name":"profile-static", "provider":"anthropic",
            "api_base":format!("http://{address}/v1"), "credential":"profile-static-key", "models":[env.model],
            "trust_upstream_usage":true,"settings":configured})).send().await.unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let created: Value = resp.json().await.unwrap();
    let channel = created["channel_id"].as_i64().unwrap();
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let resp = chat(&env).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    {
        let captured = captured.lock().unwrap();
        let (headers, body) = captured.as_ref().unwrap();
        assert_eq!(headers["x-api-key"], "profile-static-key");
        assert!(!headers.contains_key("authorization"));
        assert!(
            headers["user-agent"]
                .to_str()
                .unwrap()
                .starts_with(&format!("claude-cli/{revision} "))
        );
        assert!(
            !headers["anthropic-beta"]
                .to_str()
                .unwrap()
                .contains("oauth-")
        );
        assert!(body["metadata"]["user_id"].is_string());
        let billing = body["system"][0]["text"].as_str().unwrap();
        let cch = billing
            .split("cch=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert_eq!(cch.len(), 5);
        assert!(cch.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 0);
    assert_settlement(&env, channel, before).await;
}

#[tokio::test]
async fn unsupported_extension_is_rejected_before_creating_a_channel() {
    let env = setup().await;
    let resp = reqwest::Client::new().post(format!("http://{}/admin/channels", env.console))
        .bearer_auth(&env.admin_token).json(&json!({"name":"unsupported-profile", "provider":"openai",
            "api_base":format!("http://{}/v1", env.mock), "credential":"static", "models":[env.model],
            "trust_upstream_usage":true,"settings":settings()})).send().await.unwrap();
    assert_eq!(resp.status(), 400);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channels WHERE name='unsupported-profile'")
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert_eq!(count, 0);
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn oauth_mimic_uses_the_latest_envelope_and_the_ordinary_settlement_path() {
    let env = setup().await;
    let (channel, _) = login_channel(&env, "anthropic_max").await;
    let mut configured = settings();
    configured["extensions"]["client_profile"]["mode"] = json!("mimic");
    patch(&env, channel, &configured).await;
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let response = chat(&env).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(
        env.mock_state.seen("user-agent").as_deref(),
        Some("claude-cli/2.1.290 (external, cli)")
    );
    assert_eq!(
        env.mock_state
            .seen("x-stainless-package-version")
            .as_deref(),
        Some("0.128.0")
    );
    assert_eq!(
        env.mock_state
            .seen("x-claude-code-request-class")
            .as_deref(),
        Some("main")
    );
    let beta = env.mock_state.seen("anthropic-beta").unwrap();
    assert!(beta.contains("oauth-2025-04-20"));
    assert!(beta.contains("per-turn-control-2026-07-01"));
    let body = env.mock_state.last_body().unwrap();
    let billing = body["system"][0]["text"].as_str().unwrap();
    assert!(billing.contains("cc_version=2.1.290."));
    let cch = billing
        .split("cch=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(cch.len(), 5);
    assert!(cch.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let identity: Value =
        serde_json::from_str(body["metadata"]["user_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        env.mock_state.seen("x-claude-code-session-id"),
        identity["session_id"].as_str().map(str::to_owned)
    );
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 1);
    assert_settlement(&env, channel, before).await;
}

/// 模拟会加 system 文本、1h 缓存断点，上游分词也比本地多：预扣按客户端配置的准入提示放大。
/// 约 7500 token 的提示词按原样估约 763 万 micro、余额 1000 万放行；模拟渠道按 ×1.6+128 估
/// 约 1226 万，预扣阶段就拒绝。透传只适用于真实 Claude Code 客户端，不作对照。此前预扣看不到这些，长对话首个请求实扣可达预扣的数倍。
#[tokio::test]
async fn simulated_channels_reserve_for_what_the_profile_adds() {
    let env = setup().await;
    let (channel, _) = login_channel(&env, "anthropic_max").await;
    let long = "hello ".repeat(7500);
    let send = || {
        reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", env.gateway))
            .bearer_auth(&env.user_token)
            .json(&json!({"model": env.model, "max_tokens": 64,
                "messages": [{"role": "user", "content": long}]}))
            .send()
    };
    let mut configured = settings();
    configured["extensions"]["client_profile"]["mode"] = json!("mimic");
    patch(&env, channel, &configured).await;
    let resp = send().await.unwrap();
    assert_eq!(resp.status(), 429, "{}", resp.text().await.unwrap());
    // 不配置客户端扩展：请求不被改写，按原样估算即可放行
    patch(&env, channel, &json!({"extensions": {}})).await;
    let resp = send().await.unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}
