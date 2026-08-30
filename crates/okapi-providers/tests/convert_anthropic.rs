//! openai_to_anthropic 转换 parity 用例（skill add-provider §6）：
//! 请求出向 / 非流式回向 / 事件流回向 / usage 与 cache 字段映射。

use bytes::Bytes;
use okapi_providers::ChatEvent;
use okapi_providers::anthropic::AnthropicEvent;
use okapi_providers::convert::openai_to_anthropic::{
    StreamState, request_openai_to_anthropic, response_anthropic_to_openai,
};
use serde_json::{Value, json};

fn convert_req(body: &Value) -> Value {
    let out = request_openai_to_anthropic(
        &Bytes::from(serde_json::to_vec(body).unwrap()),
        "claude-x",
        4096,
    )
    .unwrap();
    serde_json::from_slice(&out).unwrap()
}

#[test]
fn request_extracts_system_and_maps_messages() {
    let out = convert_req(&json!({
        "model": "gpt-alias",
        "stream": true,
        "temperature": 0.5,
        "stop": "END",
        "messages": [
            {"role": "system", "content": "be nice"},
            {"role": "developer", "content": "be brief"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}}
            ]}
        ]
    }));
    assert_eq!(out["model"], "claude-x");
    assert_eq!(out["max_tokens"], 4096, "缺省 max_tokens 必须补上（必填）");
    assert_eq!(out["system"], "be nice\n\nbe brief");
    assert_eq!(out["stream"], true);
    assert_eq!(out["temperature"], 0.5);
    assert_eq!(out["stop_sequences"], json!(["END"]));
    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"][0], json!({"type":"text","text":"hi"}));
    assert_eq!(msgs[1]["role"], "assistant");
    let img = &msgs[2]["content"][1];
    assert_eq!(img["type"], "image");
    assert_eq!(img["source"]["type"], "base64");
    assert_eq!(img["source"]["media_type"], "image/png");
    assert_eq!(img["source"]["data"], "QUJD");
}

#[test]
fn request_maps_tools_and_tool_results() {
    let out = convert_req(&json!({
        "model": "m",
        "max_tokens": 128,
        "tools": [{"type": "function", "function": {
            "name": "get_weather", "description": "d",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
        }}],
        "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
        "messages": [
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"SF\"}"}
            }]},
            {"role": "tool", "tool_call_id": "call_1", "content": "sunny"},
            {"role": "tool", "tool_call_id": "call_2", "content": "warm"}
        ]
    }));
    assert_eq!(out["max_tokens"], 128);
    assert_eq!(out["tools"][0]["name"], "get_weather");
    assert!(out["tools"][0]["input_schema"]["properties"]["city"].is_object());
    assert_eq!(
        out["tool_choice"],
        json!({"type": "tool", "name": "get_weather"})
    );

    let msgs = out["messages"].as_array().unwrap();
    // user / assistant(tool_use) / user(两条 tool_result 合并)
    assert_eq!(msgs.len(), 3);
    let tu = &msgs[1]["content"][0];
    assert_eq!(tu["type"], "tool_use");
    assert_eq!(tu["id"], "call_1");
    assert_eq!(tu["input"], json!({"city": "SF"}));
    let results = msgs[2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2, "连续 tool 消息必须合并为单 user turn");
    assert_eq!(results[0]["type"], "tool_result");
    assert_eq!(results[0]["tool_use_id"], "call_1");
    assert_eq!(results[0]["content"], "sunny");
}

#[test]
fn request_drops_tools_when_choice_none() {
    let out = convert_req(&json!({
        "model": "m",
        "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}],
        "tool_choice": "none",
        "messages": [{"role": "user", "content": "hi"}]
    }));
    assert!(out.get("tools").is_none());
}

#[test]
fn response_maps_text_tools_and_cache_usage() {
    let body = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-x",
        "content": [
            {"type": "thinking", "thinking": "let me think"},
            {"type": "text", "text": "It is sunny."},
            {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SF"}}
        ],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 100, "output_tokens": 20,
                  "cache_read_input_tokens": 800, "cache_creation_input_tokens": 50}
    });
    let (out, usage) =
        response_anthropic_to_openai(&Bytes::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let out: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(out["object"], "chat.completion");
    assert_eq!(out["id"], "msg_1");
    let msg = &out["choices"][0]["message"];
    assert_eq!(msg["content"], "It is sunny.");
    assert_eq!(msg["reasoning_content"], "let me think");
    assert_eq!(msg["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(
        msg["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"SF\"}"
    );
    assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
    // prompt = input + cache_read + cache_creation；cached = cache_read
    assert_eq!(out["usage"]["prompt_tokens"], 950);
    assert_eq!(out["usage"]["completion_tokens"], 20);
    assert_eq!(out["usage"]["prompt_tokens_details"]["cached_tokens"], 800);
    let probe = usage.unwrap();
    assert_eq!(probe.prompt_tokens, 950);
    assert_eq!(probe.prompt_tokens_details.cached_tokens, 800);
}

fn ev(event: &str, data: &Value) -> Result<AnthropicEvent, okapi_providers::UpstreamError> {
    // Result 包装：与 StreamState::step 的入参形状一致（模拟上游流 item）
    #[allow(clippy::unnecessary_wraps)]
    fn wrap(ev: AnthropicEvent) -> Result<AnthropicEvent, okapi_providers::UpstreamError> {
        Ok(ev)
    }
    wrap(AnthropicEvent {
        event: event.to_owned(),
        data: data.to_string(),
    })
}

#[test]
// 完整事件序列 fixture 的线性断言，拆分破坏时序可读性
#[allow(clippy::too_many_lines)]
fn stream_sequence_maps_to_openai_chunks() {
    let mut st = StreamState::new("claude-x");
    let mut all: Vec<ChatEvent> = Vec::new();
    let seq = vec![
        ev(
            "message_start",
            &json!({"message": {"id": "msg_9", "model": "claude-real",
            "usage": {"input_tokens": 10, "cache_read_input_tokens": 90}}}),
        ),
        ev(
            "content_block_start",
            &json!({"index": 0, "content_block": {"type": "text"}}),
        ),
        ev(
            "content_block_delta",
            &json!({"index": 0, "delta": {"type": "text_delta", "text": "Hel"}}),
        ),
        ev(
            "content_block_delta",
            &json!({"index": 0, "delta": {"type": "text_delta", "text": "lo"}}),
        ),
        ev("content_block_stop", &json!({"index": 0})),
        ev(
            "content_block_start",
            &json!({"index": 1, "content_block":
            {"type": "tool_use", "id": "tu_1", "name": "get_weather"}}),
        ),
        ev(
            "content_block_delta",
            &json!({"index": 1, "delta":
            {"type": "input_json_delta", "partial_json": "{\"city\":"}}),
        ),
        ev(
            "content_block_delta",
            &json!({"index": 1, "delta":
            {"type": "input_json_delta", "partial_json": "\"SF\"}"}}),
        ),
        ev(
            "message_delta",
            &json!({"delta": {"stop_reason": "tool_use"},
            "usage": {"output_tokens": 7}}),
        ),
        ev("message_stop", &json!({})),
    ];
    for item in seq {
        for out in st.step(item) {
            all.push(out.unwrap());
        }
    }

    let datas: Vec<Value> = all
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Data { raw, .. } => serde_json::from_str(raw).ok(),
            ChatEvent::Done => None,
        })
        .collect();
    assert!(
        matches!(all.last(), Some(ChatEvent::Done)),
        "必须以 DONE 收尾"
    );

    // 角色 chunk：空 content，不触发首字
    assert_eq!(datas[0]["choices"][0]["delta"]["role"], "assistant");
    assert!(matches!(
        &all[0],
        ChatEvent::Data {
            has_output: false,
            ..
        }
    ));
    assert_eq!(datas[0]["model"], "claude-real", "model 取 message_start");

    // 文本增量
    assert_eq!(datas[1]["choices"][0]["delta"]["content"], "Hel");
    assert!(matches!(
        &all[1],
        ChatEvent::Data {
            has_output: true,
            content_chars: 3,
            ..
        }
    ));

    // 工具流：start 带 id/name，delta 带 arguments 分片
    assert_eq!(
        datas[3]["choices"][0]["delta"]["tool_calls"][0]["id"],
        "tu_1"
    );
    assert_eq!(
        datas[3]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(
        datas[4]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":"
    );

    // finish chunk + usage chunk
    assert_eq!(datas[6]["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        datas[7]["usage"]["prompt_tokens"], 100,
        "10 input + 90 cache_read"
    );
    assert_eq!(datas[7]["usage"]["completion_tokens"], 7);
    assert_eq!(
        datas[7]["usage"]["prompt_tokens_details"]["cached_tokens"],
        90
    );
    let usage_event = all
        .iter()
        .find_map(|e| match e {
            ChatEvent::Data { usage: Some(u), .. } => Some(*u),
            _ => None,
        })
        .expect("必须有携带 usage 的事件");
    assert_eq!(usage_event.prompt_tokens, 100);
    assert_eq!(usage_event.completion_tokens, 7);
}

#[test]
fn stream_thinking_delta_maps_to_reasoning_content() {
    let mut st = StreamState::new("m");
    let outs = st.step(ev(
        "content_block_delta",
        &json!({"index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
    ));
    assert_eq!(outs.len(), 1);
    let ChatEvent::Data {
        raw,
        has_output,
        content_chars,
        ..
    } = outs[0].as_ref().unwrap()
    else {
        panic!("应为 Data 事件");
    };
    assert!(has_output, "thinking 是产出，参与首字判定");
    assert_eq!(*content_chars, 3);
    let v: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(v["choices"][0]["delta"]["reasoning_content"], "hmm");
}

#[test]
fn stream_error_event_maps_to_stream_error() {
    let mut st = StreamState::new("m");
    let outs = st.step(ev(
        "error",
        &json!({"type": "error", "error": {"type": "overloaded_error", "message": "overloaded"}}),
    ));
    assert_eq!(outs.len(), 1);
    assert!(outs[0].is_err());
}
