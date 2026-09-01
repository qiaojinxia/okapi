//! `ensure_stream_usage`：OpenAI 方言流式请求补 `stream_options.include_usage`。
//! 缺这一帧，结算只能落字符估算（漏收），故网关在客户端未声明时补齐。

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
fn does_not_override_explicit_stream_options() {
    // 客户端显式关掉 usage 帧是其自身取舍（可能有严格的 chunk 解析器），不强改；
    // 站长要兜住这部分收入应走本地 tokenizer 复核，而非覆盖客户端声明。
    let out = ensure_stream_usage(&body(&json!({
        "model": "gpt-4o", "stream": true,
        "stream_options": {"include_usage": false},
        "messages": [{"role": "user", "content": "hi"}]
    })))
    .unwrap();
    assert_eq!(
        parse(&out)["stream_options"],
        json!({"include_usage": false})
    );
}

#[test]
fn rejects_non_object_body() {
    assert!(ensure_stream_usage(&Bytes::from_static(b"[1,2,3]")).is_err());
    assert!(ensure_stream_usage(&Bytes::from_static(b"not json")).is_err());
}
