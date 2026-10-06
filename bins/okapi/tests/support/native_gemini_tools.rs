//! Gemini built-ins require the same channel capability as function tools.
use super::{Protocol, record, request_with_tools, setup};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

fn builtins() -> [Value; 4] {
    [
        json!({"googleSearch":{}}),
        json!({"googleSearchRetrieval":{}}),
        json!({"codeExecution":{}}),
        json!({"urlContext":{}}),
    ]
}

#[tokio::test]
async fn native_tool_requests_never_reach_a_channel_that_explicitly_denies_tools() {
    for tool in builtins() {
        for stream in [false, true] {
            let env = setup(Protocol::Gemini, Protocol::Gemini.fixture()).await;
            sqlx::query("UPDATE channels SET capabilities=$2 WHERE name=$1")
                .bind(&env.model)
                .bind(json!({"tools":false}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            let response =
                request_with_tools(&env, Protocol::Gemini, stream, true, Some(json!([tool]))).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 503, "{stream}: {body}");
            assert!(body.contains("no_available_channel"), "{body}");
            assert_eq!(env.calls.load(Ordering::SeqCst), 0);
            assert_eq!(record(&env).await["amount_micro"], 0);
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
        }
    }
}

#[tokio::test]
async fn capable_and_unspecified_channels_preserve_builtin_definitions_and_token_usage() {
    for capability in [json!({}), json!({"tools":true})] {
        for tool in builtins() {
            for stream in [false, true] {
                let env = setup(Protocol::Gemini, Protocol::Gemini.fixture()).await;
                sqlx::query("UPDATE channels SET capabilities=$2 WHERE name=$1")
                    .bind(&env.model)
                    .bind(&capability)
                    .execute(&env.state.pg)
                    .await
                    .unwrap();
                let tools = json!([tool]);
                let response =
                    request_with_tools(&env, Protocol::Gemini, stream, true, Some(tools.clone()))
                        .await;
                let status = response.status();
                let body = response.text().await.unwrap();
                assert_eq!(status, 200, "{stream}: {body}");
                assert!(!body.contains("upstream_error"), "{body}");
                let row = record(&env).await;
                assert_eq!(row["usage"]["prompt_tokens"], 1000);
                assert_eq!(row["usage"]["completion_tokens"], 400);
                assert_eq!(row["amount_micro"], 27900);
                assert_eq!(
                    env.state
                        .ledger
                        .balance(env.user)
                        .await
                        .unwrap()
                        .as_micros(),
                    50_000_000 - 27900
                );
                assert_eq!(env.calls.load(Ordering::SeqCst), 1);
                assert_eq!(env.captured.lock().await[0].0["tools"], tools);
            }
        }
    }
}

#[tokio::test]
async fn absent_or_empty_tools_do_not_exclude_a_tools_disabled_channel() {
    for stream in [false, true] {
        let env = setup(Protocol::Gemini, Protocol::Gemini.fixture()).await;
        sqlx::query("UPDATE channels SET capabilities=$2 WHERE name=$1")
            .bind(&env.model)
            .bind(json!({"tools":false}))
            .execute(&env.state.pg)
            .await
            .unwrap();
        for tools in [None, Some(json!([]))] {
            let response = request_with_tools(&env, Protocol::Gemini, stream, true, tools).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{stream}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            assert_eq!(record(&env).await["amount_micro"], 27900);
        }
        assert_eq!(env.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            env.state
                .ledger
                .balance(env.user)
                .await
                .unwrap()
                .as_micros(),
            50_000_000 - 2 * 27900
        );
    }
}
