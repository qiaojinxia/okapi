//! openai_to_gemini 转换 parity 用例：请求出向 / 响应回向 / chunk 流回向 / usage 映射。

use bytes::Bytes;
use okapi_providers::ChatEvent;
use okapi_providers::convert::openai_to_gemini::{
    GeminiStreamState, request_openai_to_gemini, response_gemini_to_openai,
};
use serde_json::{Value, json};

fn convert_req(body: &Value) -> Value {
    let out = request_openai_to_gemini(&Bytes::from(serde_json::to_vec(body).unwrap())).unwrap();
    serde_json::from_slice(&out).unwrap()
}

#[test]
fn request_maps_contents_config_and_tools() {
    let out = convert_req(&json!({
        "model": "g-alias",
        "max_tokens": 256,
        "temperature": 0.7,
        "stop": ["END"],
        "tools": [{"type": "function", "function": {
            "name": "get_weather", "description": "d",
            "parameters": {"type": "object"}}}],
        "tool_choice": "required",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"SF\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "sunny"},
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}}
            ]}
        ]
    }));
    assert_eq!(out["systemInstruction"]["parts"][0]["text"], "be nice");
    assert_eq!(out["generationConfig"]["maxOutputTokens"], 256);
    assert_eq!(out["generationConfig"]["temperature"], 0.7);
    assert_eq!(out["generationConfig"]["stopSequences"], json!(["END"]));
    assert_eq!(
        out["tools"][0]["functionDeclarations"][0]["name"],
        "get_weather"
    );
    assert_eq!(out["toolConfig"]["functionCallingConfig"]["mode"], "ANY");

    let contents = out["contents"].as_array().unwrap();
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[0]["parts"][0]["text"], "hi");
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(
        contents[1]["parts"][0]["functionCall"],
        json!({"name": "get_weather", "args": {"city": "SF"}})
    );
    // tool 消息 → functionResponse（带函数名）并入 user turn；与后续 user 合并
    assert_eq!(contents[2]["role"], "user");
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["name"],
        "get_weather"
    );
    assert_eq!(contents[2]["parts"][1]["text"], "look");
    assert_eq!(
        contents[2]["parts"][2]["inlineData"],
        json!({"mimeType": "image/png", "data": "QUJD"})
    );
}

#[test]
fn response_maps_thought_tools_and_usage() {
    let body = json!({
        "responseId": "r-1",
        "candidates": [{"content": {"role": "model", "parts": [
            {"text": "let me think", "thought": true},
            {"text": "It is sunny."},
            {"functionCall": {"name": "get_weather", "args": {"city": "SF"}}}
        ]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 900, "candidatesTokenCount": 40,
            "cachedContentTokenCount": 800, "thoughtsTokenCount": 10}
    });
    let (out, usage) =
        response_gemini_to_openai(&Bytes::from(serde_json::to_vec(&body).unwrap()), "g-x").unwrap();
    let out: Value = serde_json::from_slice(&out).unwrap();
    let msg = &out["choices"][0]["message"];
    assert_eq!(msg["content"], "It is sunny.");
    assert_eq!(msg["reasoning_content"], "let me think");
    assert_eq!(msg["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(
        out["choices"][0]["finish_reason"], "tool_calls",
        "有 functionCall 时 finish_reason 必须是 tool_calls"
    );
    // promptTokenCount 已含缓存；completion = candidates + thoughts
    assert_eq!(out["usage"]["prompt_tokens"], 900);
    assert_eq!(out["usage"]["completion_tokens"], 50);
    assert_eq!(out["usage"]["prompt_tokens_details"]["cached_tokens"], 800);
    let probe = usage.unwrap();
    assert_eq!(probe.prompt_tokens, 900);
    assert_eq!(probe.completion_tokens, 50);
    assert_eq!(probe.completion_tokens_details.reasoning_tokens, 10);
}

#[test]
fn stream_chunks_map_and_finish_emits_usage_and_done() {
    let mut st = GeminiStreamState::new("g-x");
    let mut all: Vec<ChatEvent> = Vec::new();
    let seq = vec![
        json!({"candidates": [{"content": {"parts": [{"text": "Hel"}]}}]}).to_string(),
        json!({"candidates": [{"content": {"parts": [{"text": "lo"}]}}]}).to_string(),
        json!({"candidates": [{"content": {"parts": [{"text": "!"}]},
            "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 20}})
        .to_string(),
    ];
    for item in seq {
        for out in st.step(Ok(item)) {
            all.push(out.unwrap());
        }
    }
    assert!(
        matches!(all.last(), Some(ChatEvent::Done)),
        "必须以 Done 收尾"
    );
    let datas: Vec<Value> = all
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Data { raw, .. } => serde_json::from_str(raw).ok(),
            ChatEvent::Done => None,
        })
        .collect();
    let content: String = datas
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(content, "Hello!");
    let finish = datas
        .iter()
        .find_map(|c| c["choices"][0]["finish_reason"].as_str());
    assert_eq!(finish, Some("stop"));
    let usage = all
        .iter()
        .find_map(|e| match e {
            ChatEvent::Data { usage: Some(u), .. } => Some(*u),
            _ => None,
        })
        .expect("终局必须携带 usage");
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.completion_tokens, 20);
}

#[test]
fn stream_error_payload_maps_to_stream_error() {
    let mut st = GeminiStreamState::new("g-x");
    let outs = st.step(Ok(
        json!({"error": {"code": 500, "message": "boom"}}).to_string()
    ));
    assert_eq!(outs.len(), 1);
    assert!(outs[0].is_err());
}
