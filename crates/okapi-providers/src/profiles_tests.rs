use super::*;
use serde_json::json;

fn outbound(mode: &str) -> Outbound {
    Outbound {
        context: RequestContext {
            extensions: json!({"client_profile":{"name":"claude-code","mode":mode}}),
            identity_seed: Some("account-key-42".into()),
            session_scope: None,
            client_headers: vec![("user-agent".into(), "python-sdk/1.0".into())],
        },
        ..Default::default()
    }
}

#[test]
fn absent_extension_preserves_original_bytes_and_channel_headers() {
    let body = Bytes::from_static(br#"{ "unknown":{"x":1}, "messages":[] }"#);
    let out = Outbound {
        extra_headers: vec![("x-custom".into(), "value".into())],
        ..Default::default()
    };
    let prepared = prepare_anthropic(body.clone(), false, false, &out, None).unwrap();
    assert_eq!(prepared.body, body);
    assert_eq!(prepared.outbound.extra_headers, out.extra_headers);
    assert!(
        prepared.headers.is_empty(),
        "prepared.headers should be empty"
    );
}

#[test]
fn extension_support_is_independent_of_authentication_and_rejects_unknown_config() {
    let config = outbound("auto").context.extensions;
    for provider in ["anthropic", "anthropic_max"] {
        assert!(validate_extensions(provider, &config).is_ok());
    }
    for provider in ["openai", "codex", "azure", "bedrock", "vertex"] {
        assert!(validate_extensions(provider, &config).is_err());
    }
    for config in [
        json!({"typo":{}}),
        json!({"client_profile":{"name":"claude-code","mode":"typo"}}),
        json!({"client_profile":{"name":"claude-code","revision":"2.1.282"}}),
        json!({"client_profile":{"name":"native","version":"1"}}),
        json!([]),
    ] {
        assert!(
            validate_extensions("anthropic", &config).is_err(),
            "{config}"
        );
    }
}

/// 只实现最新客户端；为已退役版本保存的配置按最新版本读取，未知版本仍拒绝。
#[test]
fn retired_revisions_read_as_the_latest_client() {
    for revision in ["2.1.258", "2.1.286", "2.1.290"] {
        let config = json!({"client_profile":{"name":"claude-code","revision":revision}});
        assert_eq!(
            client_profile(&config).unwrap(),
            Some(ClientProfile::ClaudeCode {
                mode: ProfileMode::Auto,
                revision: ClaudeCodeRevision::V2_1_290,
                entrypoint: ClaudeCodeEntrypoint::Cli,
                request_class: ClaudeCodeRequestClass::Main,
                native_only: false,
            }),
            "{revision}"
        );
        assert!(validate_extensions("anthropic_max", &config).is_ok());
    }
    let saved = serde_json::to_value(ClaudeCodeRevision::V2_1_290).unwrap();
    assert_eq!(saved, "2.1.290");
}

#[test]
fn auto_protects_native_system_cache_and_thinking_bytes_even_with_an_identity() {
    let mut out = outbound("auto");
    out.context.client_headers = vec![
        (
            "user-agent".into(),
            "claude-cli/2.1.287 (external, cli)".into(),
        ),
        ("anthropic-beta".into(), "native-beta".into()),
    ];
    let body = Bytes::from(serde_json::to_vec(&json!({
        "system":[{"type":"text","text":"native system","cache_control":{"type":"ephemeral","ttl":"1h"}}],
        "metadata":{"user_id":json!({"device_id":"a".repeat(64),"session_id":"12345678-1234-4234-8234-123456789012"}).to_string()},
        "messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"kept","signature":"kept"}]}]
    })).unwrap());
    let prepared = prepare_anthropic(body.clone(), true, false, &out, Some("account")).unwrap();
    assert_eq!(prepared.body, body);
    assert!(
        prepared.headers.is_empty(),
        "prepared.headers should be empty"
    );
    assert!(
        prepared
            .outbound
            .extra_headers
            .contains(&("anthropic-beta".into(), "native-beta".into()))
    );
}

#[test]
fn mimic_shapes_requests_without_oauth_authority_or_duplicate_identity_headers() {
    let mut out = outbound("auto");
    out.extra_headers = vec![
        ("X-Stainless-Lang".into(), "python".into()),
        ("Accept".into(), "text/event-stream".into()),
        ("x-custom".into(), "kept".into()),
    ];
    let body = Bytes::from_static(br#"{"messages":[{"role":"user","content":"hello"}]}"#);
    let prepared = prepare_anthropic(body.clone(), true, false, &out, None).unwrap();
    let value: Value = serde_json::from_slice(&prepared.body).unwrap();
    assert!(
        value["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_version=2.1.290.")
    );
    assert!(
        prepared
            .headers
            .contains(&("x-stainless-lang".into(), "js".into()))
    );
    let beta = prepared
        .outbound
        .extra_headers
        .iter()
        .find(|(key, _)| key == "anthropic-beta")
        .unwrap();
    assert!(!beta.1.contains("oauth-"));
    assert!(
        prepared
            .outbound
            .extra_headers
            .contains(&("x-custom".into(), "kept".into()))
    );
    assert!(
        !prepared
            .outbound
            .extra_headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("x-stainless-lang"))
    );
    assert!(
        !prepared
            .outbound
            .extra_headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("accept"))
    );
    let again = prepare_anthropic(body, true, false, &out, None).unwrap();
    let again: Value = serde_json::from_slice(&again.body).unwrap();
    assert_eq!(value["metadata"], again["metadata"]);
}

#[test]
fn explicit_sessions_separate_equal_openings_and_replace_foreign_identity() {
    let body = Bytes::from_static(br#"{"metadata":{"user_id":"foreign","other":"kept"},"messages":[{"role":"user","content":"hello"}]}"#);
    let mut a = outbound("mimic");
    a.context.client_headers.push((
        "x-claude-code-session-id".into(),
        "12345678-1234-4234-8234-123456789012".into(),
    ));
    let mut b = a.clone();
    b.context.client_headers.last_mut().unwrap().1 = "12345678-1234-4234-8234-123456789013".into();
    let first = prepare_anthropic(body.clone(), false, false, &a, None).unwrap();
    let second = prepare_anthropic(body, false, false, &b, None).unwrap();
    let first_value: Value = serde_json::from_slice(&first.body).unwrap();
    let second_value: Value = serde_json::from_slice(&second.body).unwrap();
    let first_id: Value =
        serde_json::from_str(first_value["metadata"]["user_id"].as_str().unwrap()).unwrap();
    let second_id: Value =
        serde_json::from_str(second_value["metadata"]["user_id"].as_str().unwrap()).unwrap();
    assert_eq!(first_id["device_id"], second_id["device_id"]);
    assert_ne!(first_id["session_id"], second_id["session_id"]);
    assert_eq!(first_value["metadata"]["other"], "kept");
    assert!(first.headers.contains(&(
        "x-claude-code-session-id".into(),
        first_id["session_id"].as_str().unwrap().into()
    )));
    assert_eq!(
        identity::cc_fingerprint(
            br#"{"messages":[{"role":"user","content":"abcd\ud83d\ude00efghijklmnopqrstu"}]}"#,
            "2.1.282"
        ),
        "d17"
    );
}

#[test]
fn counting_and_passthrough_use_the_same_extension_boundary() {
    let body = Bytes::from_static(br#"{"messages":[]}"#);
    let prepared = prepare_anthropic(body.clone(), false, true, &outbound("auto"), None).unwrap();
    assert!(prepared.outbound.extra_headers.iter().any(
        |(key, value)| key == "anthropic-beta" && value.contains(identity::BETA_TOKEN_COUNTING)
    ));
    let prepared =
        prepare_anthropic(body.clone(), false, true, &outbound("passthrough"), None).unwrap();
    assert_eq!(prepared.body, body);
    assert!(
        prepared.headers.is_empty(),
        "prepared.headers should be empty"
    );
    assert!(!is_client_header("authorization"));
    assert!(!is_client_header("x-api-key"));
}

/// Codex CLI 0.160 经网关模式实发的身份头（本地录制）：会话头改成连字符，子代理请求另带两项。
#[test]
fn codex_cli_identity_headers_are_forwarded() {
    for name in [
        "session-id",
        "thread-id",
        "x-openai-subagent",
        "x-codex-parent-thread-id",
        "x-codex-turn-metadata",
        "x-codex-turn-state",
        "x-codex-window-id",
        "x-codex-beta-features",
        "x-client-request-id",
        "originator",
        "user-agent",
    ] {
        assert!(is_client_header(name), "{name}");
    }
}

/// 模拟（含自动）会改写请求的渠道才放大预扣；透传、原生与未配置不变。
#[test]
fn admission_hints_follow_the_profile_that_reshapes_requests() {
    let hints =
        |mode: &str| admission_hints(&json!({"client_profile":{"name":"claude-code","mode":mode}}));
    for mode in ["mimic", "auto"] {
        let hint = hints(mode);
        assert!(hint.prompt_cache_write_1h, "{mode}");
        assert_eq!(hint.prompt_tokens(1000), 1600 + 128, "{mode}");
    }
    assert_eq!(hints("passthrough"), AdmissionHints::default());
    assert_eq!(
        admission_hints(&json!({"client_profile":{"name":"native"}})),
        AdmissionHints::default()
    );
    assert_eq!(admission_hints(&Value::Null), AdmissionHints::default());
    assert_eq!(AdmissionHints::default().prompt_tokens(1000), 1000);
    let merged = AdmissionHints::default().max(hints("mimic"));
    assert_eq!(merged, hints("mimic"));
}

#[test]
fn native_only_is_an_opt_in_claude_code_switch() {
    let on = json!({"client_profile":{"name":"claude-code","mode":"auto","native_only":true}});
    assert!(native_only(&on));
    assert!(validate_extensions("anthropic_max", &on).is_ok());
    for off in [
        json!({"client_profile":{"name":"claude-code","mode":"auto"}}),
        json!({"client_profile":{"name":"native"}}),
        Value::Null,
    ] {
        assert!(!native_only(&off), "{off}");
    }
    // 关着时不落盘，已存配置的形状不变
    let profile = client_profile(&json!({"client_profile":{"name":"claude-code"}}))
        .unwrap()
        .unwrap();
    assert!(
        serde_json::to_value(profile)
            .unwrap()
            .get("native_only")
            .is_none()
    );
}

#[test]
fn native_claude_code_needs_cli_metadata_and_a_cli_marker() {
    let device = "a".repeat(64);
    let session = "12345678-1234-4234-8234-123456789012";
    let user_id = json!({"device_id":device,"session_id":session}).to_string();
    let body = |value: Value| serde_json::to_vec(&value).unwrap();
    let plain = body(json!({"metadata":{"user_id":user_id},"messages":[]}));
    assert!(is_native_claude_code(
        &plain,
        Some("claude-cli/2.1.301 (external, cli)")
    ));
    assert!(!is_native_claude_code(&plain, Some("opencode/1.0")));
    assert!(!is_native_claude_code(&plain, None));
    // UA 被中间网关改写：计费归因块仍能认出真机
    let billed = body(json!({
        "metadata":{"user_id":user_id},
        "system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.301.7bd; cc_entrypoint=cli;"}],
    }));
    assert!(is_native_claude_code(&billed, Some("Go-http-client/2.0")));
    let legacy = format!("user_{device}_account__session_{session}");
    let legacy = body(json!({"metadata":{"user_id":legacy},"system":"plain"}));
    assert!(is_native_claude_code(&legacy, Some("claude-cli/2.1.301")));
    // 只抄 UA 不带 CLI 形状的 metadata 不算
    for forged in [
        body(json!({"metadata":{"user_id":"anything"}})),
        body(
            json!({"system":[{"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."}]}),
        ),
        b"not json".to_vec(),
    ] {
        assert!(!is_native_claude_code(&forged, Some("claude-cli/2.1.301")));
    }
}
