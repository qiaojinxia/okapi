//! 失败分类、计量与补全上限的跨阶段单测（原先在 chat.rs 末尾）。
use super::*;

#[test]
fn visible_reasoning_is_counted_once_and_opaque_context_is_not() {
    let chat = Bytes::from(
        serde_json::json!({"choices":[{"message":{
            "content":"OK", "reasoning_content":"思考", "reasoning":"ignored alias"
        }}]})
        .to_string(),
    );
    assert_eq!(non_stream_content_chars(Ingress::OpenAi, &chat), 4);
    let responses = Bytes::from(serde_json::json!({"output":[
        {"type":"reasoning","summary":[{"type":"summary_text","text":"思考"}],"encrypted_content":"opaque"},
        {"type":"message","content":[{"type":"output_text","text":"OK"}]}
    ]}).to_string());
    assert_eq!(non_stream_content_chars(Ingress::Responses, &responses), 4);
    assert_eq!(
        non_stream_content_chars(Ingress::ResponsesCompact, &responses),
        0
    );
}

#[test]
fn tool_arguments_and_every_choice_feed_the_fallback_estimate() {
    let body = |value: serde_json::Value| Bytes::from(value.to_string());
    let chat = body(serde_json::json!({"choices":[
        {"message":{"content":"ab","tool_calls":[{"function":{"name":"f","arguments":"{\"x\":1}"}}]}},
        {"message":{"content":"cd"}}
    ]}));
    assert_eq!(non_stream_content_chars(Ingress::OpenAi, &chat), 2 + 7 + 2);
    let anthropic = body(serde_json::json!({"content":[
        {"type":"text","text":"ab"},
        {"type":"tool_use","name":"f","input":{"x":1}}
    ]}));
    assert_eq!(
        non_stream_content_chars(Ingress::Anthropic, &anthropic),
        2 + 7
    );
    let responses = body(serde_json::json!({"output":[
        {"type":"function_call","name":"f","arguments":"{\"x\":1}"},
        {"type":"custom_tool_call","name":"g","input":"patch"}
    ]}));
    assert_eq!(
        non_stream_content_chars(Ingress::Responses, &responses),
        7 + 5
    );
    let gemini = body(serde_json::json!({"candidates":[
        {"content":{"parts":[{"text":"ab"},{"functionCall":{"name":"f","args":{"x":1}}}]}},
        {"content":{"parts":[{"text":"cd"}]}}
    ]}));
    assert_eq!(
        non_stream_content_chars(Ingress::Gemini, &gemini),
        2 + 7 + 2
    );
}

#[test]
fn explicit_caps_are_reserved_in_full_and_defaults_stay_bounded() {
    assert_eq!(
        admitted_completion_cap(Ingress::OpenAi, Some(100_000), None),
        100_000
    );
    assert_eq!(
        admitted_completion_cap(Ingress::OpenAi, Some(100_000), Some(40_000)),
        40_000
    );
    assert_eq!(
        admitted_completion_cap(Ingress::Anthropic, Some(512), Some(40_000)),
        512
    );
    assert_eq!(
        admitted_completion_cap(Ingress::OpenAi, None, Some(128_000)),
        MAX_COMPLETION_CAP
    );
    assert_eq!(
        admitted_completion_cap(Ingress::OpenAi, None, Some(8_192)),
        8_192
    );
    assert_eq!(
        admitted_completion_cap(Ingress::OpenAi, None, None),
        DEFAULT_COMPLETION_CAP
    );
}

/// 四种入口的图片都计张，工具结果里嵌套的也算；非图片的内联文件不算。
#[test]
fn image_inputs_are_counted_across_ingresses() {
    let count = |ingress, v: serde_json::Value| image_inputs(ingress, &v);
    assert_eq!(
        count(
            Ingress::OpenAi,
            serde_json::json!({"messages":[{"role":"user","content":[
                {"type":"text","text":"hi"},
                {"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}},
                {"type":"image_url","image_url":{"url":"https://x/y.png"}}
            ]}]})
        ),
        2
    );
    assert_eq!(
        count(
            Ingress::Anthropic,
            serde_json::json!({"messages":[{"role":"user","content":[
                {"type":"image","source":{"type":"base64","data":"AA"}},
                {"type":"tool_result","tool_use_id":"t","content":[
                    {"type":"image","source":{"type":"url","url":"https://x"}}
                ]}
            ]}]})
        ),
        2
    );
    assert_eq!(
        count(
            Ingress::Responses,
            serde_json::json!({"input":[{"role":"user","content":[
                {"type":"input_text","text":"hi"},
                {"type":"input_image","image_url":"https://x"}
            ]}]})
        ),
        1
    );
    assert_eq!(
        count(
            Ingress::Gemini,
            serde_json::json!({"contents":[{"parts":[
                {"inlineData":{"mimeType":"image/png","data":"AA"}},
                {"inlineData":{"mimeType":"application/pdf","data":"AA"}},
                {"text":"hi"}
            ]}]})
        ),
        1
    );
    assert_eq!(
        count(Ingress::OpenAi, serde_json::json!({"messages":[]})),
        0
    );
}

/// PDF 按页、音频按字节、纯文本文档按长度计入；URL 引用与未知类型不计。
#[test]
fn attachments_are_bounded_from_inline_data() {
    use base64::Engine as _;
    let b64 = |bytes: &[u8]| base64::prelude::BASE64_STANDARD.encode(bytes);
    let pdf = b64(
        b"%PDF-1.4 1 0 obj <</Type /Pages /Count 2>> 2 0 obj <</Type /Page>> 3 0 obj <</Type/Page>>",
    );
    let count = |ingress, v: serde_json::Value| attachment_tokens(ingress, &v);
    assert_eq!(
        count(
            Ingress::Anthropic,
            serde_json::json!({"messages":[{"role":"user","content":[
                {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":pdf}},
                {"type":"document","source":{"type":"text","media_type":"text/plain","data":"abcdef"}},
                {"type":"document","source":{"type":"url","url":"https://x/a.pdf"}}
            ]}]})
        ),
        2 * PDF_PAGE_TOKENS + 3
    );
    assert_eq!(
        count(
            Ingress::OpenAi,
            serde_json::json!({"messages":[{"role":"user","content":[
                {"type":"file","file":{"filename":"a.pdf","file_data":format!("data:application/pdf;base64,{pdf}")}},
                {"type":"input_audio","input_audio":{"format":"wav","data":b64(&[0_u8; 12_500])}}
            ]}]})
        ),
        2 * PDF_PAGE_TOKENS + 100
    );
    assert_eq!(
        count(
            Ingress::Responses,
            serde_json::json!({"input":[{"role":"user","content":[
                {"type":"input_file","file_data":format!("data:application/pdf;base64,{pdf}")},
                {"type":"input_file","file_id":"file_1"}
            ]}]})
        ),
        2 * PDF_PAGE_TOKENS
    );
    assert_eq!(
        count(
            Ingress::Gemini,
            serde_json::json!({"contents":[{"parts":[
                {"inlineData":{"mimeType":"audio/mp3","data":b64(&[0_u8; 1_250])}},
                {"inlineData":{"mimeType":"image/png","data":"AA"}}
            ]}]})
        ),
        10
    );
}

/// 堆满 `/Type /Page` 的构造文件只按上游单文件页数上限计，预扣不被虚增到无界。
#[test]
fn pdf_page_estimate_is_capped() {
    use base64::Engine as _;
    let pdf = format!("%PDF-1.4 {}", "<</Type /Page>> ".repeat(5_000));
    let data = base64::prelude::BASE64_STANDARD.encode(pdf);
    assert_eq!(
        media_tokens(Some("application/pdf"), &data),
        PDF_MAX_PAGES * PDF_PAGE_TOKENS
    );
}

/// 放大后的估算必须仍是合法用量：计价入口先校验，不合法会让整个请求失败。
#[test]
fn admission_hints_keep_the_estimate_valid_for_pricing() {
    let base = TokenUsage {
        prompt_tokens: 1000,
        completion_tokens: 64,
        ..TokenUsage::default()
    };
    let hinted = with_admission_hints(
        base,
        okapi_providers::profiles::admission_hints(
            &serde_json::json!({"client_profile":{"name":"claude-code","mode":"mimic"}}),
        ),
    );
    hinted.validate().unwrap();
    assert_eq!(hinted.prompt_tokens, 1728);
    assert_eq!(hinted.cache_write_1h_tokens, Some(1728));
    let plain = with_admission_hints(base, okapi_providers::profiles::AdmissionHints::default());
    plain.validate().unwrap();
    assert_eq!(plain, base);
}

/// TPM 限额按放大后的 prompt 计，与预扣同一口径；没有放大时就是 prompt + 补全上限。
#[test]
fn rate_limited_tokens_follow_the_hinted_prompt() {
    let base = TokenUsage {
        prompt_tokens: 1000,
        completion_tokens: 64,
        ..TokenUsage::default()
    };
    assert_eq!(reserved_token_count(&base), 1064);
    let hinted = with_admission_hints(
        base,
        okapi_providers::profiles::admission_hints(
            &serde_json::json!({"client_profile":{"name":"claude-code","mode":"mimic"}}),
        ),
    );
    assert_eq!(reserved_token_count(&hinted), 1728 + 64);
}

/// 缺省上限只在模型能输出得比预扣封顶更多时写入；显式上限、未知 max_output 不动。
#[test]
fn omitted_cap_is_bounded_only_when_the_model_could_exceed_the_hold() {
    assert_eq!(
        default_output_cap(None, Some(128_000)),
        Some(MAX_COMPLETION_CAP)
    );
    assert_eq!(default_output_cap(None, Some(8_192)), None);
    assert_eq!(default_output_cap(None, None), None);
    assert_eq!(default_output_cap(Some(100), Some(128_000)), None);

    let cap = Some(MAX_COMPLETION_CAP);
    let json = |ingress, v: serde_json::Value| -> serde_json::Value {
        let out = bound_default_output(ingress, cap, Bytes::from(v.to_string()));
        serde_json::from_slice(&out).unwrap()
    };
    let chat = json(
        Ingress::OpenAi,
        serde_json::json!({"model":"m","messages":[]}),
    );
    assert_eq!(chat["max_tokens"], MAX_COMPLETION_CAP);
    let explicit = json(
        Ingress::OpenAi,
        serde_json::json!({"model":"m","max_completion_tokens":7}),
    );
    assert_eq!(explicit["max_completion_tokens"], 7);
    assert!(explicit.get("max_tokens").is_none());
    let responses = json(Ingress::Responses, serde_json::json!({"model":"m"}));
    assert_eq!(responses["max_output_tokens"], MAX_COMPLETION_CAP);
    let gemini = json(
        Ingress::Gemini,
        serde_json::json!({"contents":[],"generationConfig":{"temperature":0.2}}),
    );
    assert_eq!(
        gemini["generationConfig"]["maxOutputTokens"],
        MAX_COMPLETION_CAP
    );
    assert_eq!(gemini["generationConfig"]["temperature"], 0.2);
    let anthropic = json(Ingress::Anthropic, serde_json::json!({"model":"m"}));
    assert_eq!(anthropic["max_tokens"], MAX_COMPLETION_CAP);
    // 显式 null 与缺席同形：上限照写，客户端不能借 null 绕开封顶
    let nulled = json(
        Ingress::OpenAi,
        serde_json::json!({"model":"m","max_tokens":null}),
    );
    assert_eq!(nulled["max_tokens"], MAX_COMPLETION_CAP);
    let nulled_completion = json(
        Ingress::OpenAi,
        serde_json::json!({"model":"m","max_completion_tokens":null}),
    );
    assert_eq!(nulled_completion["max_tokens"], MAX_COMPLETION_CAP);
    assert!(nulled_completion.get("max_completion_tokens").is_none());
    let nulled_responses = json(
        Ingress::Responses,
        serde_json::json!({"model":"m","max_output_tokens":null}),
    );
    assert_eq!(nulled_responses["max_output_tokens"], MAX_COMPLETION_CAP);
    let nulled_gemini = json(
        Ingress::Gemini,
        serde_json::json!({"contents":[],"generationConfig":{"maxOutputTokens":null}}),
    );
    assert_eq!(
        nulled_gemini["generationConfig"]["maxOutputTokens"],
        MAX_COMPLETION_CAP
    );
    let nulled_anthropic = json(
        Ingress::Anthropic,
        serde_json::json!({"model":"m","max_tokens":null}),
    );
    assert_eq!(nulled_anthropic["max_tokens"], MAX_COMPLETION_CAP);
    let compact = json(Ingress::ResponsesCompact, serde_json::json!({"model":"m"}));
    assert!(compact.get("max_output_tokens").is_none());
    let untouched = Bytes::from_static(b"{\"model\":\"m\"}");
    assert_eq!(
        bound_default_output(Ingress::OpenAi, None, untouched.clone()),
        untouched
    );
}

/// Gemini 的生成配置对象整个是 null、或用 snake_case 拼写时，缺省上限同样要写进请求：
/// 此前 `"generationConfig": null` 让 Gemini 臂取不到对象而原样放行，预扣封顶 32768 而上游照模型
/// max_output 生成，余额被透支。snake_case 对象则把上限写进它，不并排再造一个 camelCase 对象。
#[test]
fn gemini_null_or_snake_case_config_still_gets_the_default_cap() {
    let cap = Some(MAX_COMPLETION_CAP);
    let json = |v: serde_json::Value| -> serde_json::Value {
        let out = bound_default_output(Ingress::Gemini, cap, Bytes::from(v.to_string()));
        serde_json::from_slice(&out).unwrap()
    };
    // 整个配置对象是 null 同样视同缺席（此前原样放行，上限写不进请求）
    let null_config = json(serde_json::json!({"contents":[],"generationConfig":null}));
    assert_eq!(
        null_config["generationConfig"]["maxOutputTokens"],
        MAX_COMPLETION_CAP
    );
    let null_snake_config = json(serde_json::json!({"contents":[],"generation_config":null}));
    assert_eq!(
        null_snake_config["generationConfig"]["maxOutputTokens"],
        MAX_COMPLETION_CAP
    );
    assert!(null_snake_config.get("generation_config").is_none());
    // snake_case 的配置对象：上限写进它，不并排再造一个 camelCase 对象
    let snake = json(
        serde_json::json!({"contents":[],"generation_config":{"temperature":0.2,"max_output_tokens":null}}),
    );
    assert_eq!(
        snake["generation_config"]["maxOutputTokens"],
        MAX_COMPLETION_CAP
    );
    assert!(
        snake["generation_config"]
            .get("max_output_tokens")
            .is_none()
    );
    assert_eq!(snake["generation_config"]["temperature"], 0.2);
    assert!(snake.get("generationConfig").is_none());
}

/// compact 端点的 cap 写不进请求：未声明时按模型 max_output 全额预扣，
/// 不吃 MAX_COMPLETION_CAP 封顶；其余入口维持封顶缺省。
#[test]
fn compact_reserves_full_model_output_when_no_cap_can_be_injected() {
    assert_eq!(
        admitted_completion_cap(Ingress::ResponsesCompact, None, Some(128_000)),
        128_000
    );
    assert_eq!(
        admitted_completion_cap(Ingress::OpenAi, None, Some(128_000)),
        MAX_COMPLETION_CAP
    );
    assert_eq!(
        admitted_completion_cap(Ingress::ResponsesCompact, None, None),
        MAX_COMPLETION_CAP
    );
    // 显式声明的上限照单全收，两种入口一致
    assert_eq!(
        admitted_completion_cap(Ingress::ResponsesCompact, Some(100), Some(128_000)),
        100
    );
    assert_eq!(
        admitted_completion_cap(Ingress::OpenAi, Some(100), Some(128_000)),
        100
    );
}

#[test]
fn responses_error_sequence_follows_forwarded_events() {
    let event = |sequence| ChatEvent::Data {
        raw: serde_json::json!({"type":"response.output_text.delta","sequence_number":sequence})
            .to_string(),
        event: Some("response.output_text.delta".into()),
        has_output: true,
        content_chars: 1,
        usage: None,
    };
    let mut next = 0;
    advance_stream_sequence(&event(7), Ingress::Responses, &mut next);
    assert_eq!(next, 8);
    advance_stream_sequence(&event(4), Ingress::Responses, &mut next);
    assert_eq!(next, 8);
    advance_stream_sequence(&event(9), Ingress::OpenAi, &mut next);
    assert_eq!(next, 8);
    advance_stream_sequence(&ChatEvent::Done, Ingress::Responses, &mut next);
    assert_eq!(next, 8);
}

fn status(status: u16, body: &str) -> UpstreamError {
    UpstreamError::Status {
        status,
        body: Bytes::copy_from_slice(body.as_bytes()),
        retry_after_secs: None,
    }
}
#[test]
fn request_errors_and_resource_permissions_do_not_change_account_health() {
    for error in [
        UpstreamError::Connect("dns".into()),
        UpstreamError::Timeout,
        UpstreamError::Stream("transport".into()),
        status(403, r#"{"error":{"code":"access_denied"}}"#),
        status(408, ""),
    ] {
        assert!(matches!(failure_kind_of(&error), KeyFailure::Request));
    }
    assert!(matches!(
        failure_kind_of(&status(401, "")),
        KeyFailure::Invalid
    ));
    assert!(matches!(
        failure_kind_of(&status(403, r#"{"error":{"code":"invalid_api_key"}}"#)),
        KeyFailure::Invalid
    ));
    assert!(matches!(
        failure_kind_of(&status(429, r#"{"error":{"code":"insufficient_quota"}}"#)),
        KeyFailure::QuotaExhausted
    ));
    assert!(matches!(
        failure_kind_of(&status(529, "")),
        KeyFailure::RateLimited {
            retry_after_secs: Some(600)
        }
    ));
    assert!(matches!(
        failure_kind_of(&status(503, "")),
        KeyFailure::Transient
    ));
}

/// Google 系的 429 把等待时间放在正文的 `RetryInfo.retryDelay`：key 的冷却按它走，不再落到缺省 60 秒。
#[test]
fn google_retry_info_sets_the_key_cooldown() {
    let body = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"34s"}]}}"#;
    assert!(matches!(
        failure_kind_of(&status(429, body)),
        KeyFailure::RateLimited {
            retry_after_secs: Some(34)
        }
    ));
}

#[test]
fn account_level_errors_inside_request_statuses_register_on_the_key_and_fail_over() {
    let credit = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#;
    let org = r#"{"type":"error","error":{"type":"invalid_request_error","message":"This organization has been disabled."}}"#;
    let plain =
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages: bad"}}"#;
    assert!(matches!(
        failure_kind_of(&status(400, credit)),
        KeyFailure::QuotaExhausted
    ));
    assert!(matches!(
        failure_kind_of(&status(400, org)),
        KeyFailure::Invalid
    ));
    // 403 没带凭证关键词，但正文说了组织停用：不再按「资源级拒绝」放过
    assert!(matches!(
        failure_kind_of(&status(403, org)),
        KeyFailure::Invalid
    ));
    assert!(matches!(
        failure_kind_of(&status(400, plain)),
        KeyFailure::Request
    ));

    // 入口分类：被包进 400 的账号级错误是可换渠道的失败；同状态的普通请求错误仍原样回客户端
    match classify_fatal(status(400, credit), 0, (1, 2)) {
        AttemptError::Retriable {
            code,
            upstream_status,
            failure_kind,
        } => {
            assert_eq!(code, "upstream_status");
            assert_eq!(upstream_status, Some(400));
            assert!(matches!(failure_kind, KeyFailure::QuotaExhausted));
        }
        AttemptError::Fatal(_) => panic!("账号级错误应换渠道"),
    }
    assert!(matches!(
        classify_fatal(status(400, plain), 0, (1, 2)),
        AttemptError::Fatal(_)
    ));
}
