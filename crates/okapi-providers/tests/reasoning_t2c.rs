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
