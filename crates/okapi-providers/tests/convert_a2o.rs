//! anthropic_to_openai 转换 parity 用例：请求出向（Anthropic→OpenAI）/
//! 响应回向（OpenAI→Anthropic）/ OpenAI chunk 流 → Anthropic 事件骨架。

use bytes::Bytes;
use okapi_providers::ChatEvent;
use okapi_providers::convert::anthropic_to_openai::{
    OaiStreamToAnthropic, request_anthropic_to_openai, response_openai_to_anthropic,
};
use serde_json::{Value, json};

fn convert_req(body: &Value) -> Value {
    let out = request_anthropic_to_openai(&Bytes::from(serde_json::to_vec(body).unwrap()), "gpt-x")
        .unwrap();
    serde_json::from_slice(&out).unwrap()
}

#[test]
fn request_maps_system_tools_and_stream_options() {
    let out = convert_req(&json!({
        "model": "claude-alias",
        "max_tokens": 321,
        "stream": true,
        "system": "be nice",
        "stop_sequences": ["END"],
        "tools": [{"name": "get_weather", "description": "d",
                   "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "any"},
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SF"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "sunny"},
                {"type": "text", "text": "and now?"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}}
            ]}
        ]
    }));
    assert_eq!(out["model"], "gpt-x");
    assert_eq!(out["max_tokens"], 321);
    assert_eq!(out["stream"], true);
    assert_eq!(
        out["stream_options"],
        json!({"include_usage": true}),
        "流式必须带 include_usage（出口需要终局 usage）"
    );
    assert_eq!(out["stop"], json!(["END"]));
    assert_eq!(out["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(out["tool_choice"], "required");

    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "be nice");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"], "hi");
    // assistant：text + tool_calls
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[2]["content"], "checking");
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "tu_1");
    assert_eq!(
        msgs[2]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"SF\"}"
    );
    // tool_result 拆独立 tool 消息，其余聚合为 user
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "tu_1");
    assert_eq!(msgs[3]["content"], "sunny");
    assert_eq!(msgs[4]["role"], "user");
    let parts = msgs[4]["content"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "and now?");
    assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,QUJD");
}

#[test]
fn response_maps_to_anthropic_message() {
    let body = json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": "gpt-x",
        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": "It is sunny.",
            "reasoning_content": "hmm",
            "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "f", "arguments": "{\"a\":1}"}}]
        }}],
        "usage": {"prompt_tokens": 900, "completion_tokens": 50,
                  "prompt_tokens_details": {"cached_tokens": 800}}
    });
    let (out, usage) =
        response_openai_to_anthropic(&Bytes::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let out: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(out["type"], "message");
    assert_eq!(out["stop_reason"], "tool_use");
    let content = out["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "It is sunny.");
    assert_eq!(content[2]["type"], "tool_use");
    assert_eq!(content[2]["input"], json!({"a": 1}));
    // Anthropic 口径：input 不含缓存
    assert_eq!(out["usage"]["input_tokens"], 100);
    assert_eq!(out["usage"]["cache_read_input_tokens"], 800);
    assert_eq!(out["usage"]["output_tokens"], 50);
    // 计费探针保持 OpenAI 口径
    let probe = usage.unwrap();
    assert_eq!(probe.prompt_tokens, 900);
    assert_eq!(probe.prompt_tokens_details.cached_tokens, 800);
}

fn oai_chunk(delta: &Value, finish: Option<&str>) -> ChatEvent {
    let chunk = json!({"id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-real",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    ChatEvent::Data {
        raw: chunk.to_string(),
        event: None,
        has_output: true,
        content_chars: 0,
        usage: None,
    }
}

fn usage_chunk(prompt: u32, cached: u32, completion: u32) -> ChatEvent {
    let chunk = json!({"id": "chatcmpl-9", "object": "chat.completion.chunk", "model": "gpt-real",
        "choices": [], "usage": {"prompt_tokens": prompt, "completion_tokens": completion,
            "prompt_tokens_details": {"cached_tokens": cached}}});
    ChatEvent::Data {
        raw: chunk.to_string(),
        event: None,
        has_output: false,
        content_chars: 0,
        usage: Some(okapi_api::UsageProbe {
            prompt_tokens: prompt,
            completion_tokens: completion,
            prompt_tokens_details: okapi_api::PromptTokensDetails {
                cached_tokens: cached,
                cache_write_tokens: 0,
            },
            completion_tokens_details: okapi_api::CompletionTokensDetails::default(),
        }),
    }
}

#[test]
fn stream_builds_anthropic_event_skeleton() {
    let mut st = OaiStreamToAnthropic::new("gpt-x");
    let mut names: Vec<String> = Vec::new();
    let mut datas: Vec<Value> = Vec::new();
    let mut final_usage = None;
    let seq = vec![
        oai_chunk(&json!({"role": "assistant", "content": ""}), None),
        oai_chunk(&json!({"content": "Hel"}), None),
        oai_chunk(&json!({"content": "lo"}), None),
        oai_chunk(
            &json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                "function": {"name": "f", "arguments": ""}}]}),
            None,
        ),
        oai_chunk(
            &json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"a\":1}"}}]}),
            None,
        ),
        oai_chunk(&json!({}), Some("tool_calls")),
        usage_chunk(100, 40, 7),
        ChatEvent::Done,
    ];
    let seq: Vec<Result<ChatEvent, okapi_providers::UpstreamError>> =
        seq.into_iter().map(Ok).collect();
    let mut done = false;
    for item in seq {
        for out in st.step(item) {
            match out.unwrap() {
                ChatEvent::Data {
                    raw, event, usage, ..
                } => {
                    names.push(event.expect("anthropic 事件必须有名字"));
                    datas.push(serde_json::from_str(&raw).unwrap());
                    if let Some(u) = usage {
                        final_usage = Some(u);
                    }
                }
                ChatEvent::Done => done = true,
            }
        }
    }
    assert!(done);
    assert_eq!(
        names,
        vec![
            "message_start",
            "content_block_start", // text
            "content_block_delta", // Hel
            "content_block_delta", // lo
            "content_block_stop",  // 切工具块自动关文本块
            "content_block_start", // tool_use
            "content_block_delta", // input_json_delta
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(datas[0]["message"]["id"], "chatcmpl-9");
    assert_eq!(datas[1]["content_block"]["type"], "text");
    assert_eq!(datas[2]["delta"]["text"], "Hel");
    assert_eq!(datas[5]["content_block"]["type"], "tool_use");
    assert_eq!(datas[5]["content_block"]["name"], "f");
    assert_eq!(datas[6]["delta"]["partial_json"], "{\"a\":1}");
    assert_eq!(datas[8]["delta"]["stop_reason"], "tool_use");
    // Anthropic 口径 usage：input 不含缓存
    assert_eq!(datas[8]["usage"]["input_tokens"], 60);
    assert_eq!(datas[8]["usage"]["cache_read_input_tokens"], 40);
    assert_eq!(datas[8]["usage"]["output_tokens"], 7);
    // 计费探针 OpenAI 口径
    let u = final_usage.expect("message_delta 必须携带计费探针");
    assert_eq!(u.prompt_tokens, 100);
    assert_eq!(u.completion_tokens, 7);
}

#[test]
fn stream_empty_then_done_still_emits_skeleton() {
    let mut st = OaiStreamToAnthropic::new("gpt-x");
    let outs = st.step(Ok(ChatEvent::Done));
    let names: Vec<_> = outs
        .iter()
        .filter_map(|o| match o.as_ref().unwrap() {
            ChatEvent::Data { event, .. } => event.clone(),
            ChatEvent::Done => None,
        })
        .collect();
    assert_eq!(
        names,
        vec!["message_start", "message_delta", "message_stop"]
    );
}
