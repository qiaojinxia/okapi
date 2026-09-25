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
                audio_tokens: 0,
                image_tokens: 0,
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

/// `tool_choice` 全表。Anthropic 有四个取值：auto / any / tool / none。
///
/// 此前 `none` 落进兜底分支被当成 auto：客户端明确要求"不许调工具"，上游拿到的却是工具列表 +
/// 缺省 auto，模型照样可能去调。OpenAI 同样支持 `tool_choice: "none"`，原样映射过去。
#[test]
fn tool_choice_maps_all_four_values() {
    let with_choice = |choice: Value| {
        convert_req(&json!({
            "model": "claude-x", "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
            "tool_choice": choice
        }))["tool_choice"]
            .clone()
    };
    assert_eq!(
        with_choice(json!({"type": "none"})),
        "none",
        "none 不得被当成 auto"
    );
    assert_eq!(with_choice(json!({"type": "any"})), "required");
    assert_eq!(
        with_choice(json!({"type": "tool", "name": "get_weather"})),
        json!({"type": "function", "function": {"name": "get_weather"}})
    );
    // auto 是 OpenAI 的缺省，不带即可
    assert_eq!(with_choice(json!({"type": "auto"})), Value::Null);
}

/// 两跳往返：Anthropic usage → `openai_to_anthropic` 转成 OpenAI 形状 → `anthropic_to_openai`
/// 再转回 Anthropic 形状，四个数必须原样还原，第二跳的计费探针也必须带着缓存写入。
///
/// 场景是网关串网关（Okapi 前面再挂一层 Okapi / 同类网关，上游走 OpenAI 兼容协议）。
/// 此前两处各丢一半：OpenAI 形状的 usage 只带 `cached_tokens`、不带缓存写入；
/// 转回 Anthropic 时 `cache_creation_input_tokens` 又写死为 0。结果是缓存写入被并进
/// `input_tokens`——下游那一跳把它按普通 prompt 计费（计价引擎有单独的 cache_write 轴），
/// Claude Code 按分项单价估算的成本也随之偏低。
#[test]
fn usage_round_trips_through_two_hops() {
    use okapi_providers::convert::openai_to_anthropic::response_anthropic_to_openai;

    let original = json!({
        "input_tokens": 100,
        "cache_read_input_tokens": 800,
        "cache_creation_input_tokens": 50,
        "output_tokens": 20
    });
    let anthropic_msg = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-x",
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": original
    });

    // 第一跳：Anthropic 上游 → OpenAI 形状
    let (openai_body, _) =
        response_anthropic_to_openai(&Bytes::from(serde_json::to_vec(&anthropic_msg).unwrap()))
            .unwrap();
    // 第二跳：OpenAI 形状 → Anthropic 形状（这一跳自己的计费探针也从这里来）
    let (back, probe) = response_openai_to_anthropic(&openai_body).unwrap();
    let back: Value = serde_json::from_slice(&back).unwrap();

    assert_eq!(back["usage"], original, "两跳之后 usage 必须原样还原");
    let probe = probe.unwrap();
    assert_eq!(
        probe.prompt_tokens, 950,
        "prompt = input + cache_read + cache_creation"
    );
    assert_eq!(probe.prompt_tokens_details.cached_tokens, 800);
    assert_eq!(
        probe.prompt_tokens_details.cache_write_tokens, 50,
        "第二跳的计费探针丢了缓存写入，会按普通 prompt 计价"
    );
}

/// 请求侧此前没被钉住的映射（规则级变异 SURVIVED，或只被不相干的用例顺带撞到）：
/// url 形式的图片、temperature / top_p 透传、`input_schema → parameters`、
/// system 块数组以空行拼接、tool_result 多段内容以换行拼接。
#[test]
fn request_maps_url_images_sampling_schema_and_multipart_text() {
    let schema = json!({
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"]
    });
    let out = convert_req(&json!({
        "model": "claude-alias", "max_tokens": 64,
        "temperature": 0.3, "top_p": 0.8,
        "system": [{"type": "text", "text": "rule one"}, {"type": "text", "text": "rule two"}],
        "tools": [{"name": "get_weather", "input_schema": schema}],
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "what is this"},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}}
            ]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SF"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1",
                 "content": [{"type": "text", "text": "line one"}, {"type": "text", "text": "line two"}]}
            ]}
        ]
    }));
    assert_eq!(out["temperature"], 0.3);
    assert_eq!(out["top_p"], 0.8);
    assert_eq!(
        out["tools"][0]["function"]["parameters"], schema,
        "工具参数表丢了，上游模型不知道该传什么"
    );
    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(
        msgs[0],
        json!({"role": "system", "content": "rule one\n\nrule two"})
    );
    assert_eq!(
        msgs[1]["content"][1],
        json!({"type": "image_url", "image_url": {"url": "https://example.com/a.png"}})
    );
    assert_eq!(
        msgs[3],
        json!({"role": "tool", "tool_call_id": "tu_1", "content": "line one\nline two"})
    );
}

/// OpenAI `finish_reason` → Anthropic `stop_reason` 全表。此前只有 `tool_calls` 一项被直接断言；
/// `length → max_tokens`、`content_filter → refusal` 改坏后全量无一变红——
/// Claude Code 靠 `max_tokens` 判断输出被截断、靠 `refusal` 判断拒答。
#[test]
fn response_finish_reason_table() {
    let stop = |finish: Value| {
        let body = json!({
            "id": "c", "model": "gpt-x",
            "choices": [{"index": 0, "finish_reason": finish,
                         "message": {"role": "assistant", "content": "x"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        let (out, _) =
            response_openai_to_anthropic(&Bytes::from(serde_json::to_vec(&body).unwrap())).unwrap();
        serde_json::from_slice::<Value>(&out).unwrap()["stop_reason"].clone()
    };
    assert_eq!(stop(json!("length")), "max_tokens");
    assert_eq!(stop(json!("tool_calls")), "tool_use");
    assert_eq!(stop(json!("content_filter")), "refusal");
    assert_eq!(stop(json!("stop")), "end_turn");
    assert_eq!(stop(Value::Null), "end_turn");
}

/// 上游报的缓存计数超过 prompt 时要夹住。不夹的话 `prompt - cached` 是 u32 下溢：
/// debug 下 panic，release 下回绕成四十亿级的 `input_tokens` 回给客户端。
#[test]
fn usage_clamps_cache_counts_that_exceed_prompt() {
    let usage_of = |usage: Value| {
        let body = json!({
            "id": "c", "model": "gpt-x",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "x"}}],
            "usage": usage
        });
        let (out, _) =
            response_openai_to_anthropic(&Bytes::from(serde_json::to_vec(&body).unwrap())).unwrap();
        serde_json::from_slice::<Value>(&out).unwrap()["usage"].clone()
    };
    assert_eq!(
        usage_of(json!({"prompt_tokens": 10, "completion_tokens": 1,
                        "prompt_tokens_details": {"cached_tokens": 50}})),
        json!({"input_tokens": 0, "cache_read_input_tokens": 10,
               "cache_creation_input_tokens": 0, "output_tokens": 1})
    );
    assert_eq!(
        usage_of(json!({"prompt_tokens": 10, "completion_tokens": 1,
                        "prompt_tokens_details": {"cached_tokens": 4, "cache_write_tokens": 20}})),
        json!({"input_tokens": 0, "cache_read_input_tokens": 4,
               "cache_creation_input_tokens": 6, "output_tokens": 1})
    );
}
