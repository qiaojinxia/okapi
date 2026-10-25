//! 属性测试：不管规则怎么写，校验放行的规则集永远写不进受保护路径、不 panic、结果确定。
//!
//! 引擎运行期有一道兜底（应用后发现主链字段变了就恢复并记 `protected_field_changed`），
//! 这里要求兜底**从不被触发**——触发了就说明校验器漏过了一种写法。
//!
//! 响应阶段同理：没有 `billing_critical_ack` 的规则永远动不了用量 / 计价事实；
//! 不需要 `compat.sensitive` 的规则（`Validated::sensitive()` 为空）永远写不出新的模型输出内容。
//! 错误阶段的 `retry_same.mutate` 与请求阶段共用受保护路径：重发的体主链字段不会变。

use okapi_compat::protect::Snapshot;
use okapi_compat::{Action, Context, Layer, LayerInput, resolve, validate};
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

/// `each` 子操作里的路径（相对元素）。刻意混进主链字段名和通配：相对路径里的 `/model` 不是顶层的 `/model`，
/// 但分级时按拼起来的绝对路径保守处理，不能因此漏掉别的写法。
const REL_POOL: [&str; 9] = [
    "/x",
    "/a/b",
    "/model",
    "/stream",
    "/role",
    "/content",
    "/*/y",
    "/include_usage",
    "/cache_control",
];

fn rel() -> impl Strategy<Value = String> {
    prop::sample::select(REL_POOL.to_vec()).prop_map(str::to_owned)
}

fn nested_op() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::collection::vec(rel(), 1..3)
            .prop_map(|paths| json!({"op": "remove", "paths": paths})),
        (rel(), scalar())
            .prop_map(|(path, value)| json!({"op": "set", "path": path, "value": value})),
        (rel(), rel()).prop_map(|(from, to)| json!({"op": "rename", "from": from, "to": to})),
        (rel(), rel()).prop_map(|(from, to)| json!({"op": "copy", "from": from, "to": to})),
        rel().prop_map(|path| json!({"op": "clamp", "path": path, "min": 0, "max": 1})),
    ]
}

fn each_op() -> impl Strategy<Value = Value> {
    (pointer(), prop::collection::vec(nested_op(), 1..3))
        .prop_map(|(path, ops)| json!({"op": "each", "path": path, "do": ops}))
}

fn op() -> impl Strategy<Value = Value> {
    prop_oneof![
        each_op(),
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

// ------------------------------------------------------------------ 响应阶段

const RESPONSE_POOL: [&str; 29] = [
    "/usage",
    "/usage/prompt_tokens",
    "/usage/completion_tokens",
    "/usage/total_tokens",
    "/usage/prompt_tokens_details/cached_tokens",
    "/model",
    "/service_tier",
    "/id",
    "/choices",
    "/choices/0",
    "/choices/*/message",
    "/choices/*/message/content",
    "/choices/0/message/content",
    "/choices/*/message/tool_calls",
    "/choices/*/message/refusal",
    "/choices/1/message/refusal",
    "/choices/*/message/function_call",
    "/choices/*/message/reasoning",
    "/choices/*/message/reasoning_content",
    "/choices/*/finish_reason",
    "/choices/*/delta",
    "/choices/*/delta/content",
    "/choices/*/delta/tool_calls",
    "/choices/*/delta/refusal",
    "/choices/*/delta/reasoning",
    "/*/x",
    "/a/*/b",
    "/system_fingerprint",
    "/x",
];

fn response_pointer() -> impl Strategy<Value = String> {
    prop::sample::select(RESPONSE_POOL.to_vec()).prop_map(str::to_owned)
}

const RESPONSE_REL: [&str; 8] = [
    "/message",
    "/message/content",
    "/message/reasoning",
    "/message/reasoning_content",
    "/delta/content",
    "/finish_reason",
    "/usage",
    "/model",
];

fn response_rel() -> impl Strategy<Value = String> {
    prop::sample::select(RESPONSE_REL.to_vec()).prop_map(str::to_owned)
}

fn expr() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "total - prompt",
        "prompt + completion",
        "max(0, total - prompt - reasoning)",
        "min(prompt, total)",
        "cache_read",
        "7",
    ])
    .prop_map(str::to_owned)
}

fn response_nested() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::collection::vec(response_rel(), 1..3)
            .prop_map(|paths| json!({"op": "remove", "paths": paths})),
        (response_rel(), scalar())
            .prop_map(|(path, value)| json!({"op": "set", "path": path, "value": value})),
        (response_rel(), response_rel())
            .prop_map(|(from, to)| json!({"op": "rename", "from": from, "to": to})),
        (response_rel(), response_rel()).prop_map(
            |(from, to)| json!({"op": "copy", "from": from, "to": to, "mode": "if_absent"})
        ),
        response_rel().prop_map(
            |path| json!({"op": "map_value", "path": path, "table": {"eos": "stop", "x": 1}})
        ),
    ]
}

fn response_op() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::collection::vec(response_pointer(), 1..4)
            .prop_map(|paths| json!({"op": "remove", "paths": paths})),
        (response_pointer(), scalar())
            .prop_map(|(path, value)| json!({"op": "set", "path": path, "value": value})),
        (response_pointer(), response_pointer())
            .prop_map(|(from, to)| json!({"op": "rename", "from": from, "to": to})),
        (response_pointer(), response_pointer())
            .prop_map(|(from, to)| json!({"op": "copy", "from": from, "to": to})),
        response_pointer()
            .prop_map(|path| json!({"op": "clamp", "path": path, "min": 0, "max": 1})),
        response_pointer().prop_map(
            |path| json!({"op": "map_value", "path": path, "table": {"stop": "length", "x": 1}})
        ),
        (
            response_pointer(),
            prop::collection::vec(response_nested(), 1..3)
        )
            .prop_map(|(path, ops)| json!({"op": "each", "path": path, "do": ops})),
        prop::collection::btree_map(
            prop::sample::select(vec![
                "prompt",
                "completion",
                "total",
                "cache_read",
                "reasoning"
            ])
            .prop_map(str::to_owned),
            expr(),
            1..3,
        )
        .prop_map(|slots| json!({"op": "usage_map", "slots": slots})),
    ]
}

fn response_rule_set(ack: impl Strategy<Value = bool> + 'static) -> impl Strategy<Value = Value> {
    prop::collection::vec((prop::collection::vec(response_op(), 1..5), ack), 1..4).prop_map(
        |rules| {
            Value::Array(
                rules
                    .into_iter()
                    .enumerate()
                    .map(|(i, (ops, ack))| {
                        json!({
                            "id": format!("r{i}"), "phase": "response",
                            "select": {"dialect": ["openai.chat"]},
                            "do": ops, "billing_critical_ack": ack
                        })
                    })
                    .collect(),
            )
        },
    )
}

fn response_body() -> impl Strategy<Value = Value> {
    (scalar(), scalar()).prop_map(|(a, b)| {
        json!({
            "id": "chatcmpl-1", "model": "grok-3", "service_tier": "default", "system_fingerprint": "fp",
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 25,
                      "prompt_tokens_details": {"cached_tokens": 3}},
            "choices": [
                {"index": 0, "finish_reason": "eos",
                 "message": {"role": "assistant", "content": a, "reasoning": "r0", "tool_calls": [{"id": "t"}],
                             "function_call": {"name": "f"}},
                 "delta": {"content": b, "reasoning": "d0", "tool_calls": [{"id": "d"}], "refusal": "dr"}},
                {"index": 1, "finish_reason": "stop",
                 "message": {"role": "assistant", "content": "second", "refusal": "no"}}
            ],
            "a": [{"b": 1}], "x": a
        })
    })
}

fn effective_of(rules: &Value) -> Option<okapi_compat::EffectiveRules> {
    let valid = validate(rules).ok()?;
    Some(resolve(vec![LayerInput {
        layer: Layer::Channel,
        label: "channel#1@v1".to_owned(),
        rules: Arc::new(valid),
    }]))
}

/// 响应里账单会读的那几处。
fn billing_facts(doc: &Value) -> [Option<&Value>; 3] {
    [doc.get("usage"), doc.get("model"), doc.get("service_tier")]
}

/// 模型输出本体里的全部值（正文、工具调用、拒答、函数调用，message 与 delta 两处）。
fn output_values(doc: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(choices) = doc.get("choices").and_then(Value::as_array) {
        for choice in choices {
            for part in ["message", "delta"] {
                for field in ["content", "tool_calls", "refusal", "function_call"] {
                    if let Some(value) = choice.get(part).and_then(|p| p.get(field)) {
                        out.push(value.clone());
                    }
                }
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn response_rules_without_the_ack_never_move_billing_facts(
        rules in response_rule_set(Just(false)),
        body in response_body(),
    ) {
        let Some(effective) = effective_of(&rules) else {
            return Ok(());
        };
        let mut doc = body.clone();
        effective.apply_response_value(&ctx(), &mut doc);
        prop_assert_eq!(billing_facts(&doc), billing_facts(&body), "{}", rules);
    }

    #[test]
    fn response_rules_that_need_no_grant_never_write_new_model_output(
        rules in response_rule_set(prop::bool::ANY),
        body in response_body(),
    ) {
        let Ok(valid) = validate(&rules) else {
            return Ok(());
        };
        if !valid.sensitive().is_empty() {
            return Ok(());
        }
        let effective = resolve(vec![LayerInput {
            layer: Layer::Channel,
            label: "channel#1@v1".to_owned(),
            rules: Arc::new(valid),
        }]);
        let mut doc = body.clone();
        effective.apply_response_value(&ctx(), &mut doc);
        let before = output_values(&body);
        for value in output_values(&doc) {
            prop_assert!(before.contains(&value), "凭空长出了输出内容 {value}：{}", rules);
        }
    }

    #[test]
    fn response_application_is_deterministic_and_never_panics(
        rules in response_rule_set(prop::bool::ANY),
        body in response_body(),
    ) {
        let Some(effective) = effective_of(&rules) else {
            return Ok(());
        };
        let (mut first, mut second) = (body.clone(), body);
        let a = effective.apply_response_value(&ctx(), &mut first);
        let b = effective.apply_response_value(&ctx(), &mut second);
        prop_assert_eq!(first, second);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn retry_mutations_never_change_the_trunk_of_the_resent_request(
        ops in prop::collection::vec(op(), 1..4),
        ack in prop::bool::ANY,
        body in body(),
    ) {
        let rules = json!([{
            "id": "e", "phase": "error", "when": {"status": [400]},
            "billing_critical_ack": ack,
            "do": [{"op": "retry_same", "mutate": ops}]
        }]);
        let Some(effective) = effective_of(&rules) else {
            return Ok(());
        };
        let reaction = effective.react(&ctx(), 400, b"{}").expect("the rule matches");
        let Action::RetrySame { mutation, .. } = reaction.action else {
            panic!("retry_same expected");
        };
        let bytes = serde_json::to_vec(&body).expect("json");
        if let Some(resent) = mutation.apply(&bytes) {
            let after: Value = serde_json::from_slice(&resent).expect("json");
            prop_assert!(Snapshot::capture(&body).intact(&after), "重发的体主链字段被改了：{}", rules);
            prop_assert!(after.get("messages").is_some_and(Value::is_array));
        }
    }
}
