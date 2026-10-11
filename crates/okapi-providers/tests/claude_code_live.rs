//! Explicit opt-in smoke. Reads only an access token, never refreshes credentials,
//! executes only a synthetic arithmetic tool and never opens a user's client session.
use bytes::Bytes;
use futures::StreamExt;
use okapi_providers::{
    HttpPool, Outbound, anthropic::MessagesResponse, oauth::anthropic_max, profiles::RequestContext,
};
use serde_json::{Value, json};

struct Reply {
    content: Vec<Value>,
    stop: String,
}

async fn call(body: Value, out: &Outbound, account: &str, token: &str) -> Reply {
    let wire = serde_json::to_vec(&body).unwrap();
    assert!(
        wire.is_ascii(),
        "real upstream smoke requests must contain only ASCII"
    );
    tokio::time::timeout(std::time::Duration::from_mins(1), async {
        let response = anthropic_max::messages(
            &HttpPool::new().unwrap(),
            "https://api.anthropic.com/v1",
            token,
            Bytes::from(wire),
            true,
            out,
            Some(account),
        )
        .await
        .unwrap_or_else(|error| panic!("real upstream request failed: {error}"));
        let MessagesResponse::Stream(mut stream) = response else {
            panic!("expected streaming response");
        };
        let mut content: Vec<Value> = vec![];
        let mut arguments: Vec<String> = vec![];
        let mut stop = String::new();
        let mut complete = false;
        while let Some(event) = stream.events.next().await {
            {
                let raw = event.unwrap().data;
                let value: Value = serde_json::from_str(&raw).unwrap();
                let index = usize::try_from(value["index"].as_u64().unwrap_or(0)).unwrap();
                match value["type"].as_str().unwrap_or_default() {
                    "content_block_start" => {
                        assert_eq!(index, content.len());
                        content.push(value["content_block"].clone());
                        arguments.push(String::new());
                    }
                    "content_block_delta" => {
                        let delta = &value["delta"];
                        if delta["type"] == "input_json_delta" {
                            arguments[index].push_str(delta["partial_json"].as_str().unwrap());
                        } else {
                            for key in ["text", "thinking", "signature"] {
                                if let Some(text) = delta[key].as_str() {
                                    let old = content[index][key].as_str().unwrap_or_default();
                                    content[index][key] = json!(format!("{old}{text}"));
                                }
                            }
                        }
                    }
                    "content_block_stop" if !arguments[index].is_empty() => {
                        content[index]["input"] = serde_json::from_str(&arguments[index]).unwrap();
                    }
                    "message_delta" => {
                        stop = value["delta"]["stop_reason"]
                            .as_str()
                            .unwrap_or_default()
                            .into();
                    }
                    "message_stop" => complete = true,
                    "error" => panic!("real upstream SSE error"),
                    _ => {}
                }
            }
        }
        assert!(complete, "must receive message_stop");
        Reply { content, stop }
    })
    .await
    .expect("bounded real upstream smoke")
}

fn setup(entry: &str, class: &str) -> (Outbound, String, String) {
    let token =
        std::env::var("OKAPI_TEST_CLAUDE_ACCESS_TOKEN").expect("explicit access token required");
    let account = std::env::var("OKAPI_TEST_CLAUDE_ACCOUNT_UUID").unwrap_or_default();
    let out = Outbound {
        context: RequestContext {
            extensions: json!({"client_profile":{"name":"claude-code","revision":"2.1.290","mode":"mimic","entrypoint":entry,"request_class":class}}),
            identity_seed: Some("live-smoke".into()),
            session_scope: None,
            client_headers: vec![
                (
                    "x-claude-code-session-id".into(),
                    "12345678-1234-4234-8234-123456789012".into(),
                ),
                (
                    "x-claude-code-prompt-id".into(),
                    "23456789-1234-4234-8234-123456789012".into(),
                ),
            ],
        },
        ..Default::default()
    };
    (out, account, token)
}

#[tokio::test]
#[ignore = "real subscription smoke; requires explicitly supplied access token; at most two requests"]
async fn cli_real_tool_result_round_trip_preserves_upstream_thinking_blocks() {
    let (out, account, token) = setup("cli", "main");
    let tools = json!([{"name":"add","description":"Add two integers. This is a local synthetic test tool.",
        "input_schema":{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"integer"}},"required":["a","b"]}}]);
    let opening = json!({"role":"user","content":"Call add exactly once with a=2 and b=3. After its result, reply exactly 5. Do not calculate without calling the tool."});
    let first = call(
        json!({"model":"claude-sonnet-5-5","max_tokens":1024,"tools":tools,
        "messages":[opening]}),
        &out,
        &account,
        &token,
    )
    .await;
    assert_eq!(first.stop, "tool_use");
    let tool = first
        .content
        .iter()
        .find(|block| block["type"] == "tool_use")
        .expect("tool request");
    assert_eq!(tool["name"], "add");
    assert_eq!(tool["input"], json!({"a":2,"b":3}));
    let result = json!({"role":"user","content":[{"type":"tool_result","tool_use_id":tool["id"],"content":"5"}]});
    let second = call(
        json!({"model":"claude-sonnet-5-5","max_tokens":1024,"tools":tools,
        "messages":[opening,{"role":"assistant","content":first.content},result]}),
        &out,
        &account,
        &token,
    )
    .await;
    assert_eq!(second.stop, "end_turn");
    let text: String = second
        .content
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    assert_eq!(text.trim(), "5");
}

#[tokio::test]
#[ignore = "real subscription smoke; requires explicitly supplied access token; one request"]
async fn sdk_real_text_stream_completes() {
    let (out, account, token) = setup("sdk-cli", "main");
    let reply = call(
        json!({"model":"claude-sonnet-5-5","max_tokens":512,
        "messages":[{"role":"user","content":"Reply exactly OK."}]}),
        &out,
        &account,
        &token,
    )
    .await;
    assert_eq!(reply.stop, "end_turn");
    let text: String = reply
        .content
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    // This smoke verifies the wire profile and a complete text stream. The
    // model may add terminal punctuation even to an "exactly OK" instruction.
    assert!(
        matches!(text.trim(), "OK" | "OK."),
        "unexpected smoke reply"
    );
}

#[tokio::test]
#[ignore = "real subscription smoke; requires explicitly supplied access token; one request"]
async fn cli_auxiliary_haiku_request_completes() {
    let (out, account, token) = setup("cli", "auxiliary");
    let reply = call(
        json!({"model":"claude-haiku-4-5-20251001","max_tokens":64,
        "messages":[{"role":"user","content":"Name this session in two words: adding numbers."}]}),
        &out,
        &account,
        &token,
    )
    .await;
    assert_eq!(reply.stop, "end_turn");
    let text: String = reply
        .content
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    assert!(!text.trim().is_empty(), "auxiliary reply must contain text");
}
