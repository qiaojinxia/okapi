use super::*;
use crate::types::ChatEvent;

#[test]
fn chat_request_maps_system_tools_multimodal_reasoning_and_json_schema() {
    let req = json!({
        "model": "gpt-5",
        "messages": [
            {"role": "system", "content": "Be concise."},
            {"role": "user", "content": [
                {"type": "text", "text": "What is in this image?"},
                {"type": "image_url", "image_url": {"url": "https://example.com/a.png", "detail": "high"}}
            ]},
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "lookup", "arguments": "{\"q\":\"cat\"}"}
            }]},
            {"role": "tool", "tool_call_id": "call_1", "content": "orange tabby"}
        ],
        "max_completion_tokens": 512,
        "temperature": 0.2,
        "reasoning_effort": "high",
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "answer",
                "strict": true,
                "schema": {"type": "object", "properties": {"label": {"type": "string"}}}
            }
        },
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "Search catalog",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
            }
        }],
        "tool_choice": {"type": "function", "function": {"name": "lookup"}},
        "parallel_tool_calls": false,
        "stream": true,
        "stream_options": {"include_usage": true}
    });

    let out = request_chat_to_responses_value(&req, "gpt-5-upstream").unwrap();
    assert_eq!(out["model"], "gpt-5-upstream");
    assert_eq!(out["instructions"], "Be concise.");
    assert_eq!(out["max_output_tokens"], 512);
    assert_eq!(out["temperature"], 0.2);
    assert_eq!(out["reasoning"]["effort"], "high");
    assert_eq!(out["stream"], true);
    assert!(out.get("stream_options").is_none());
    assert_eq!(out["text"]["format"]["type"], "json_schema");
    assert_eq!(out["text"]["format"]["name"], "answer");
    assert_eq!(out["text"]["format"]["strict"], true);
    assert_eq!(out["tools"][0]["type"], "function");
    assert_eq!(out["tools"][0]["name"], "lookup");
    assert_eq!(
        out["tool_choice"],
        json!({"type": "function", "name": "lookup"})
    );
    assert_eq!(out["parallel_tool_calls"], false);

    let input = out["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
    assert_eq!(input[0]["content"][1]["type"], "input_image");
    assert_eq!(
        input[0]["content"][1]["image_url"],
        "https://example.com/a.png"
    );
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_1");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[2]["output"], "orange tabby");
}

#[test]
fn rejects_multi_choice_and_unknown_content_parts() {
    let multi = json!({"model": "x", "n": 2, "messages": [{"role": "user", "content": "hi"}]});
    assert!(matches!(
        request_chat_to_responses_value(&multi, "x"),
        Err(UpstreamError::Build(msg)) if msg == "unsupported_content:n"
    ));

    let bad_part = json!({
        "model": "x",
        "messages": [{"role": "user", "content": [{"type": "video_frame", "url": "x"}]}]
    });
    assert!(matches!(
        request_chat_to_responses_value(&bad_part, "x"),
        Err(UpstreamError::Build(msg)) if msg == "unsupported_content:video_frame"
    ));
}

#[test]
fn non_stream_response_preserves_text_reasoning_tool_calls_and_usage() {
    let body = Bytes::from(
        json!({
            "id": "resp_123",
            "object": "response",
            "created_at": 1_700_000_000,
            "model": "gpt-5",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{"type": "summary_text", "text": "thinking step"}]
                },
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Checking weather."}]
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_weather",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Tokyo\"}"
                }
            ],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 40,
                "total_tokens": 140,
                "input_tokens_details": {"cached_tokens": 60},
                "output_tokens_details": {"reasoning_tokens": 15}
            }
        })
        .to_string(),
    );
    let (chat_bytes, probe) = response_responses_to_chat(&body, "fallback").unwrap();
    let chat: Value = serde_json::from_slice(&chat_bytes).unwrap();
    assert_eq!(chat["id"], "resp_123");
    assert_eq!(chat["object"], "chat.completion");
    assert_eq!(chat["model"], "gpt-5");
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        chat["choices"][0]["message"]["content"],
        "Checking weather."
    );
    assert_eq!(
        chat["choices"][0]["message"]["reasoning_content"],
        "thinking step"
    );
    assert_eq!(
        chat["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_weather"
    );
    assert_eq!(
        chat["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(chat["usage"]["prompt_tokens"], 100);
    assert_eq!(chat["usage"]["completion_tokens"], 40);
    assert_eq!(chat["usage"]["prompt_tokens_details"]["cached_tokens"], 60);
    assert_eq!(
        chat["usage"]["completion_tokens_details"]["reasoning_tokens"],
        15
    );
    let u = probe.unwrap().to_token_usage().unwrap();
    assert_eq!(u.prompt_tokens, 100);
    assert_eq!(u.completion_tokens, 40);
    assert_eq!(u.cached_tokens, 60);
    assert_eq!(u.reasoning_tokens, 15);
}

#[test]
fn stream_conversion_emits_text_reasoning_tool_calls_usage_and_single_done() {
    let raw_events = [
        (
            "response.created",
            json!({"type": "response.created", "response": {"id": "resp_s1", "created_at": 1_700_000_001, "model": "gpt-5"}}),
        ),
        (
            "response.reasoning_summary_text.delta",
            json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "delta": "plan"}),
        ),
        (
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "delta": "Hello "}),
        ),
        (
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "delta": "world"}),
        ),
        (
            "response.output_item.added",
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "ping", "arguments": ""}}),
        ),
        (
            "response.function_call_arguments.delta",
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 2, "delta": "{\"a\":1}"}),
        ),
        (
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_s1",
                    "created_at": 1_700_000_001,
                    "model": "gpt-5",
                    "status": "completed",
                    "usage": {"input_tokens": 12, "output_tokens": 8, "total_tokens": 20}
                }
            }),
        ),
    ];

    let mut conv = ResponsesStreamToChat::new("fallback");
    let mut emitted = Vec::new();
    for (name, payload) in raw_events {
        for ev in crate::responses::parse_event(name, &payload.to_string()) {
            emitted.extend(conv.step(Ok(ev)));
        }
    }

    let done_count = emitted
        .iter()
        .filter(|e| matches!(e, Ok(ChatEvent::Done)))
        .count();
    assert_eq!(
        done_count, 1,
        "completed + trailing parse_event Done should emit exactly one Done"
    );

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_args = String::new();
    let mut finish_reason = None;
    let mut seen_usage = None;
    for item in emitted {
        if let Ok(ChatEvent::Data { raw, usage, .. }) = item {
            if usage.is_some() {
                seen_usage = usage;
            }
            let v: Value = serde_json::from_str(&raw).unwrap();
            if let Some(d) = v
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                text.push_str(d);
            }
            if let Some(r) = v
                .pointer("/choices/0/delta/reasoning_content")
                .and_then(Value::as_str)
            {
                reasoning.push_str(r);
            }
            if let Some(a) = v
                .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                .and_then(Value::as_str)
            {
                tool_args.push_str(a);
            }
            if let Some(f) = v
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
            {
                finish_reason = Some(f.to_owned());
            }
        }
    }
    assert_eq!(reasoning, "plan");
    assert_eq!(text, "Hello world");
    assert_eq!(tool_args, "{\"a\":1}");
    assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
    let u = seen_usage.unwrap().to_token_usage().unwrap();
    assert_eq!((u.prompt_tokens, u.completion_tokens), (12, 8));
}

#[test]
fn anthropic_messages_composes_cleanly_to_responses_and_back() {
    let anthropic_req = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 256,
        "system": "You are helpful.",
        "messages": [{"role": "user", "content": "Say hi"}],
        "stream": true
    });
    let chat_req = crate::convert::anthropic_to_openai::request_anthropic_to_openai_value(
        &anthropic_req,
        "gpt-5-codex",
    )
    .unwrap();
    let resp_req = request_chat_to_responses_value(&chat_req, "gpt-5-codex").unwrap();
    assert_eq!(resp_req["model"], "gpt-5-codex");
    assert_eq!(resp_req["instructions"], "You are helpful.");
    assert_eq!(resp_req["max_output_tokens"], 256);
    assert_eq!(resp_req["stream"], true);
    assert_eq!(resp_req["input"][0]["content"][0]["text"], "Say hi");

    // Stream back: Responses -> Chat -> Anthropic Messages SSE
    let mut r2c = ResponsesStreamToChat::new("gpt-5-codex");
    let mut c2a = crate::convert::anthropic_to_openai::OaiStreamToAnthropic::new("gpt-5-codex");
    let mut anthropic_events = Vec::new();
    for (name, payload) in [
        (
            "response.created",
            json!({"type": "response.created", "response": {"id": "resp_a1", "created_at": 1_700_000_002, "model": "gpt-5-codex"}}),
        ),
        (
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "item_id": "m1", "delta": "Hi there!"}),
        ),
        (
            "response.completed",
            json!({"type": "response.completed", "response": {"id": "resp_a1", "status": "completed", "usage": {"input_tokens": 5, "output_tokens": 3}}}),
        ),
    ] {
        for ev in crate::responses::parse_event(name, &payload.to_string()) {
            for chat_ev in r2c.step(Ok(ev)) {
                anthropic_events.extend(c2a.step(chat_ev));
            }
        }
    }
    let saw_text = anthropic_events.iter().any(|item| {
        matches!(item, Ok(ChatEvent::Data { raw, event: Some(ev), .. }) if ev == "content_block_delta" && raw.contains("Hi there!"))
    });
    let saw_stop = anthropic_events.iter().any(
        |item| matches!(item, Ok(ChatEvent::Data { event: Some(ev), .. }) if ev == "message_stop"),
    );
    assert!(saw_text, "expected Anthropic content_block_delta");
    assert!(saw_stop, "expected Anthropic message_stop");
}
