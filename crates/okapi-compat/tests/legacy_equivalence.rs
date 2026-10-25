//! 等价替换的回归：渠道的 `strip_request_fields` / `inject_request_fields`（原
//! `gateway::chat::adjust_request_fields`）与它迁移成的两条渠道层规则，对任意请求体结果一致。
//!
//! `legacy_adjust` 是旧实现的逐字复刻（只把 `Bytes` 换成 `Value`），`rules_from_legacy` 是迁移
//! 脚本（`0006_compat_rules.sql`）的 Rust 镜像——两边的过滤条件必须一致，改一边就要改另一边。

use okapi_compat::pointer::escape;
use okapi_compat::{Context, Layer, LayerInput, resolve, validate};
use proptest::prelude::*;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::sync::Arc;

const STRIP_PROTECTED: [&str; 5] = [
    "model",
    "messages",
    "stream",
    "previous_response_id",
    "conversation",
];
const INJECT_PROTECTED: [&str; 6] = [
    "model",
    "messages",
    "stream",
    "provider",
    "previous_response_id",
    "conversation",
];

/// 旧 `adjust_request_fields` 的行为：先剥后注，受保护键两边都不动，只动顶层。
fn legacy_adjust(body: &Value, strip: &[String], inject: &Map<String, Value>) -> Value {
    let mut value = body.clone();
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    for field in strip {
        if !STRIP_PROTECTED.contains(&field.as_str()) {
            obj.remove(field);
        }
    }
    for (key, val) in inject {
        if !INJECT_PROTECTED.contains(&key.as_str()) {
            obj.insert(key.clone(), val.clone());
        }
    }
    value
}

/// 迁移脚本的 Rust 镜像：旧设置 → 渠道层规则集。
/// 过滤掉写不成指针的键（空串、超 128 字符、字面 `*`）与受保护键；`provider` 不进 strip——
/// 它在 `shape_upstream_body` 里更早就被 `routing_prefs::strip` 剥掉了，进了也是空操作，
/// 而新注册表把它列为受保护路径，写进去反而让整个规则集过不了校验。
fn rules_from_legacy(strip: &[String], inject: &Map<String, Value>) -> Value {
    let expressible = |key: &str| !key.is_empty() && key.chars().count() <= 128 && key != "*";
    let mut seen = HashSet::new();
    let paths: Vec<String> = strip
        .iter()
        .filter(|f| expressible(f))
        .filter(|f| !STRIP_PROTECTED.contains(&f.as_str()) && f.as_str() != "provider")
        .filter(|f| seen.insert((*f).clone()))
        .take(256)
        .map(|f| format!("/{}", escape(f)))
        .collect();
    let sets: Vec<Value> = inject
        .iter()
        .filter(|(k, _)| expressible(k) && !INJECT_PROTECTED.contains(&k.as_str()))
        .take(32)
        .map(|(k, v)| json!({"op": "set", "path": format!("/{}", escape(k)), "value": v}))
        .collect();
    let mut rules = Vec::new();
    if !paths.is_empty() {
        let ack = paths.iter().any(|p| p == "/stream_options");
        rules.push(json!({
            "id": "strip-request-fields",
            "do": [{"op": "remove", "paths": paths}],
            "billing_critical_ack": ack,
        }));
    }
    if !sets.is_empty() {
        let ack = inject.contains_key("stream_options");
        rules.push(json!({
            "id": "inject-request-fields",
            "do": sets,
            "billing_critical_ack": ack,
        }));
    }
    Value::Array(rules)
}

fn engine_adjust(body: &Value, strip: &[String], inject: &Map<String, Value>) -> Value {
    let rules = rules_from_legacy(strip, inject);
    let valid = validate(&rules)
        .unwrap_or_else(|issues| panic!("迁移出的规则集不合法：{issues:?}\n{rules}"));
    let effective = resolve(vec![LayerInput {
        layer: Layer::Channel,
        label: "channel#1@v1".to_owned(),
        rules: Arc::new(valid),
    }]);
    let ctx = Context {
        vendor: None,
        host: None,
        model: "m",
        client_model: "m",
        dialect: "openai.chat",
        operation: "chat",
        stream: false,
        client: None,
    };
    let outcome = effective
        .apply_request(&ctx, body.to_string().as_bytes())
        .expect("apply");
    outcome.body.map_or_else(
        || body.clone(),
        |bytes| serde_json::from_slice(&bytes).expect("json"),
    )
}

/// 与真实流水线一致：okapi 的 `provider` 路由指令在整形前已被剥掉。
fn without_provider(mut body: Value) -> Value {
    if let Some(obj) = body.as_object_mut() {
        obj.remove("provider");
    }
    body
}

fn key() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "a",
        "b",
        "user",
        "metadata",
        "stream_options",
        "service_tier",
        "temperature",
        "a/b",
        "a~b",
        "~1",
        "model",
        "messages",
        "stream",
        "provider",
        "previous_response_id",
        "conversation",
        "instructions",
        "x1",
    ])
    .prop_map(str::to_owned)
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!(1)),
        Just(json!(1.5)),
        Just(json!(true)),
        Just(json!("s")),
        Just(Value::Null),
        Just(json!({"k": [1, 2]})),
        Just(json!([])),
    ]
}

fn body() -> impl Strategy<Value = Value> {
    prop::collection::btree_map(key(), leaf(), 0..10)
        .prop_map(|m| Value::Object(m.into_iter().collect()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn migrated_rules_behave_like_the_legacy_settings(
        body in body(),
        strip in prop::collection::vec(key(), 0..6),
        inject in prop::collection::btree_map(key(), leaf(), 0..5),
    ) {
        let inject: Map<String, Value> = inject.into_iter().collect();
        let body = without_provider(body);
        prop_assert_eq!(
            engine_adjust(&body, &strip, &inject),
            legacy_adjust(&body, &strip, &inject)
        );
    }
}

#[test]
fn the_common_real_world_settings_round_trip() {
    let strip = vec![
        "stream_options".to_owned(),
        "user".to_owned(),
        "service_tier".to_owned(),
    ];
    let inject: Map<String, Value> = serde_json::from_value(json!({
        "thinking": {"type": "disabled"},
        "metadata": {"source": "okapi"}
    }))
    .expect("map");
    let body = json!({
        "model": "m", "messages": [], "stream": true, "user": "u",
        "stream_options": {"include_usage": true}, "service_tier": "auto"
    });
    let expected = json!({
        "model": "m", "messages": [], "stream": true,
        "thinking": {"type": "disabled"}, "metadata": {"source": "okapi"}
    });
    assert_eq!(engine_adjust(&body, &strip, &inject), expected);
    assert_eq!(legacy_adjust(&body, &strip, &inject), expected);
}

#[test]
fn protected_keys_in_the_legacy_lists_are_dropped_not_fatal() {
    let strip = vec![
        "model".to_owned(),
        "messages".to_owned(),
        "provider".to_owned(),
        "user".to_owned(),
    ];
    let inject: Map<String, Value> = serde_json::from_value(json!({
        "model": "evil", "stream": false, "conversation": "c", "ok": 1
    }))
    .expect("map");
    let rules = rules_from_legacy(&strip, &inject);
    let valid = validate(&rules).expect("valid");
    assert_eq!(valid.len(), 2);
    let body = json!({"model": "m", "messages": [{"role": "user"}], "stream": true, "user": "u"});
    assert_eq!(
        engine_adjust(&body, &strip, &inject),
        json!({"model": "m", "messages": [{"role": "user"}], "stream": true, "ok": 1})
    );
}

#[test]
fn keys_that_cannot_be_pointers_are_skipped_instead_of_poisoning_the_set() {
    let strip = vec![
        String::new(),
        "*".to_owned(),
        "x".repeat(200),
        "user".to_owned(),
    ];
    let inject: Map<String, Value> =
        serde_json::from_value(json!({"": 1, "*": 2, "ok": 3})).expect("map");
    let rules = rules_from_legacy(&strip, &inject);
    assert!(validate(&rules).is_ok(), "{rules}");
}
