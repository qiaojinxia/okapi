//! `ensure_stream_usage`：OpenAI 方言流式请求补 `stream_options.include_usage`。
//! 缺这一帧，结算只能落字符估算（漏收），故网关一律补齐：客户端没声明就加上，声明了 false 也改回 true。

use bytes::Bytes;
use okapi_providers::ensure_stream_usage;
use serde_json::{Value, json};

fn body(v: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(v).unwrap())
}

fn parse(b: &Bytes) -> Value {
    serde_json::from_slice(b).unwrap()
}

#[test]
fn injects_when_client_omits_stream_options() {
    let out = ensure_stream_usage(&body(&json!({
        "model": "gpt-4o", "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    })))
    .unwrap();
    assert_eq!(
        parse(&out)["stream_options"],
        json!({"include_usage": true}),
        "客户端没声明就必须补，否则上游不返 usage"
    );
}

#[test]
fn preserves_other_fields() {
    let out = ensure_stream_usage(&body(&json!({
        "model": "gpt-4o", "stream": true, "temperature": 0.7,
        "messages": [{"role": "user", "content": "hi"}]
    })))
    .unwrap();
    let v = parse(&out);
    assert_eq!(v["model"], "gpt-4o");
    assert_eq!(v["temperature"], 0.7);
    assert_eq!(v["messages"][0]["content"], "hi");
}

#[test]
fn explicit_stream_options_cannot_switch_off_usage() {
    // 客户端传 include_usage:false 就能让上游不返 usage、结算落到字符估算，是可以刻意构造的
    // 少收：一律改回 true，其余字段原样保留
    let out = ensure_stream_usage(&body(&json!({
        "model": "gpt-4o", "stream": true,
        "stream_options": {"include_usage": false, "include_obfuscation": false},
        "messages": [{"role": "user", "content": "hi"}]
    })))
    .unwrap();
    assert_eq!(
        parse(&out)["stream_options"],
        json!({"include_usage": true, "include_obfuscation": false})
    );
}

#[test]
fn rejects_non_object_body() {
    assert!(ensure_stream_usage(&Bytes::from_static(b"[1,2,3]")).is_err());
    assert!(ensure_stream_usage(&Bytes::from_static(b"not json")).is_err());
}
