//! Explicit opt-in smoke against the ChatGPT Codex backend. Reads an access token and account id
//! from the environment, never refreshes, and sends only short synthetic prompts (four model
//! requests in total, plus the usage and model-catalog reads).
use bytes::Bytes;
use futures::StreamExt;
use okapi_providers::{
    ChatEvent, ChatResponse, HttpPool, Outbound, StreamHandle,
    account::{AccountHooks, QuotaContext},
    oauth::codex,
    response_relay,
    responses_ws::{ResponsesSocket, SocketTimeouts},
};
use serde_json::{Value, json};

const PROMPT: &str = "Reply with exactly the word OK and nothing else.";

fn credentials() -> (String, String) {
    (
        std::env::var("OKAPI_TEST_CODEX_ACCESS_TOKEN").expect("explicit access token required"),
        std::env::var("OKAPI_TEST_CODEX_ACCOUNT_ID").expect("explicit ChatGPT account id required"),
    )
}

fn model() -> String {
    std::env::var("OKAPI_TEST_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.5".into())
}

/// Collects output text and requires a completed terminal event.
async fn reply_text(mut handle: StreamHandle) -> String {
    let mut text = String::new();
    let mut completed = false;
    while let Some(event) = handle.events.next().await {
        let ChatEvent::Data { raw, .. } = event.unwrap() else {
            break;
        };
        let value: Value = serde_json::from_str(&raw).unwrap_or_default();
        match value["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => text.push_str(value["delta"].as_str().unwrap()),
            "response.completed" => completed = true,
            "response.failed" | "error" => panic!("real upstream failure: {raw}"),
            _ => {}
        }
    }
    assert!(completed, "must receive response.completed");
    text
}

/// What a generic SDK sends through a Codex channel: a string input, no instructions, no client
/// identity headers. The adapter must not need to invent `OpenAI-Beta` for it to be accepted.
#[tokio::test]
#[ignore = "real subscription smoke; requires an explicitly supplied access token"]
async fn generic_client_streams_without_a_beta_header_and_yields_a_turn_state() {
    let (token, account) = credentials();
    let body =
        Bytes::from(serde_json::to_vec(&json!({"model": model(), "input": PROMPT})).unwrap());
    let (response, relayed) = response_relay::capture_relayed(codex::responses(
        &HttpPool::new().unwrap(),
        codex::DEFAULT_API_BASE,
        &token,
        Some(&account),
        body,
        true,
        &Outbound::default(),
    ))
    .await;
    let ChatResponse::Stream(handle) = response.expect("real upstream request") else {
        panic!("expected a stream");
    };
    assert!(reply_text(handle).await.contains("OK"));
    eprintln!("turn state returned: {}", relayed.turn_state.is_some());
}

#[tokio::test]
#[ignore = "real subscription smoke; requires an explicitly supplied access token"]
async fn non_stream_client_gets_the_aggregated_response() {
    let (token, account) = credentials();
    let body = json!({"model": model(), "input": PROMPT, "stream": false});
    let response = codex::responses(
        &HttpPool::new().unwrap(),
        codex::DEFAULT_API_BASE,
        &token,
        Some(&account),
        Bytes::from(serde_json::to_vec(&body).unwrap()),
        false,
        &Outbound::default(),
    )
    .await
    .expect("real upstream request");
    let ChatResponse::Json { body, usage, .. } = response else {
        panic!("expected aggregated JSON");
    };
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["status"], "completed");
    let text: String = value["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().cloned().unwrap_or_default())
        .filter_map(|part| part["text"].as_str().map(str::to_owned))
        .collect();
    assert!(text.contains("OK"), "{value}");
    assert!(usage.is_some_and(|usage| usage.prompt_tokens > 0));
}

/// The gateway's default WebSocket beta when the client sends none.
#[tokio::test]
#[ignore = "real subscription smoke; requires an explicitly supplied access token"]
async fn websocket_accepts_the_cli_beta_value() {
    let (token, account) = credentials();
    let bearer = format!("Bearer {token}");
    let socket = ResponsesSocket::connect(
        &HttpPool::new().unwrap(),
        &format!("{}/responses", codex::DEFAULT_API_BASE),
        &[
            ("authorization", bearer.as_str()),
            ("originator", codex::ORIGINATOR),
            ("openai-beta", codex::WEBSOCKET_BETA),
            ("chatgpt-account-id", account.as_str()),
        ],
        &Outbound::default(),
        SocketTimeouts::default(),
    )
    .await
    .expect("WebSocket handshake");
    let mut frame: Value = serde_json::from_slice(
        &codex::prepare_body(
            &serde_json::to_vec(&json!({"model": model(), "input": PROMPT})).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    frame.as_object_mut().unwrap().remove("stream");
    frame["type"] = json!("response.create");
    let handle = socket
        .create(Bytes::from(serde_json::to_vec(&frame).unwrap()))
        .await
        .expect("response.create admitted");
    assert!(reply_text(handle).await.contains("OK"));
    socket.close();
}

/// Records which rate-limit headers the backend really sends next to the usage endpoint.
#[tokio::test]
#[ignore = "real subscription smoke; requires an explicitly supplied access token"]
async fn rate_limit_headers_and_usage_windows_agree() {
    let (token, account) = credentials();
    let body = codex::prepare_body(
        &serde_json::to_vec(&json!({"model": model(), "input": PROMPT})).unwrap(),
    )
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{}/responses", codex::DEFAULT_API_BASE))
        .bearer_auth(&token)
        .header("chatgpt-account-id", &account)
        .header("originator", codex::ORIGINATOR)
        .header("accept", "text/event-stream")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let mut names: Vec<String> = response
        .headers()
        .keys()
        .map(|name| name.as_str().to_owned())
        .filter(|name| name.starts_with("x-codex-"))
        .collect();
    names.sort();
    eprintln!("x-codex response headers: {names:?}");
    drop(response.bytes().await);
    if names
        .iter()
        .any(|name| name == "x-codex-primary-used-percent")
    {
        assert!(
            names.iter().any(|name| name == "x-codex-primary-reset-at")
                || names
                    .iter()
                    .any(|name| name == "x-codex-primary-reset-after-seconds"),
            "a usage window must say when it resets"
        );
    }

    let snapshot = codex::account::HOOKS
        .quota(QuotaContext {
            http: &HttpPool::new().unwrap(),
            api_base: codex::DEFAULT_API_BASE,
            access_token: &token,
            account_id: Some(&account),
            outbound: &Outbound::default(),
        })
        .await
        .expect("usage endpoint");
    eprintln!("usage windows: {:?}", snapshot.windows);
    assert!(!snapshot.windows.is_empty() || snapshot.allowed == Some(false));
}

#[tokio::test]
#[ignore = "real subscription smoke; requires an explicitly supplied access token"]
async fn model_catalog_lists_models_for_the_tracked_client_version() {
    let (token, account) = credentials();
    let models = codex::list_models(
        &HttpPool::new().unwrap(),
        codex::DEFAULT_API_BASE,
        &token,
        Some(&account),
        &Outbound::default(),
    )
    .await
    .expect("model catalog");
    eprintln!(
        "models for client_version {}: {models:?}",
        codex::CLIENT_VERSION
    );
    assert!(models.contains(&model()), "{models:?}");
}
