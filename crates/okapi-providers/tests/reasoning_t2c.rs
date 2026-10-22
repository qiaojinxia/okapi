//! reasoning 后缀解析/三向注入 + thinking-to-content 转换的 parity 用例。

use bytes::Bytes;
use okapi_providers::ChatEvent;
use okapi_providers::convert::thinking::{ThinkingToContent, rewrite_json};
use okapi_providers::reasoning::{
    Effort, apply_anthropic, apply_gemini, apply_openai, split_reasoning_suffix,
};
use serde_json::{Value, json};

// ---- 后缀解析 ----

#[test]
fn suffix_parsing_matrix() {
    let (base, d) = split_reasoning_suffix("gpt-x-high").unwrap();
    assert_eq!(base, "gpt-x");
    assert_eq!(d.effort, Some(Effort::High));

    let (base, d) = split_reasoning_suffix("m-low").unwrap();
    assert_eq!(base, "m");
    assert_eq!(d.effort, Some(Effort::Low));

    let (base, d) = split_reasoning_suffix("claude-y-thinking").unwrap();
    assert_eq!(base, "claude-y");
    assert_eq!(d.budget_tokens, None);
    assert_eq!(d.effective_budget(), 8_000, "缺省预算");

    let (base, d) = split_reasoning_suffix("claude-y-thinking-4096").unwrap();
    assert_eq!(base, "claude-y");
    assert_eq!(d.budget_tokens, Some(4096));

    assert!(split_reasoning_suffix("gpt-4o").is_none());
    assert!(split_reasoning_suffix("-high").is_none(), "空基名拒绝");
}

// ---- 三向注入 ----

fn body(v: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(v).unwrap())
}

#[test]
fn openai_injection_respects_explicit_field() {
    let (_, d) = split_reasoning_suffix("m-high").unwrap();
    let out = apply_openai(&body(&json!({"model": "m"})), d).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["reasoning_effort"], "high");

    // 显式字段不覆盖
    let out = apply_openai(&body(&json!({"model": "m", "reasoning_effort": "low"})), d).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["reasoning_effort"], "low");

    // 预算档折成档位（§11.26 起的行为变更）：openai 没有预算参数、但有档位，
    // 此前这里是"原样返回"——用户要了思考、上游没收到、钱照收
    let (_, d) = split_reasoning_suffix("m-thinking-2048").unwrap();
    let out = apply_openai(&body(&json!({"model": "m"})), d).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["reasoning_effort"], "low", "2048 按逆映射落在 low");

    // 但显式字段依然不覆盖
    let out = apply_openai(&body(&json!({"model": "m", "reasoning_effort": "high"})), d).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["reasoning_effort"], "high");
}

#[test]
fn anthropic_injection_raises_max_tokens() {
    let (_, d) = split_reasoning_suffix("m-thinking-4096").unwrap();
    let out = apply_anthropic(&body(&json!({"model": "m", "max_tokens": 1024})), d).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["thinking"]["type"], "enabled");
    assert_eq!(v["thinking"]["budget_tokens"], 4096);
    assert_eq!(v["max_tokens"], 4096 + 1024, "预算 ≥ max_tokens 时抬高");

    // 已带 thinking 不覆盖
    let out = apply_anthropic(
        &body(&json!({"thinking": {"type": "disabled"}, "max_tokens": 10})),
        d,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["thinking"]["type"], "disabled");
}

#[test]
fn gemini_injection_into_generation_config() {
    let (_, d) = split_reasoning_suffix("m-medium").unwrap();
    let out = apply_gemini(
        &body(&json!({"contents": [], "generationConfig": {"temperature": 0.5}})),
        d,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        v["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        8000
    );
    assert_eq!(v["generationConfig"]["temperature"], 0.5, "既有配置保留");
}

// ---- thinking-to-content ----

fn reasoning_chunk(text: &str) -> ChatEvent {
    let chunk = json!({"id": "c", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"reasoning_content": text}}]});
    ChatEvent::Data {
        raw: chunk.to_string(),
        event: None,
        has_output: true,
        content_chars: text.chars().count(),
        usage: None,
    }
}

fn content_chunk(text: &str) -> ChatEvent {
    let chunk = json!({"id": "c", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": text}}]});
    ChatEvent::Data {
        raw: chunk.to_string(),
        event: None,
        has_output: true,
        content_chars: text.chars().count(),
        usage: None,
    }
}

#[test]
fn t2c_stream_wraps_reasoning_in_think_tags() {
    let mut st = ThinkingToContent::new();
    let seq: Vec<Result<ChatEvent, okapi_providers::UpstreamError>> = vec![
        Ok(reasoning_chunk("step one")),
        Ok(reasoning_chunk(" step two")),
        Ok(content_chunk("answer")),
        Ok(content_chunk(" more")),
        Ok(ChatEvent::Done),
    ];
    let mut texts = String::new();
    let mut saw_reasoning_field = false;
    for item in seq {
        for out in st.step(item) {
            if let ChatEvent::Data { raw, .. } = out.unwrap() {
                let v: Value = serde_json::from_str(&raw).unwrap();
                if v.pointer("/choices/0/delta/reasoning_content").is_some() {
                    saw_reasoning_field = true;
                }
                if let Some(t) = v
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
                {
                    texts.push_str(t);
                }
            }
        }
    }
    assert!(!saw_reasoning_field, "reasoning_content 字段必须被移除");
    assert_eq!(texts, "<think>\nstep one step two\n</think>\nanswer more");
}

#[test]
fn t2c_json_prefixes_think_block() {
    let body_in = json!({"choices": [{"index": 0, "message": {
        "role": "assistant", "content": "answer", "reasoning_content": "hmm"}}]});
    let out = rewrite_json(&Bytes::from(serde_json::to_vec(&body_in).unwrap()));
    let v: Value = serde_json::from_slice(&out).unwrap();
    let msg = &v["choices"][0]["message"];
    assert_eq!(msg["content"], "<think>\nhmm\n</think>\nanswer");
    assert!(msg.get("reasoning_content").is_none());

    // 无 reasoning：原样
    let plain = json!({"choices": [{"index": 0, "message": {"content": "x"}}]});
    let bytes = Bytes::from(serde_json::to_vec(&plain).unwrap());
    let out = rewrite_json(&bytes);
    assert_eq!(out, bytes);
}

#[test]
fn t2c_json_keeps_array_content_parts() {
    let image = json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}});
    let body_in = json!({"choices": [{"index": 0, "message": {
        "role": "assistant",
        "content": [{"type": "text", "text": "answer"}, image.clone()],
        "reasoning_content": "hmm"}}]});
    let out = rewrite_json(&body(&body_in));
    let v: Value = serde_json::from_slice(&out).unwrap();
    let msg = &v["choices"][0]["message"];
    assert!(msg.get("reasoning_content").is_none());
    assert_eq!(
        msg["content"],
        json!([
            {"type": "text", "text": "<think>\nhmm\n</think>\n"},
            {"type": "text", "text": "answer"},
            image,
        ]),
        "原有分段一个不丢，思考块作为第一个文本片段"
    );
}

#[test]
fn t2c_json_empty_or_null_content_becomes_the_think_block() {
    for content in [json!([]), Value::Null] {
        let body_in = json!({"choices": [{"index": 0, "message": {
            "role": "assistant", "content": content, "reasoning_content": "hmm"}}]});
        let out = rewrite_json(&body(&body_in));
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v["choices"][0]["message"]["content"],
            "<think>\nhmm\n</think>\n"
        );
    }
}

#[test]
fn t2c_json_leaves_unknown_content_shape_alone() {
    let body_in = json!({"choices": [{"index": 0, "message": {
        "content": {"unexpected": true}, "reasoning_content": "hmm"}}]});
    let bytes = body(&body_in);
    assert_eq!(
        rewrite_json(&bytes),
        bytes,
        "认不得的形状原样放行，不吞内容"
    );
}

fn delta_chunk(delta: &Value) -> ChatEvent {
    let chunk = json!({"id": "c", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": delta}]});
    ChatEvent::Data {
        raw: chunk.to_string(),
        event: None,
        has_output: true,
        content_chars: 0,
        usage: None,
    }
}

/// 过一遍转换器，返回每个事件改写后的 `delta` 与 `content_chars`。
fn run_stream(items: Vec<ChatEvent>) -> Vec<(Value, usize)> {
    let mut st = ThinkingToContent::new();
    let mut out = Vec::new();
    for item in items {
        for event in st.step(Ok(item)) {
            let ChatEvent::Data {
                raw, content_chars, ..
            } = event.unwrap()
            else {
                continue;
            };
            let v: Value = serde_json::from_str(&raw).unwrap();
            out.push((v["choices"][0]["delta"].clone(), content_chars));
        }
    }
    out
}

#[test]
fn t2c_stream_array_content_keeps_its_parts_and_closes_the_think_block() {
    let parts = json!([{"type": "text", "text": "answer"}]);
    let out = run_stream(vec![
        reasoning_chunk("step one"),
        delta_chunk(&json!({"content": parts})),
        content_chunk(" tail"),
    ]);
    assert_eq!(out[0].0["content"], "<think>\nstep one");
    assert_eq!(
        out[1].0["content"],
        json!([
            {"type": "text", "text": "\n</think>\n"},
            {"type": "text", "text": "answer"},
        ]),
        "分段正文一个不丢，闭合标签作为首个片段"
    );
    assert_eq!(out[2].0["content"], " tail", "已闭合，后续正文原样");
}

#[test]
fn t2c_stream_reasoning_and_array_content_in_one_chunk() {
    let parts = json!([{"type": "text", "text": "answer"}]);
    let out = run_stream(vec![delta_chunk(
        &json!({"reasoning_content": "why", "content": parts}),
    )]);
    let (delta, chars) = &out[0];
    assert!(delta.get("reasoning_content").is_none());
    assert_eq!(
        delta["content"],
        json!([
            {"type": "text", "text": "<think>\nwhy\n</think>\n"},
            {"type": "text", "text": "answer"},
        ])
    );
    assert_eq!(
        *chars,
        "<think>\nwhy\n</think>\n".chars().count() + "answer".chars().count(),
        "改写后重数：标签、思考和分段正文都算"
    );
}

#[test]
fn t2c_stream_unknown_content_shape_splits_the_thinking_off_and_keeps_the_rest() {
    let out = run_stream(vec![
        delta_chunk(&json!({"reasoning_content": "x", "content": {"weird": 1}})),
        reasoning_chunk("y"),
        content_chunk("answer"),
    ]);
    // 认不得的正文拼不进标签：思考单独成一条，原 chunk 去掉 reasoning_content、正文原样保留
    assert_eq!(out[0].0, json!({"content": "<think>\nx"}));
    assert_eq!(out[1].0, json!({"content": {"weird": 1}}));
    // 开标签已记下：后续思考接在标签里，不会再从标签外漏出来
    assert_eq!(out[2].0["content"], "y");
    assert_eq!(out[3].0["content"], "\n</think>\nanswer");
    assert!(
        out.iter()
            .all(|(delta, _)| delta.get("reasoning_content").is_none())
    );
}
