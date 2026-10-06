//! Installed CLI smoke tests: explicit opt-in, local gateway and mock only.
use super::*;
use std::{fmt::Write as _, time::Duration};
use tokio::process::Command;

fn sse(events: &[Value]) -> axum::response::Response {
    let body = events.iter().fold(String::new(), |mut body, event| {
        writeln!(
            body,
            "event: {}\ndata: {event}\n",
            event["type"].as_str().unwrap()
        )
        .unwrap();
        body
    });
    ([("content-type", "text/event-stream")], body).into_response()
}

pub(super) fn messages_stream(req: &Value) -> axum::response::Response {
    sse(&[
        json!({"type":"message_start","message":{"id":"msg_cli","type":"message","role":"assistant",
            "model":req["model"],"content":[],"stop_reason":null,"stop_sequence":null,
            "usage":{"input_tokens":100,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello max"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":50}}),
        json!({"type":"message_stop"}),
    ])
}

pub(super) fn responses_stream(req: &Value) -> axum::response::Response {
    let id = format!("resp_cli_{}", Uuid::new_v4().simple());
    let item = json!({"id":"msg_cli","type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":"Hello codex","annotations":[]}]});
    sse(&[
        json!({"type":"response.created","sequence_number":0,"response":{"id":id,"object":"response","status":"in_progress","model":req["model"],"output":[]}}),
        json!({"type":"response.output_item.added","sequence_number":1,"output_index":0,
            "item":{"id":"msg_cli","type":"message","role":"assistant","status":"in_progress","content":[]}}),
        json!({"type":"response.content_part.added","sequence_number":2,"item_id":"msg_cli","output_index":0,"content_index":0,
            "part":{"type":"output_text","text":"","annotations":[]}}),
        json!({"type":"response.output_text.delta","sequence_number":3,"item_id":"msg_cli","output_index":0,"content_index":0,"delta":"Hello codex"}),
        json!({"type":"response.output_text.done","sequence_number":4,"item_id":"msg_cli","output_index":0,"content_index":0,"text":"Hello codex"}),
        json!({"type":"response.content_part.done","sequence_number":5,"item_id":"msg_cli","output_index":0,"content_index":0,"part":item["content"][0]}),
        json!({"type":"response.output_item.done","sequence_number":6,"output_index":0,"item":item}),
        json!({"type":"response.completed","sequence_number":7,"response":{"id":id,"object":"response","status":"completed",
            "model":req["model"],"output":[item],"usage":{"input_tokens":100,"output_tokens":50,"total_tokens":150}}}),
    ])
}

fn isolated_command(binary: &str, directory: &std::path::Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .current_dir(directory)
        .kill_on_drop(true);
    // Keep the real home value; use supported CLI flags to ignore configuration/auth.
    for name in ["HOME", "PATH", "TMPDIR", "LANG"] {
        if let Ok(value) = std::env::var(name) {
            command.env(name, value);
        }
    }
    command
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    command
}

async fn completed_output(mut command: Command) -> String {
    let output = tokio::time::timeout(Duration::from_secs(50), command.output())
        .await
        .expect("installed CLI did not finish within 50 seconds")
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "CLI failed: {stdout}\n{stderr}");
    stdout
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep both installed-client smoke scenarios together; they share one fixture contract.
#[ignore = "requires OKAPI_TEST_CLAUDE_BIN and OKAPI_TEST_CODEX_BIN; invokes installed CLIs against local mock only"]
async fn installed_claude_code_and_codex_complete_through_subscription_adapters() {
    for provider in ["anthropic_max", "codex"] {
        let env = setup().await;
        env.mock_state.cli_mode.store(true, Ordering::SeqCst);
        let (channel, key) = login_channel(&env, provider).await;
        env.state
            .ledger
            .credit(env.user_id, Money::from_micros(990_000_000))
            .await
            .unwrap();
        let directory = std::env::temp_dir().join(format!("okapi-cli-smoke-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let output = if provider == "anthropic_max" {
            let binary =
                std::env::var("OKAPI_TEST_CLAUDE_BIN").expect("set installed Claude Code path");
            let mut command = isolated_command(&binary, &directory);
            command
                .args([
                    "--bare",
                    "--restricted",
                    "--strict-mcp-config",
                    "--mcp-config",
                    "{\"mcpServers\":{}}",
                    "--setting-sources",
                    "",
                    "--tools",
                    "",
                    "--no-session-persistence",
                    "--print",
                    "--output-format",
                    "json",
                    "--model",
                    &env.model,
                    "Reply with a greeting without using tools.",
                ])
                .env("ANTHROPIC_BASE_URL", format!("http://{}", env.gateway))
                .env("ANTHROPIC_API_KEY", &env.user_token);
            completed_output(command).await
        } else {
            let binary = std::env::var("OKAPI_TEST_CODEX_BIN").expect("set installed Codex path");
            let mut command = isolated_command(&binary, &directory);
            command
                .args([
                    "exec",
                    "--ignore-user-config",
                    "--ignore-rules",
                    "--ephemeral",
                    "--skip-git-repo-check",
                    "--json",
                    "--sandbox",
                    "read-only",
                    "--model",
                    &env.model,
                ])
                .env("OKAPI_CLI_TEST_KEY", &env.user_token);
            for setting in [
                "model_provider=\"okapi_test\"".to_owned(),
                "model_providers.okapi_test.name=\"Okapi isolated fixture\"".into(),
                format!(
                    "model_providers.okapi_test.base_url=\"http://{}/v1\"",
                    env.gateway
                ),
                "model_providers.okapi_test.env_key=\"OKAPI_CLI_TEST_KEY\"".into(),
                "model_providers.okapi_test.wire_api=\"responses\"".into(),
                "model_providers.okapi_test.requires_openai_auth=false".into(),
                "model_providers.okapi_test.supports_websockets=false".into(),
                "model_providers.okapi_test.request_max_retries=0".into(),
                "model_providers.okapi_test.stream_max_retries=0".into(),
                "approval_policy=\"never\"".into(),
                "web_search=\"disabled\"".into(),
                "project_doc_max_bytes=0".into(),
                "features.apps=false".into(),
            ] {
                command.args(["-c", &setting]);
            }
            command.arg("Reply with a greeting without using tools.");
            completed_output(command).await
        };
        std::fs::remove_dir_all(directory).unwrap();
        let greeting = if provider == "anthropic_max" {
            "Hello max"
        } else {
            "Hello codex"
        };
        assert!(
            output.contains(greeting),
            "missing final greeting: {output}"
        );
        assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 1);
        assert!(
            env.mock_state
                .seen("authorization")
                .unwrap()
                .starts_with("Bearer access-")
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let count: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1 AND channel_id=$2 AND channel_key_id=$3 AND status=20")
                    .bind(env.user_id).bind(channel).bind(key).fetch_one(&env.pg).await.unwrap();
                if count > 0 { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await.expect("CLI reply must settle against the selected account");
    }
}
