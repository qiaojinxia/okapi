use super::*;
use crate::profiles::{RequestContext, prepare_anthropic, validate_extensions};

const SESSION: &str = "12345678-1234-4234-8234-123456789012";
const PROMPT: &str = "23456789-1234-4234-8234-123456789012";

fn outbound(entry: &str, class: &str) -> Outbound {
    Outbound {
        context: RequestContext {
            extensions: json!({"client_profile":{"name":"claude-code","mode":"mimic",
                "revision":"2.1.290","entrypoint":entry,"request_class":class}}),
            identity_seed: Some("fixture-key".into()),
            client_headers: vec![
                ("x-claude-code-session-id".into(), SESSION.into()),
                ("x-claude-code-prompt-id".into(), PROMPT.into()),
            ],
        },
        ..Default::default()
    }
}

#[allow(clippy::needless_pass_by_value)] // Fixtures pass owned JSON for a compact test DSL.
fn prepare_value(body: Value, out: &Outbound, counting: bool) -> (PreparedRequest, Value) {
    let prepared = prepare_anthropic(
        Bytes::from(serde_json::to_vec(&body).unwrap()),
        true,
        counting,
        out,
        None,
    )
    .unwrap();
    let value = serde_json::from_slice(&prepared.body).unwrap();
    (prepared, value)
}

fn header_value<'a>(prepared: &'a PreparedRequest, name: &str) -> &'a str {
    &prepared
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .unwrap()
        .1
}

#[test]
fn cli_envelope_matches_the_installed_capture_and_retains_caller_tools() {
    // Non-sensitive values taken from our isolated 2.1.290 capture, not invented SDK versions.
    let tools =
        json!([{"name":"lookup","description":"Find a value","input_schema":{"type":"object"}}]);
    let (prepared, body) = prepare_value(
        json!({"model":"claude-sonnet-5-5",
        "messages":[{"role":"user","content":"Reply exactly OK. Do not use tools."}],
        "system":"Caller policy", "tools":tools, "metadata":{"tenant":"kept"}}),
        &outbound("cli", "main"),
        false,
    );
    for (key, expected) in [
        ("user-agent", "claude-cli/2.1.290 (external, cli)"),
        ("x-stainless-package-version", "0.128.0"),
        ("x-stainless-runtime-version", "v26.3.0"),
        ("x-stainless-os", "MacOS"),
        ("x-stainless-arch", "arm64"),
        ("accept", "application/json"),
        ("x-claude-code-request-class", "main"),
        ("x-claude-code-session-id", SESSION),
        ("x-claude-code-prompt-id", PROMPT),
    ] {
        assert_eq!(header_value(&prepared, key), expected, "{key}");
    }
    assert!(
        !prepared
            .headers
            .iter()
            .any(|(key, _)| key == "x-stainless-helper-method"),
        "真机 2.1.292 抓包：主请求从不携带此头"
    );
    let attribution = body["system"][0]["text"].as_str().unwrap();
    assert!(attribution.contains("cc_version=2.1.290.fe6; cc_entrypoint=cli;"));
    assert!(attribution.contains(&format!(
        "cc_prompt_id={PROMPT}; cc_turn_origin=human; cc_prompt_index=1; cc_turn_index=1;"
    )));
    let cch = attribution
        .split("cch=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(cch.len(), 5);
    assert!(cch.bytes().all(|b| b.is_ascii_hexdigit()));
    let mut resigned = prepared.body.to_vec();
    let marker = format!("cch={cch};");
    let offset = resigned
        .windows(marker.len())
        .position(|s| s == marker.as_bytes())
        .unwrap();
    resigned[offset + 4..offset + 9].copy_from_slice(b"00000");
    cch::sign(&mut resigned).unwrap();
    assert_eq!(
        resigned, prepared.body,
        "final transmitted bytes must carry a valid checksum"
    );
    assert_eq!(
        body["system"][1]["text"],
        crate::oauth::anthropic_max::SYSTEM_PREFIX
    );
    assert_eq!(
        body["system"][2]["cache_control"],
        json!({"type":"ephemeral","ttl":"1h","scope":"global"})
    );
    assert_eq!(body["system"][3]["text"], "Caller policy");
    assert_eq!(body["max_tokens"], 128_000);
    assert_eq!(
        body["thinking"],
        json!({"type":"adaptive","display":"updates"})
    );
    assert_eq!(body["output_config"]["effort"], "medium");
    assert_eq!(body["context_management"]["edits"][0]["keep"], "all");
    assert_eq!(body["diagnostics"], json!({"previous_message_id":null}));
    assert_eq!(body["tools"][0]["input_schema"], tools[0]["input_schema"]);
    assert_eq!(body["metadata"]["tenant"], "kept");
    let metadata: Value =
        serde_json::from_str(body["metadata"]["user_id"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["session_id"], SESSION);
    assert_eq!(cache_markers(&body), 4);
    let beta = &prepared
        .outbound
        .extra_headers
        .iter()
        .find(|(key, _)| key == "anthropic-beta")
        .unwrap()
        .1;
    // Captured CLI main request without the ToolSearch tool, minus the OAuth beta.
    assert_eq!(
        beta,
        "claude-code-20250219,interleaved-thinking-2025-05-14,thinking-token-count-2026-05-13,\
context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,\
per-turn-control-2026-07-01,mid-conversation-tool-changes-2026-07-01,\
mid-conversation-system-clear-at-2026-08-21,effort-2025-11-24,dangerous-tool-use-2026-09-03,\
thinking-binding-controls-2026-08-01,thinking-display-updates-2026-08-18,afk-mode-2026-01-31,\
extended-cache-ttl-2025-04-11,cache-diagnosis-2026-04-07"
    );
    assert!(
        !beta.contains("oauth-"),
        "the credential adapter owns authentication"
    );
}

#[test]
fn helper_method_absent_in_all_request_classes() {
    let body = Bytes::from(
        serde_json::to_vec(&json!({"model":"claude-sonnet-5-5",
        "messages":[{"role":"user","content":"hello"}]}))
        .unwrap(),
    );
    let out = outbound("cli", "main");
    let streaming = prepare_anthropic(body.clone(), true, false, &out, None).unwrap();
    assert!(
        !streaming
            .headers
            .iter()
            .any(|(key, _)| key == "x-stainless-helper-method"),
        "真机抓包：真实 CLI 流式请求也不带此头（仅 TS SDK stream helper 会发）"
    );
    let unary = prepare_anthropic(body.clone(), false, false, &out, None).unwrap();
    assert!(
        !unary
            .headers
            .iter()
            .any(|(key, _)| key == "x-stainless-helper-method")
    );
    let counting = prepare_anthropic(body, false, true, &out, None).unwrap();
    assert!(
        !counting
            .headers
            .iter()
            .any(|(key, _)| key == "x-stainless-helper-method")
    );
}

#[test]
fn sdk_and_auxiliary_use_their_observed_entrypoint_and_feature_sets() {
    let (sdk, body) = prepare_value(
        json!({"model":"claude-sonnet-5-5",
        "messages":[{"role":"user","content":"hello"}]}),
        &outbound("sdk-cli", "main"),
        false,
    );
    assert_eq!(
        header_value(&sdk, "user-agent"),
        "claude-cli/2.1.290 (external, sdk-cli)"
    );
    assert_eq!(body["system"][1]["text"], SDK_IDENTITY);
    assert_eq!(body["thinking"]["display"], "omitted");
    assert!(
        body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_turn_origin=sdk; cc_prompt_index=0; cc_turn_index=1;")
    );
    assert!(
        !sdk.outbound.extra_headers[0]
            .1
            .contains("thinking-display-updates")
    );
    let (aux, body) = prepare_value(
        json!({"model":"claude-haiku-4-5-20251001",
        "messages":[{"role":"user","content":"title"}]}),
        &outbound("cli", "auxiliary"),
        false,
    );
    assert_eq!(
        header_value(&aux, "x-claude-code-request-class"),
        "auxiliary"
    );
    assert_eq!(body["max_tokens"], 32_000);
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(cache_markers(&body), 0);
    assert!(
        !body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_prompt_id=")
    );
    assert!(
        aux.outbound.extra_headers[0]
            .1
            .contains("structured-outputs-2025-12-15")
    );
    assert!(
        !aux.outbound.extra_headers[0]
            .1
            .contains("claude-code-20250219")
    );
}

#[test]
fn caller_parameters_cache_order_and_thinking_signatures_survive() {
    let signed = json!({"type":"thinking","thinking":"reason","signature":"signed-by-upstream"});
    let request = json!({"model":"claude-sonnet-4-6","max_tokens":800,"temperature":0.4,
        "thinking":{"type":"enabled","budget_tokens":2048},
        "tools":[{"name":"lookup","input_schema":{"type":"object"},"cache_control":{"type":"ephemeral","ttl":"1h"}}],
        "system":[{"type":"text","text":"policy","cache_control":{"type":"ephemeral"}}],
        "messages":[{"role":"user","content":"The literal cch=00000; must stay."},{"role":"assistant","content":[signed]}],
        "safeguards":{"caller_context":"kept"},"opaque_extension":{"future":"kept"}});
    let (_, body) = prepare_value(request.clone(), &outbound("cli", "main"), false);
    assert_eq!(body["messages"][1], request["messages"][1]);
    assert_eq!(body["tools"], request["tools"]);
    assert_eq!(body["messages"][0], request["messages"][0]);
    assert_eq!(body["thinking"], request["thinking"]);
    assert_eq!(body["max_tokens"], 800);
    assert_eq!(body["temperature"], 0.4);
    assert_eq!(body["safeguards"], request["safeguards"]);
    assert_eq!(body["opaque_extension"], request["opaque_extension"]);
    assert_eq!(
        cache_markers(&body),
        2,
        "mixed TTL order must not gain new markers"
    );
}

#[test]
fn cache_budget_and_counting_do_not_gain_messages_parameters() {
    let mut blocks = vec![];
    for i in 0..4 {
        blocks.push(json!({"type":"text","text":format!("policy {i}"),"cache_control":{"type":"ephemeral","ttl":"1h"}}));
    }
    let (_, body) = prepare_value(
        json!({"model":"claude-sonnet-5-5","system":blocks,
        "messages":[{"role":"user","content":"hello"}]}),
        &outbound("cli", "main"),
        false,
    );
    assert_eq!(cache_markers(&body), 4);
    let (count, body) = prepare_value(
        json!({"model":"claude-sonnet-5-5",
        "messages":[{"role":"user","content":"hello"}]}),
        &outbound("cli", "main"),
        true,
    );
    for key in [
        "max_tokens",
        "thinking",
        "diagnostics",
        "stream",
        "context_management",
    ] {
        assert!(body.get(key).is_none(), "{key}");
    }
    assert!(
        count.outbound.extra_headers[0]
            .1
            .contains("token-counting-2024-11-01")
    );
    assert_eq!(body["messages"][0]["content"], "hello");
}

#[test]
fn auto_preserves_native_checksum_and_body_bytes() {
    let uid = json!({"device_id":"a".repeat(64),"session_id":SESSION}).to_string();
    let body = Bytes::from(format!(
        r#"{{ "system":[{{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.290.fe6; cch=abcde;"}}], "metadata":{{"user_id":{}}}, "messages":[] }}"#,
        serde_json::to_string(&uid).unwrap()
    ));
    let mut out = outbound("cli", "main");
    out.context.extensions["client_profile"]["mode"] = json!("auto");
    out.context.client_headers.push((
        "user-agent".into(),
        "claude-cli/2.1.290 (external, cli)".into(),
    ));
    let prepared = prepare_anthropic(body.clone(), true, false, &out, None).unwrap();
    assert_eq!(prepared.body, body);
    assert!(prepared.headers.is_empty());
    for (entry, class) in [("sdk-cli", "main"), ("cli", "auxiliary")] {
        let out = outbound(entry, class);
        assert!(validate_extensions("anthropic_max", &out.context.extensions).is_ok());
    }
}

/// 2.1.290 只在启用 ToolSearch 工具时声明 advanced-tool-use；开启后整串与抓包一致。
#[test]
fn tool_search_requests_announce_advanced_tool_use_in_captured_order() {
    let (prepared, _) = prepare_value(
        json!({"model":"claude-sonnet-5-5",
        "messages":[{"role":"user","content":"hello"}],
        "tools":[{"name":"ToolSearch","description":"Search tools","input_schema":{"type":"object"}}]}),
        &outbound("cli", "main"),
        false,
    );
    let beta = &prepared
        .outbound
        .extra_headers
        .iter()
        .find(|(key, _)| key == "anthropic-beta")
        .unwrap()
        .1;
    assert_eq!(
        beta,
        "claude-code-20250219,interleaved-thinking-2025-05-14,thinking-token-count-2026-05-13,\
context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,\
per-turn-control-2026-07-01,mid-conversation-tool-changes-2026-07-01,advanced-tool-use-2025-11-20,\
mid-conversation-system-clear-at-2026-08-21,effort-2025-11-24,dangerous-tool-use-2026-09-03,\
thinking-binding-controls-2026-08-01,thinking-display-updates-2026-08-18,afk-mode-2026-01-31,\
extended-cache-ttl-2025-04-11,cache-diagnosis-2026-04-07"
    );
    let (auxiliary, _) = prepare_value(
        json!({"model":"claude-haiku-4-5-20251001",
        "messages":[{"role":"user","content":"title"}],
        "tools":[{"name":"ToolSearch","description":"Search tools","input_schema":{"type":"object"}}]}),
        &outbound("cli", "auxiliary"),
        false,
    );
    let beta = &auxiliary
        .outbound
        .extra_headers
        .iter()
        .find(|(key, _)| key == "anthropic-beta")
        .unwrap()
        .1;
    assert_eq!(
        beta,
        "interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,thinking-token-count-2026-05-13,\
context-management-2025-06-27,prompt-caching-scope-2026-01-05,structured-outputs-2025-12-15,\
cache-diagnosis-2026-04-07"
    );
}

#[test]
fn tool_result_rounds_retain_session_and_increment_turn_without_new_prompt() {
    let mut out = outbound("cli", "main");
    out.context
        .client_headers
        .retain(|(key, _)| key != "x-claude-code-prompt-id");
    let request = json!({"model":"claude-sonnet-5-5","messages":[
        {"role":"user","content":"calculate"},
        {"role":"assistant","content":[{"type":"tool_use","id":"tool_1","name":"calculate","input":{"x":1}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"tool_1","content":"2"}]}]});
    let (first, body) = prepare_value(request.clone(), &out, false);
    let (second, again) = prepare_value(request.clone(), &out, false);
    assert_eq!(body["metadata"]["user_id"], again["metadata"]["user_id"]);
    assert_ne!(
        header_value(&first, "x-claude-code-prompt-id"),
        header_value(&second, "x-claude-code-prompt-id")
    );
    assert!(
        body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_prompt_index=1; cc_turn_index=2;")
    );
    assert_eq!(body["messages"][1], request["messages"][1]);
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "tool_1");
    assert_eq!(body["messages"][2]["content"][0]["content"], "2");
}
