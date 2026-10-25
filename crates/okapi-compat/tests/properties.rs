//! 属性测试：不管规则怎么写，校验放行的规则集永远写不进受保护路径、不 panic、结果确定。
//!
//! 引擎运行期有一道兜底（应用后发现主链字段变了就恢复并记 `protected_field_changed`），
//! 这里要求兜底**从不被触发**——触发了就说明校验器漏过了一种写法。

use okapi_compat::protect::Snapshot;
use okapi_compat::{Context, Layer, LayerInput, resolve, validate};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;

const POOL: [&str; 24] = [
    "/model",
    "/stream",
    "/previous_response_id",
    "/conversation",
    "/provider",
    "/messages",
    "/messages/0",
    "/messages/*/role",
    "/messages/*/content",
    "/input",
    "/input/*/role",
    "/tools",
    "/tools/*/type",
    "/temperature",
    "/stream_options",
    "/stream_options/include_usage",
    "/metadata/user",
    "/*/x",
    "/a/*/b",
    "/x",
    "/service_tier",
    "/instructions",
    "/max_tokens",
    "/max_completion_tokens",
];

fn pointer() -> impl Strategy<Value = String> {
    prop::sample::select(POOL.to_vec()).prop_map(str::to_owned)
}

fn scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!(1)),
        Just(json!(0.5)),
        Just(json!(true)),
        Just(json!("developer")),
        Just(json!("system")),
        Just(Value::Null),
        Just(json!({"include_usage": true})),
        Just(json!({"include_usage": false})),
    ]
}

fn op() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::collection::vec(pointer(), 1..4).prop_map(|paths| json!({"op": "remove", "paths": paths})),
        prop::collection::vec(pointer(), 1..4).prop_map(|paths| json!({"op": "keep_only", "paths": paths})),
        (pointer(), scalar(), prop::bool::ANY).prop_map(|(path, value, absent)| json!({
            "op": "set", "path": path, "value": value,
            "mode": if absent { "if_absent" } else { "always" }
        })),
        (pointer(), pointer(), prop::bool::ANY).prop_map(|(from, to, absent)| json!({
            "op": "rename", "from": from, "to": to,
            "mode": if absent { "if_absent" } else { "always" }
        })),
        (pointer(), pointer()).prop_map(|(from, to)| json!({"op": "copy", "from": from, "to": to})),
        pointer().prop_map(|path| json!({"op": "clamp", "path": path, "min": 0, "max": 1})),
        pointer().prop_map(|path| json!({"op": "map_value", "path": path, "table": {"developer": "system", "x": 1}})),
        Just(json!({"op": "role_map", "from": "developer", "to": "system"})),
    ]
}

fn rule_set() -> impl Strategy<Value = Value> {
    prop::collection::vec((prop::collection::vec(op(), 1..5), prop::bool::ANY), 1..4).prop_map(
        |rules| {
            Value::Array(
            rules
                .into_iter()
                .enumerate()
                .map(|(i, (ops, ack))| {
                    json!({"id": format!("r{i}"), "do": ops, "billing_critical_ack": ack})
                })
                .collect(),
        )
        },
    )
}

fn body() -> impl Strategy<Value = Value> {
    (scalar(), scalar(), scalar()).prop_map(|(a, b, c)| {
        json!({
            "model": "m", "stream": true, "previous_response_id": "resp_1",
            "conversation": {"id": "c"}, "provider": {"only": ["x"]},
            "messages": [{"role": "developer", "content": a}, {"role": "user", "content": b}],
            "input": [{"role": "developer", "content": c}],
            "tools": [{"type": "function"}],
            "temperature": 3, "stream_options": {"include_usage": true},
            "metadata": {"user": "u"}, "a": [{"b": 1}], "x": a, "max_tokens": 10,
            "service_tier": "auto", "instructions": "i", "z": {"x": 1}
        })
    })
}

fn ctx() -> Context<'static> {
    Context {
        vendor: None,
        host: None,
        model: "m",
        client_model: "m",
        dialect: "openai.chat",
        operation: "chat",
        stream: true,
        client: None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn accepted_rules_never_touch_protected_fields(rules in rule_set(), body in body()) {
        let Ok(valid) = validate(&rules) else {
            return Ok(());
        };
        let effective = resolve(vec![LayerInput {
            layer: Layer::Channel,
            label: "channel#1@v1".to_owned(),
            rules: Arc::new(valid),
        }]);
        let mut doc = body.clone();
        let snapshot = Snapshot::capture(&doc);
        let report = effective.apply_request_value(&ctx(), &mut doc).expect("no required rules");
        prop_assert!(snapshot.intact(&doc), "主链字段被改了：{rules}");
        prop_assert!(
            report.skipped.iter().all(|s| s.reason != "protected_field_changed"),
            "兜底被触发，说明校验漏过了一种写法：{}",
            rules
        );
        prop_assert!(doc.get("messages").is_some_and(Value::is_array), "对话本体不能丢：{rules}");
    }

    #[test]
    fn application_is_deterministic(rules in rule_set(), body in body()) {
        let Ok(valid) = validate(&rules) else {
            return Ok(());
        };
        let effective = resolve(vec![LayerInput {
            layer: Layer::Channel,
            label: "channel#1@v1".to_owned(),
            rules: Arc::new(valid),
        }]);
        let (mut first, mut second) = (body.clone(), body);
        let a = effective.apply_request_value(&ctx(), &mut first).expect("apply");
        let b = effective.apply_request_value(&ctx(), &mut second).expect("apply");
        prop_assert_eq!(first, second);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn validation_is_stable_through_serialization(rules in rule_set()) {
        if let Ok(valid) = validate(&rules) {
            let again = validate(&valid.to_value()).expect("round trip");
            prop_assert_eq!(valid.to_value(), again.to_value());
        }
    }
}
