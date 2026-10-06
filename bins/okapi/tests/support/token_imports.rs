//! Direct token imports exercise admin writes and the ordinary request/billing pipeline.
use super::*;

#[derive(Clone, Default)]
struct TokenMock {
    rejected: Arc<std::sync::atomic::AtomicBool>,
    requests: Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, Value)>>>,
}

async fn token_mock() -> (SocketAddr, TokenMock) {
    let mock = TokenMock::default();
    let router = Router::new().route("/v1/messages", post(|State(mock): State<TokenMock>, headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
        let body: Value = serde_json::from_slice(&body).unwrap();
        mock.requests.lock().unwrap().push((headers, body.clone()));
        if mock.rejected.load(Ordering::SeqCst) {
            return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({"error":{"type":"authentication_error","message":"expired imported token"}}))).into_response();
        }
        axum::Json(json!({"id":"msg_import", "type":"message", "role":"assistant", "model":body["model"],
            "content":[{"type":"text","text":"imported token works"}], "stop_reason":"end_turn",
            "usage":{"input_tokens":100,"output_tokens":50}})).into_response()
    })).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (address, mock)
}

async fn import(env: &Env, mock: SocketAddr, credential: &str) -> reqwest::Response {
    reqwest::Client::new().post(format!("http://{}/admin/channels", env.console))
        .bearer_auth(&env.admin_token).json(&json!({"name":format!("direct-token-{}", env.model), "provider":"anthropic_max",
            "api_base":format!("http://{mock}/v1"), "credential":credential,"models":[env.model],
            "trust_upstream_usage":true,"settings":{
                "oauth_token_url":format!("http://{}/token", env.mock),
                "extensions":{"client_profile":{"name":"claude-code","mode":"mimic","revision":"2.1.290"}}
            }})).send().await.unwrap()
}

async fn imported(env: &Env, mock: SocketAddr, credential: &str) -> (i64, i64) {
    let response = import(env, mock, credential).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let body: Value = response.json().await.unwrap();
    (
        body["channel_id"].as_i64().unwrap(),
        body["channel_key_id"].as_i64().unwrap(),
    )
}

#[tokio::test]
async fn bare_token_mimics_client_and_settles_once_without_refresh() {
    let env = setup().await;
    let (address, mock) = token_mock().await;
    let (channel, key) = imported(&env, address, "sk-ant-oat01-imported").await;
    let credential = read_cred(&env, key).await;
    assert_eq!(credential.access_token, "sk-ant-oat01-imported");
    assert!(!credential.can_refresh());
    assert_eq!(credential.expires_at, 0);
    let kind: i16 = sqlx::query_scalar("SELECT credential_kind FROM channel_keys WHERE id=$1")
        .bind(key)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(kind, 1);
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let response = chat(&env).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    {
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let (headers, body) = &requests[0];
        assert_eq!(headers["authorization"], "Bearer sk-ant-oat01-imported");
        assert!(!headers.contains_key("x-api-key"));
        assert!(
            headers["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("claude-cli/2.1.290 ")
        );
        assert!(
            headers["anthropic-beta"]
                .to_str()
                .unwrap()
                .contains("oauth-2025-04-20")
        );
        assert!(body["metadata"]["user_id"].is_string());
        assert!(
            body["system"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("x-anthropic-billing-header: cc_version=2.1.290.")
        );
        assert_eq!(
            body["system"][1]["text"],
            okapi_providers::oauth::anthropic_max::SYSTEM_PREFIX
        );
    }
    super::client_profiles::assert_settlement(&env, channel, before).await;
    let mut cursor = key - 1;
    okapi::worker::oauth_refresh::refresh_once(
        &env.state,
        okapi::worker::oauth_refresh::RefreshPolicy {
            batch_size: 1,
            requests_per_second: 5,
            ..Default::default()
        },
        &mut cursor,
    )
    .await
    .unwrap();
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 0);
    let list: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/channels?q=direct-token-{}",
            env.console, env.model
        ))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == channel)
        .unwrap();
    assert_eq!(row["keys"][0]["oauth_refreshable"], false);
    assert!(row["keys"][0].get("credential_expires_at").is_none());
    assert!(!list.to_string().contains("sk-ant-oat01-imported"));
}

#[tokio::test]
async fn rejected_token_refunds_stops_retry_and_can_be_replaced() {
    let env = setup().await;
    let (address, mock) = token_mock().await;
    let (channel, key) = imported(&env, address, "sk-ant-oat01-rejected").await;
    mock.rejected.store(true, Ordering::SeqCst);
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), chat(&env))
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    let (status, code): (i16, String) =
        sqlx::query_as("SELECT status,last_error FROM channel_keys WHERE id=$1")
            .bind(key)
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert_eq!(status, 6);
    assert_eq!(code, "oauth_access_token_rejected");
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 0);
    assert_eq!(env.state.ledger.balance(env.user_id).await.unwrap(), before);
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/admin/channels/{channel}/credential",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .json(&json!({"credential":"sk-ant-oat01-replacement","channel_key_id":key}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    mock.rejected.store(false, Ordering::SeqCst);
    let response = chat(&env).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(
        read_cred(&env, key).await.access_token,
        "sk-ant-oat01-replacement"
    );
    assert_eq!(
        mock.requests.lock().unwrap().last().unwrap().0["authorization"],
        "Bearer sk-ant-oat01-replacement"
    );
    super::client_profiles::assert_settlement(&env, channel, before).await;
}

#[tokio::test]
async fn known_expiry_is_enforced_and_manual_refresh_does_not_invalidate_usable_token() {
    let env = setup().await;
    let (address, mock) = token_mock().await;
    let (channel, key) = imported(&env, address, "sk-ant-oat01-unknown-expiry").await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/admin/channels/{channel}/keys/{key}/oauth/refresh",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let status: i16 = sqlx::query_scalar("SELECT status FROM channel_keys WHERE id=$1")
        .bind(key)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 1);
    let mut credential = read_cred(&env, key).await;
    credential.expires_at = chrono::Utc::now().timestamp() - 1;
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/admin/channels/{channel}/credential",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .json(&json!({"credential":credential.to_plaintext(),"channel_key_id":key}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let response = chat(&env).await;
    assert_eq!(response.status(), 502);
    assert!(mock.requests.lock().unwrap().is_empty());
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 0);
    assert_eq!(env.state.ledger.balance(env.user_id).await.unwrap(), before);
    let code: String = sqlx::query_scalar("SELECT last_error FROM channel_keys WHERE id=$1")
        .bind(key)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(code, "oauth_access_token_expired");
}

#[tokio::test]
async fn wrong_token_and_malformed_oauth_are_rejected_before_provisioning() {
    let env = setup().await;
    let (address, _) = token_mock().await;
    for input in [
        "sk-ant-api03-not-oauth",
        "sk-ant-ort01-not-access",
        r#"{"kind":"oauth","access_token":"access","refresh_token":"refresh"}"#,
    ] {
        assert_eq!(import(&env, address, input).await.status(), 400);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM channels WHERE name=$1")
        .bind(format!("direct-token-{}", env.model))
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
