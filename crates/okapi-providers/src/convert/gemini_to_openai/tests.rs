use super::*;
use crate::types::ChatEvent;

fn req(v: &Value) -> Value {
    let out = request_gemini_to_openai(&Bytes::from(v.to_string()), "gpt-x", false).unwrap();
    serde_json::from_slice(&out).unwrap()
}

#[test]
fn code_execution_parts_are_rejected_instead_of_dropped() {
    let build = |contents: Value| {
        request_gemini_to_openai(
            &Bytes::from(json!({"contents": contents}).to_string()),
            "gpt-x",
            false,
        )
    };
    let rejected = |contents: Value| match build(contents) {
        Err(UpstreamError::Build(reason)) => reason,
        other => panic!("应拒收，实际 {other:?}"),
    };
    assert!(
        rejected(json!([{"role": "model", "parts": [
            {"executableCode": {"language": "PYTHON", "code": "print(1)"}}]}]))
        .ends_with("part.executableCode")
    );
    assert!(
        rejected(json!([{"role": "user", "parts": [
            {"codeExecutionResult": {"outcome": "OUTCOME_OK", "output": "1"}}]}]))
        .ends_with("part.codeExecutionResult")
    );
    // 只带思维签名的空 part 不是内容，照常跳过
    assert!(
        build(json!([
            {"role": "user", "parts": [{"text": "hi"}]},
            {"role": "model", "parts": [{"thoughtSignature": "sig"}, {"text": "ok"}]}
        ]))
        .is_ok()
    );
}

#[test]
fn maps_system_contents_and_config() {
    let v = req(&json!({
        "systemInstruction": {"parts": [{"text": "be brief"}]},
        "contents": [
            {"role": "user", "parts": [{"text": "hi"}]},
            {"role": "model", "parts": [{"text": "hello"}, {"text": "hidden", "thought": true}]},
            {"role": "user", "parts": [{"text": "look"},
                {"inlineData": {"mimeType": "image/png", "data": "AAAA"}}]}
        ],
        "generationConfig": {"maxOutputTokens": 64, "temperature": 0.2, "topP": 0.9,
            "stopSequences": ["END"], "responseMimeType": "application/json",
            "thinkingConfig": {"thinkingBudget": 2048}}
    }));
    assert_eq!(v["model"], "gpt-x");
    assert_eq!(
        v["messages"][0],
        json!({"role": "system", "content": "be brief"})
    );
    assert_eq!(v["messages"][1], json!({"role": "user", "content": "hi"}));
    assert_eq!(
        v["messages"][2],
        json!({"role": "assistant", "content": "hello"}),
        "thought 部件不回灌"
    );
    assert_eq!(v["messages"][3]["content"][1]["type"], "image_url");
    assert_eq!(
        v["messages"][3]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
    assert_eq!(v["max_tokens"], 64);
    assert_eq!(v["temperature"], 0.2);
    assert_eq!(v["top_p"], 0.9);
    assert_eq!(v["stop"], json!(["END"]));
    assert_eq!(v["response_format"], json!({"type": "json_object"}));
    assert!(v["reasoning_effort"].is_string(), "thinkingBudget 折成档位");
    assert!(v.get("stream").is_none());
}

#[test]
fn maps_tools_and_function_round_trip() {
    let v = req(&json!({
        "contents": [
            {"role": "user", "parts": [{"text": "weather?"}]},
            {"role": "model", "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "SF"}}}]},
            {"role": "user", "parts": [{"functionResponse": {"name": "get_weather",
                "response": {"content": "sunny"}}}]}
        ],
        "tools": [{"functionDeclarations": [{"name": "get_weather", "description": "d",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}]}],
        "toolConfig": {"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["get_weather"]}}
    }));
    assert_eq!(v["messages"][1]["role"], "assistant");
    assert_eq!(v["messages"][1]["content"], Value::Null);
    assert_eq!(v["messages"][1]["tool_calls"][0]["id"], "call_0");
    assert_eq!(
        v["messages"][1]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(
        v["messages"][1]["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"SF"}"#
    );
    assert_eq!(
        v["messages"][2],
        json!({"role": "tool", "tool_call_id": "call_0", "content": "sunny"}),
        "functionResponse 按名字回指到 call_0"
    );
    assert_eq!(v["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        v["tool_choice"],
        json!({"type": "function", "function": {"name": "get_weather"}})
    );
}

#[test]
fn stream_flag_adds_usage_option() {
    let out = request_gemini_to_openai(
        &Bytes::from(json!({"contents": [{"parts": [{"text": "x"}]}]}).to_string()),
        "m",
        true,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["stream"], true);
    assert_eq!(v["stream_options"], json!({"include_usage": true}));
    assert_eq!(v["messages"][0]["role"], "user", "缺 role 视为 user");
}

#[test]
fn response_maps_to_gemini_shape() {
    let body = json!({
        "id": "chatcmpl-1", "model": "gpt-real",
        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": "ok", "reasoning_content": "hmm",
            "tool_calls": [{"id": "c1", "type": "function",
                "function": {"name": "f", "arguments": "{\"a\":1}"}}]}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 7,
            "prompt_tokens_details": {"cached_tokens": 4},
            "completion_tokens_details": {"reasoning_tokens": 3}}
    });
    let (out, usage) = response_openai_to_gemini(&Bytes::from(body.to_string())).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    let parts = &v["candidates"][0]["content"]["parts"];
    assert_eq!(parts[0], json!({"text": "hmm", "thought": true}));
    assert_eq!(parts[1], json!({"text": "ok"}));
    assert_eq!(
        parts[2],
        json!({"functionCall": {"name": "f", "args": {"a": 1}}})
    );
    assert_eq!(v["candidates"][0]["finishReason"], "STOP");
    assert_eq!(v["candidates"][0]["content"]["role"], "model");
    assert_eq!(v["modelVersion"], "gpt-real");
    assert_eq!(
        v["usageMetadata"],
        json!({"promptTokenCount": 10, "candidatesTokenCount": 4, "totalTokenCount": 17,
               "cachedContentTokenCount": 4, "thoughtsTokenCount": 3})
    );
    let u = usage.unwrap();
    assert_eq!((u.prompt_tokens, u.completion_tokens), (10, 7));
}

#[allow(clippy::unnecessary_wraps)] // 与 step() 的输入类型对齐
fn data(raw: &Value) -> Result<ChatEvent, UpstreamError> {
    Ok(ChatEvent::Data {
        raw: raw.to_string(),
        event: None,
        has_output: true,
        content_chars: 0,
        usage: raw
            .get("usage")
            .and_then(|u| serde_json::from_value(u.clone()).ok()),
    })
}

#[test]
fn stream_emits_text_then_final_chunk_with_calls_and_usage() {
    let mut st = OaiStreamToGemini::new("m");
    let mut out = Vec::new();
    out.extend(st.step(data(&json!({"id": "c1", "model": "gpt-real",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}))));
    out.extend(st.step(data(
        &json!({"choices": [{"index": 0, "delta": {"content": "lo"}}]}),
    )));
    out.extend(st.step(data(&json!({"choices": [{"index": 0, "delta": {"tool_calls": [
        {"index": 0, "id": "c", "type": "function", "function": {"name": "f", "arguments": "{\"a\""}}]}}]}))));
    out.extend(st.step(data(
        &json!({"choices": [{"index": 0, "delta": {"tool_calls": [
        {"index": 0, "function": {"arguments": ":1}"}}]}}]}),
    )));
    out.extend(st.step(data(
        &json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
    )));
    out.extend(st.step(data(&json!({"choices": [],
        "usage": {"prompt_tokens": 5, "completion_tokens": 9}}))));
    out.extend(st.step(Ok(ChatEvent::Done)));

    let datas: Vec<Value> = out
        .iter()
        .filter_map(|e| match e {
            Ok(ChatEvent::Data { raw, .. }) => serde_json::from_str(raw).ok(),
            _ => None,
        })
        .collect();
    assert_eq!(datas.len(), 3, "两段文本 + 一条终局：{datas:?}");
    assert_eq!(
        datas[0]["candidates"][0]["content"]["parts"][0]["text"],
        "Hel"
    );
    assert_eq!(datas[0]["modelVersion"], "gpt-real");
    assert_eq!(datas[0]["responseId"], "c1");
    assert_eq!(
        datas[1]["candidates"][0]["content"]["parts"][0]["text"],
        "lo"
    );
    let last = &datas[2];
    assert_eq!(
        last["candidates"][0]["content"]["parts"][0],
        json!({"functionCall": {"name": "f", "args": {"a": 1}}}),
        "分片参数拼完整后一次给出"
    );
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    assert_eq!(last["usageMetadata"]["promptTokenCount"], 5);
    assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 9);
    assert!(matches!(out.last(), Some(Ok(ChatEvent::Done))));
    let usage_events = out
        .iter()
        .filter(|e| matches!(e, Ok(ChatEvent::Data { usage: Some(_), .. })))
        .count();
    assert_eq!(usage_events, 1, "usage 只随终局 chunk 给结算");
}

#[test]
fn stream_final_chunk_without_output_is_not_output() {
    let mut st = OaiStreamToGemini::new("m");
    let out = st.step(Ok(ChatEvent::Done));
    assert_eq!(out.len(), 2);
    assert!(matches!(
        out[0],
        Ok(ChatEvent::Data {
            has_output: false,
            ..
        })
    ));
}
