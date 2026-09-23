//! gemini_to_openai 转换用例：Gemini 协议**客户端**（Gemini SDK / Gemini CLI）打进来，
//! 请求转成 OpenAI chat 发给上游，响应再转回 `GenerateContentResponse`。
//!
//! 为什么单独一个文件：`convert_gemini.rs` 名字像两个方向都测了，实际只测
//! `openai_to_gemini`（OpenAI 客户端 + Gemini 上游）。本方向端到端由网关层的
//! `gateway_gemini_ingress` 覆盖，但那一层只证明"转换确实发生了"——逐条规则变异实测，
//! 下面 5 条映射规则里它只钉住 1 条（流式注入 include_usage），另外 4 条改坏了全绿：
//!
//! - `candidatesTokenCount` 把 thoughts 也算进去 → Gemini SDK 用户看到的 token 数不对
//! - functionResponse 配到**最晚**而非最早的同名调用 → 多次调用同名工具时结果串位
//! - `length` 不映射 `MAX_TOKENS` → 客户端检测不到输出被截断
//! - thought 部件回灌进历史 → 推理内容被当作正文发给上游，既多耗 token 又泄露推理
//!
//! 本文件是纯函数单测（不依赖库、不起 mock 上游），规则改坏时直接指出是哪个字段。

use bytes::Bytes;
use okapi_api::UsageProbe;
use okapi_providers::convert::gemini_to_openai::{
    gemini_usage_json, request_gemini_to_openai, response_openai_to_gemini,
};
use serde_json::{Value, json};

fn req(body: &Value, model: &str, stream: bool) -> Value {
    let raw = Bytes::from(serde_json::to_vec(body).unwrap());
    let out = request_gemini_to_openai(&raw, model, stream).unwrap();
    serde_json::from_slice(&out).unwrap()
}

fn resp(body: &Value) -> (Value, UsageProbe) {
    let raw = Bytes::from(serde_json::to_vec(body).unwrap());
    let (bytes, probe) = response_openai_to_gemini(&raw).unwrap();
    (
        serde_json::from_slice(&bytes).unwrap(),
        probe.expect("非流式响应必须回计费探针"),
    )
}

fn usage(prompt: u32, completion: u32, cached: u32, reasoning: u32) -> UsageProbe {
    serde_json::from_value(json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "prompt_tokens_details": {"cached_tokens": cached},
        "completion_tokens_details": {"reasoning_tokens": reasoning},
    }))
    .unwrap()
}

#[test]
fn request_maps_system_history_and_takes_model_from_url() {
    let out = req(
        &json!({
            "systemInstruction": {"parts": [{"text": "be brief"}, {"text": "use english"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [
                    {"text": "planning...", "thought": true},
                    {"text": "hello"}
                ]},
                {"role": "user", "parts": [{"text": "again"}]}
            ]
        }),
        "gpt-up",
        false,
    );

    // 模型名来自 URL 路径（调用方传入），不是请求体
    assert_eq!(out["model"], "gpt-up");
    assert_eq!(
        out["messages"],
        json!([
            // 多段 systemInstruction 以换行拼接
            {"role": "system", "content": "be brief\nuse english"},
            // 纯文本 user 降成字符串 content
            {"role": "user", "content": "hi"},
            // thought 部件不回灌进历史（OpenAI 没有对应位）；无工具调用时不带 tool_calls
            {"role": "assistant", "content": "hello"},
            {"role": "user", "content": "again"},
        ])
    );
    assert!(out.get("stream").is_none(), "非流式不得带 stream");
    assert!(out.get("stream_options").is_none());
}

#[test]
fn request_stream_flag_forces_usage_in_final_chunk() {
    let out = req(
        &json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}),
        "m",
        true,
    );
    assert_eq!(out["stream"], true);
    // 与 openai.rs::ensure_stream_usage 同一条红线：流式不带 include_usage，
    // 上游不回 usage，计费只能靠本地估算（曾是 CJK 流式约 72% 少收的根因）
    assert_eq!(out["stream_options"], json!({"include_usage": true}));
}

#[test]
fn request_pairs_function_responses_with_generated_call_ids() {
    let out = req(
        &json!({
            "contents": [
                {"role": "user", "parts": [{"text": "weather in two cities"}]},
                {"role": "model", "parts": [
                    {"functionCall": {"name": "weather", "args": {"city": "sf"}}},
                    {"functionCall": {"name": "weather", "args": {"city": "ny"}}}
                ]},
                {"role": "user", "parts": [
                    // 单键对象结果直接取值；非单键保留原样
                    {"functionResponse": {"name": "weather", "response": {"result": "sunny"}}},
                    {"functionResponse": {"name": "weather", "response": {"t": 20, "rain": false}}}
                ]}
            ]
        }),
        "m",
        false,
    );

    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 4, "{msgs:?}");
    // Gemini 的 functionCall 没有 id：按出现顺序生成 call_<n>，同一条 content 并成一条 assistant
    assert_eq!(
        msgs[1],
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [
                {"id": "call_0", "type": "function",
                 "function": {"name": "weather", "arguments": "{\"city\":\"sf\"}"}},
                {"id": "call_1", "type": "function",
                 "function": {"name": "weather", "arguments": "{\"city\":\"ny\"}"}}
            ]
        })
    );
    // functionResponse 只靠 name 回指：同名取**最早未匹配**的那个，顺序配对不串
    assert_eq!(
        msgs[2],
        json!({"role": "tool", "tool_call_id": "call_0", "content": "sunny"})
    );
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "call_1");
    let second: Value = serde_json::from_str(msgs[3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(second, json!({"t": 20, "rain": false}));
}

#[test]
fn request_maps_media_parts_to_multipart_user_content() {
    let out = req(
        &json!({"contents": [{"role": "user", "parts": [
            {"text": "what is this"},
            {"inlineData": {"mimeType": "image/png", "data": "AAAA"}},
            {"inline_data": {"mime_type": "audio/wav", "data": "BBBB"}},
            // OpenAI chat 没有 pdf 部件：丢弃，不得把整条消息搞坏
            {"inlineData": {"mimeType": "application/pdf", "data": "CCCC"}}
        ]}]}),
        "m",
        false,
    );
    assert_eq!(
        out["messages"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "what is this"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
            {"type": "input_audio", "input_audio": {"data": "BBBB", "format": "wav"}}
        ]}])
    );
}

#[test]
fn request_maps_generation_config_and_structured_output() {
    let out = req(
        &json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {
                "maxOutputTokens": 128,
                "temperature": 0.2,
                "topP": 0.9,
                "stopSequences": ["END"],
                "presencePenalty": 0.1,
                "frequencyPenalty": 0.3,
                "seed": 7,
                "responseMimeType": "application/json",
                "responseSchema": {"type": "object", "properties": {"a": {"type": "string"}}}
            }
        }),
        "m",
        false,
    );
    assert_eq!(out["max_tokens"], 128);
    assert_eq!(out["temperature"], 0.2);
    assert_eq!(out["top_p"], 0.9);
    assert_eq!(out["stop"], json!(["END"]));
    assert_eq!(out["presence_penalty"], 0.1);
    assert_eq!(out["frequency_penalty"], 0.3);
    assert_eq!(out["seed"], 7);
    // 给了 schema 就走 json_schema（优先于只看 mimeType 的 json_object）
    assert_eq!(
        out["response_format"],
        json!({"type": "json_schema", "json_schema": {
            "name": "response",
            "schema": {"type": "object", "properties": {"a": {"type": "string"}}}
        }})
    );

    // 只给 mimeType → json_object
    let only_mime = req(
        &json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generation_config": {"response_mime_type": "application/json", "max_output_tokens": 9}
        }),
        "m",
        false,
    );
    assert_eq!(only_mime["response_format"], json!({"type": "json_object"}));
    // snake_case 别名同样生效（部分语言的 Gemini SDK 用 snake_case 序列化）
    assert_eq!(only_mime["max_tokens"], 9);
}

#[test]
fn request_thinking_budget_zero_means_off() {
    let with_budget = req(
        &json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {"thinkingConfig": {"thinkingBudget": 8192}}
        }),
        "m",
        false,
    );
    let effort = with_budget["reasoning_effort"].as_str().unwrap_or("");
    assert!(
        ["minimal", "low", "medium", "high"].contains(&effort),
        "正预算必须映射成一个 reasoning_effort 档位，实得 {:?}",
        with_budget["reasoning_effort"]
    );

    let off = req(
        &json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {"thinkingConfig": {"thinkingBudget": 0}}
        }),
        "m",
        false,
    );
    assert!(
        off.get("reasoning_effort").is_none(),
        "预算 0 = 关闭思考，不得注入 reasoning_effort"
    );
}

#[test]
fn request_maps_function_declarations_and_calling_mode() {
    let decl = json!({"functionDeclarations": [
        {"name": "weather", "description": "get weather",
         "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}},
        // 缺 parameters 时补空 object schema，否则 OpenAI 上游会拒
        {"name": "now"}
    ]});
    let base = |tool_config: Value| {
        req(
            &json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "tools": [decl.clone()],
                "toolConfig": tool_config
            }),
            "m",
            false,
        )
    };

    let out = base(json!({"functionCallingConfig": {"mode": "AUTO"}}));
    assert_eq!(
        out["tools"],
        json!([
            {"type": "function", "function": {
                "name": "weather", "description": "get weather",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
            }},
            {"type": "function", "function": {
                "name": "now", "description": "",
                "parameters": {"type": "object", "properties": {}}
            }}
        ])
    );
    assert_eq!(out["tool_choice"], "auto");

    assert_eq!(
        base(json!({"functionCallingConfig": {"mode": "NONE"}}))["tool_choice"],
        "none"
    );
    assert_eq!(
        base(json!({"functionCallingConfig": {"mode": "ANY"}}))["tool_choice"],
        "required"
    );
    // ANY + 恰好一个允许的函数 → 指名调用
    assert_eq!(
        base(json!({"functionCallingConfig": {
            "mode": "ANY", "allowedFunctionNames": ["weather"]
        }}))["tool_choice"],
        json!({"type": "function", "function": {"name": "weather"}})
    );
}

#[test]
fn response_maps_parts_finish_and_ids() {
    let (out, _) = resp(&json!({
        "id": "chatcmpl-1",
        "model": "gpt-up",
        "choices": [{
            "index": 0,
            "finish_reason": "length",
            "message": {
                "role": "assistant",
                "reasoning_content": "let me think",
                "content": "answer",
                "tool_calls": [{"id": "x", "type": "function",
                    "function": {"name": "weather", "arguments": "{\"city\":\"sf\"}"}}]
            }
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    }));

    let cand = &out["candidates"][0];
    assert_eq!(cand["content"]["role"], "model");
    // 顺序：thought 在前、正文其次、函数调用最后
    assert_eq!(
        cand["content"]["parts"],
        json!([
            {"text": "let me think", "thought": true},
            {"text": "answer"},
            {"functionCall": {"name": "weather", "args": {"city": "sf"}}}
        ])
    );
    assert_eq!(cand["finishReason"], "MAX_TOKENS");
    assert_eq!(cand["index"], 0);
    assert_eq!(out["modelVersion"], "gpt-up");
    assert_eq!(out["responseId"], "chatcmpl-1");
}

#[test]
fn response_finish_reason_table() {
    let finish = |reason: Value| {
        resp(&json!({"choices": [{"finish_reason": reason,
            "message": {"content": "x"}}]}))
        .0["candidates"][0]["finishReason"]
            .clone()
    };
    assert_eq!(finish(json!("length")), "MAX_TOKENS");
    assert_eq!(finish(json!("content_filter")), "SAFETY");
    // Gemini 对函数调用也回 STOP
    assert_eq!(finish(json!("tool_calls")), "STOP");
    assert_eq!(finish(json!("stop")), "STOP");
    assert_eq!(finish(Value::Null), "STOP");
}

#[test]
fn response_tool_arguments_that_are_not_an_object_become_empty_args() {
    let (out, _) = resp(&json!({"choices": [{"message": {"tool_calls": [
        {"function": {"name": "a", "arguments": "not json"}},
        {"function": {"name": "b", "arguments": "[1,2]"}}
    ]}}]}));
    // Gemini 的 functionCall.args 必须是对象；坏参数不得把整条响应搞坏
    assert_eq!(
        out["candidates"][0]["content"]["parts"],
        json!([
            {"functionCall": {"name": "a", "args": {}}},
            {"functionCall": {"name": "b", "args": {}}}
        ])
    );
}

/// usage 映射直接决定两件事：Gemini SDK 客户端看到的 token 数，以及计费拿到的探针。
/// 口径（模块头注释）：promptTokenCount 含缓存、candidatesTokenCount = completion − reasoning、
/// thoughtsTokenCount = reasoning、totalTokenCount = prompt + completion。
#[test]
fn response_usage_metadata_and_probe_agree_with_upstream() {
    let (out, probe) = resp(&json!({
        "choices": [{"message": {"content": "x"}}],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 40,
            "prompt_tokens_details": {"cached_tokens": 30},
            "completion_tokens_details": {"reasoning_tokens": 15}
        }
    }));
    assert_eq!(
        out["usageMetadata"],
        json!({
            "promptTokenCount": 100,
            "candidatesTokenCount": 25,
            "thoughtsTokenCount": 15,
            "cachedContentTokenCount": 30,
            "totalTokenCount": 140
        })
    );
    // 探针是计费的输入：必须与上游 usage 逐字段相同，不能被"客户端口径"改写
    assert_eq!(probe.prompt_tokens, 100);
    assert_eq!(probe.completion_tokens, 40);
    assert_eq!(probe.prompt_tokens_details.cached_tokens, 30);
    assert_eq!(probe.completion_tokens_details.reasoning_tokens, 15);
}

#[test]
fn usage_json_omits_zero_optional_counts_and_clamps_reasoning() {
    // 无缓存、无推理：两个可选字段都不出现
    assert_eq!(
        gemini_usage_json(usage(10, 5, 0, 0)),
        json!({"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15})
    );
    // 上游若报 reasoning 超过 completion：按 completion 夹，
    // candidates 不得下溢（u32 减法下溢在 debug 下 panic、release 下回绕成巨大值）
    assert_eq!(
        gemini_usage_json(usage(10, 5, 0, 9)),
        json!({
            "promptTokenCount": 10,
            "candidatesTokenCount": 0,
            "thoughtsTokenCount": 5,
            "totalTokenCount": 15
        })
    );
}

#[test]
fn response_without_usage_still_returns_a_zero_probe() {
    let (out, probe) = resp(&json!({"choices": [{"message": {"content": "x"}}]}));
    assert_eq!(probe.prompt_tokens, 0);
    assert_eq!(probe.completion_tokens, 0);
    assert_eq!(
        out["usageMetadata"],
        json!({"promptTokenCount": 0, "candidatesTokenCount": 0, "totalTokenCount": 0})
    );
}
