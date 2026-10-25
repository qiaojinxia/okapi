//! 响应 / 错误 / 能力三个阶段，以及 `each`、`usage_map`、`when` 的行为（只用公共接口）。
//! 校验器的拒绝码是用户在界面和 API 里看到的东西，所以这里逐个钉住。

use okapi_compat::{
    Action, Context, EffectiveRules, Layer, LayerInput, Phase, Report, resolve, validate,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn codes(doc: &Value) -> Vec<&'static str> {
    match validate(doc) {
        Ok(_) => Vec::new(),
        Err(issues) => issues.into_iter().map(|issue| issue.code).collect(),
    }
}

fn layer(layer: Layer, label: &str, doc: &Value) -> LayerInput {
    LayerInput {
        layer,
        label: label.to_owned(),
        rules: Arc::new(validate(doc).unwrap_or_else(|issues| panic!("{doc}: {issues:?}"))),
    }
}

fn rules(doc: &Value) -> EffectiveRules {
    resolve(vec![layer(Layer::Channel, "channel#1@v1", doc)])
}

fn ctx() -> Context<'static> {
    Context {
        vendor: Some("builtin:xai"),
        host: Some("api.x.ai"),
        model: "grok-3",
        client_model: "grok-3",
        dialect: "openai.chat",
        operation: "chat",
        stream: false,
        client: None,
    }
}

fn response_rule(ops: &Value) -> Value {
    json!([{"id": "r", "phase": "response", "select": {"dialect": ["openai.chat"]}, "do": ops}])
}

fn error_rule(when: &Value, op: &Value) -> Value {
    json!([{"id": "e", "phase": "error", "when": when, "do": [op]}])
}

// ---------------------------------------------------------------- validation

#[test]
fn the_phase_by_operation_matrix_is_enforced() {
    // 每个阶段里「不属于它」的操作都给 op_not_allowed_in_phase
    let request_only = json!({"op": "role_map", "from": "a", "to": "b"});
    let response_only = json!({"op": "usage_map", "slots": {"completion": "total - prompt"}});
    let capability_only = json!({"op": "capability", "facts": {"vision": false}});
    let error_only = json!({"op": "failover"});
    let plain = json!({"op": "remove", "paths": ["/x"]});

    let in_phase = |phase: &str, op: &Value| {
        let mut rule = json!({"id": "t", "phase": phase, "do": [op]});
        match phase {
            "response" => rule["select"] = json!({"dialect": ["openai.chat"]}),
            "error" => rule["when"] = json!({"status": [404]}),
            _ => {}
        }
        codes(&json!([rule]))
    };
    assert_eq!(in_phase("request", &request_only), Vec::<&str>::new());
    assert_eq!(
        in_phase("request", &response_only),
        ["op_not_allowed_in_phase"]
    );
    assert_eq!(
        in_phase("request", &capability_only),
        ["op_not_allowed_in_phase"]
    );
    assert_eq!(
        in_phase("request", &error_only),
        ["op_not_allowed_in_phase"]
    );
    assert_eq!(
        in_phase("response", &request_only),
        ["op_not_allowed_in_phase"]
    );
    assert_eq!(
        in_phase("response", &json!({"op": "keep_only", "paths": ["/a"]})),
        ["op_not_allowed_in_phase"]
    );
    assert_eq!(in_phase("response", &plain), Vec::<&str>::new());
    assert_eq!(in_phase("error", &plain), ["op_not_allowed_in_phase"]);
    assert_eq!(
        in_phase("error", &response_only),
        ["op_not_allowed_in_phase"]
    );
    assert_eq!(in_phase("capability", &plain), ["op_not_allowed_in_phase"]);
    assert_eq!(in_phase("capability", &capability_only), Vec::<&str>::new());
}

#[test]
fn required_is_request_only_and_when_is_error_only() {
    let required = json!([{"id": "t", "phase": "capability", "required": true, "do": [{"op": "capability", "facts": {"tools": true}}]}]);
    assert_eq!(codes(&required), ["required_not_supported"]);
    let stray_when =
        json!([{"id": "t", "when": {"status": [404]}, "do": [{"op": "remove", "paths": ["/x"]}]}]);
    assert_eq!(codes(&stray_when), ["when_not_allowed"]);
    let missing_when = json!([{"id": "t", "phase": "error", "do": [{"op": "failover"}]}]);
    assert_eq!(codes(&missing_when), ["when_required"]);
    let empty_when = json!([{"id": "t", "phase": "error", "when": {}, "do": [{"op": "failover"}]}]);
    assert_eq!(codes(&empty_when), ["when_empty"]);
    // 墓碑（只有 disabled）不受阶段规则约束，才能关掉上一层的任何阶段的规则
    for phase in ["response", "error", "capability"] {
        let tombstone = json!([{"id": "t", "phase": phase, "disabled": true}]);
        assert_eq!(codes(&tombstone), Vec::<&str>::new(), "{phase}");
    }
}

#[test]
fn error_rules_may_only_react_to_statuses_the_gateway_leaves_alone() {
    for status in [401, 402, 403, 408, 429, 500, 502, 529, 200, 301, 99, 600] {
        let doc = error_rule(&json!({"status": [status]}), &json!({"op": "failover"}));
        assert_eq!(codes(&doc), ["error_status_not_reactable"], "{status}");
    }
    for status in [400, 404, 405, 409, 410, 413, 415, 422] {
        let doc = error_rule(&json!({"status": [status]}), &json!({"op": "failover"}));
        assert_eq!(codes(&doc), Vec::<&str>::new(), "{status}");
    }
    let too_many: Vec<u16> = (400..430)
        .filter(|s| ![401, 402, 403, 408, 429].contains(s))
        .collect();
    assert!(too_many.len() > 16);
    let doc = error_rule(&json!({"status": too_many}), &json!({"op": "failover"}));
    assert_eq!(codes(&doc), ["when_too_long"]);
    let blank = error_rule(
        &json!({"body_contains": ["  "]}),
        &json!({"op": "failover"}),
    );
    assert_eq!(codes(&blank), ["when_invalid_item"]);
}

#[test]
fn error_actions_are_validated_one_per_rule() {
    let when = json!({"status": [400]});
    let two = json!([{"id": "e", "phase": "error", "when": when, "do": [{"op": "failover"}, {"op": "fail", "status": 422}]}]);
    assert_eq!(codes(&two), ["error_rule_single_action"]);
    let none = json!([{"id": "e", "phase": "error", "when": when, "do": []}]);
    assert_eq!(codes(&none), ["no_ops"]);
    let bad = |op: Value| codes(&error_rule(&when, &op));
    assert_eq!(bad(json!({"op": "retry_same"})), ["no_ops"]);
    assert_eq!(
        bad(
            json!({"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}], "max": 0})
        ),
        ["invalid_retry"]
    );
    assert_eq!(
        bad(
            json!({"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}], "max": 4})
        ),
        ["invalid_retry"]
    );
    assert_eq!(
        bad(
            json!({"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}], "max": 3})
        ),
        Vec::<&str>::new()
    );
    // mutate 改的是发往上游的请求体，按请求阶段的注册表校验
    assert_eq!(
        bad(json!({"op": "retry_same", "mutate": [{"op": "set", "path": "/model", "value": "x"}]})),
        ["protected_path"]
    );
    assert_eq!(
        bad(
            json!({"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/stream_options"]}]})
        ),
        ["billing_critical_ack_required"]
    );
    assert_eq!(
        bad(json!({"op": "retry_same", "mutate": [{"op": "failover"}]})),
        ["op_not_allowed_in_phase"]
    );
    assert_eq!(
        bad(json!({"op": "retry_same", "mutate": [{"op": "teleport"}]})),
        ["unknown_op"]
    );
    assert_eq!(
        bad(json!({"op": "failover", "cooldown_secs": 0})),
        ["invalid_cooldown"]
    );
    assert_eq!(
        bad(json!({"op": "failover", "cooldown_secs": 86_401})),
        ["invalid_cooldown"]
    );
    assert_eq!(
        bad(json!({"op": "failover", "cooldown_secs": 60})),
        Vec::<&str>::new()
    );
    assert_eq!(bad(json!({"op": "fail"})), ["no_fields"]);
    assert_eq!(
        bad(json!({"op": "fail", "status": 200})),
        ["invalid_status"]
    );
    assert_eq!(
        bad(json!({"op": "fail", "status": 600})),
        ["invalid_status"]
    );
    assert_eq!(
        bad(json!({"op": "fail", "code": "Bad Code"})),
        ["invalid_code"]
    );
    // 网关不返回自然语言：错误只带码，由客户端按语言包渲染
    assert_eq!(
        bad(json!({"op": "fail", "message": "natural language is not allowed here"})),
        ["invalid_rule"]
    );
    assert_eq!(
        bad(json!({"op": "fail", "status": 422, "code": "unsupported_param"})),
        Vec::<&str>::new()
    );
    assert_eq!(
        bad(json!({"op": "fail", "status": 422})),
        Vec::<&str>::new()
    );
}

#[test]
fn response_rules_must_name_the_openai_chat_dialect() {
    let ops = json!([{"op": "remove", "paths": ["/x"]}]);
    let rule =
        |select: Value| json!([{"id": "r", "phase": "response", "select": select, "do": ops}]);
    assert_eq!(codes(&rule(json!({}))), ["response_dialect_unsupported"]);
    assert_eq!(
        codes(&rule(json!({"dialect": ["anthropic.messages"]}))),
        ["response_dialect_unsupported"]
    );
    assert_eq!(
        codes(&rule(
            json!({"dialect": ["openai.chat", "openai.responses"]})
        )),
        ["response_dialect_unsupported"]
    );
    assert_eq!(
        codes(&rule(json!({"dialect": ["openai.chat"]}))),
        Vec::<&str>::new()
    );
}

#[test]
fn capability_rules_use_only_the_selectors_known_at_load_time() {
    let rule = |select: Value| json!([{"id": "c", "phase": "capability", "select": select, "do": [{"op": "capability", "facts": {"vision": false}}]}]);
    assert_eq!(
        codes(&rule(
            json!({"model": ["deepseek-*"], "vendor": ["deepseek"], "host": ["api.deepseek.com"]})
        )),
        Vec::<&str>::new()
    );
    for select in [
        json!({"stream": true}),
        json!({"client": ["codex"]}),
        json!({"dialect": ["openai.chat"]}),
        json!({"operation": ["chat"]}),
        json!({"client_model": ["x"]}),
    ] {
        assert_eq!(
            codes(&rule(select.clone())),
            ["selector_not_supported_in_phase"],
            "{select}"
        );
    }
    let unknown = json!([{"id": "c", "phase": "capability", "do": [{"op": "capability", "facts": {"telepathy": true}}]}]);
    assert_eq!(codes(&unknown), ["unknown_fact"]);
    let empty =
        json!([{"id": "c", "phase": "capability", "do": [{"op": "capability", "facts": {}}]}]);
    assert_eq!(codes(&empty), ["no_facts"]);
    // 能力事实只能是布尔
    let stringy = json!([{"id": "c", "phase": "capability", "do": [{"op": "capability", "facts": {"vision": "no"}}]}]);
    assert_eq!(codes(&stringy), ["invalid_rule"]);
}

#[test]
fn response_usage_pricing_and_output_paths_are_graded() {
    let valid = |ops: Value, ack: bool| {
        let mut doc = response_rule(&ops);
        doc[0]["billing_critical_ack"] = json!(ack);
        validate(&doc)
    };
    // 用量 / 计价事实：要 ack，写入还需要 compat.sensitive（动账）
    for op in [
        json!([{"op": "set", "path": "/usage/total_tokens", "value": 1}]),
        json!([{"op": "remove", "paths": ["/usage"]}]),
        json!([{"op": "set", "path": "/model", "value": "x"}]),
        json!([{"op": "remove", "paths": ["/service_tier"]}]),
        json!([{"op": "copy", "from": "/a", "to": "/usage/prompt_tokens"}]),
        json!([{"op": "rename", "from": "/usage", "to": "/u"}]),
        json!([{"op": "usage_map", "slots": {"completion": "total - prompt"}}]),
    ] {
        let without = validate(&response_rule(&op)).expect_err("needs the ack");
        assert!(
            without
                .iter()
                .any(|i| i.code == "billing_critical_ack_required"),
            "{op}: {without:?}"
        );
        let with = valid(op.clone(), true).unwrap_or_else(|issues| panic!("{op}: {issues:?}"));
        assert!(
            !with.sensitive().is_empty(),
            "{op}: 用量事实的改动要报给权限检查"
        );
    }
    // 模型输出本体：写入敏感、移除自由
    let write = valid(
        json!([{"op": "set", "path": "/choices/0/message/content", "value": "x"}]),
        false,
    )
    .expect("valid");
    assert_eq!(write.sensitive().len(), 1);
    let remove = valid(json!([{"op": "remove", "paths": ["/choices/0/message/content", "/choices/0/message/tool_calls"]}]), false).expect("valid");
    assert!(remove.sensitive().is_empty(), "只删字段不该要求敏感权限");
    // 推理字段别名、finish_reason、厂商私有字段：自由
    let free = valid(json!([
        {"op": "each", "path": "/choices", "do": [
            {"op": "copy", "from": "/message/reasoning", "to": "/message/reasoning_content", "mode": "if_absent"},
            {"op": "map_value", "path": "/finish_reason", "table": {"eos": "stop"}}
        ]},
        {"op": "remove", "paths": ["/system_fingerprint"]}
    ]), false).expect("valid");
    assert!(free.sensitive().is_empty(), "{:?}", free.sensitive());
}

#[test]
fn usage_map_expressions_are_checked_at_write_time() {
    let doc = |slots: Value| {
        let mut rule = response_rule(&json!([{"op": "usage_map", "slots": slots}]));
        rule[0]["billing_critical_ack"] = json!(true);
        rule
    };
    assert_eq!(
        codes(&doc(json!({"completion": "total - prompt"}))),
        Vec::<&str>::new()
    );
    assert_eq!(
        codes(&doc(
            json!({"completion": "max(0, total - prompt - reasoning)", "cache_read": "cache_read"})
        )),
        Vec::<&str>::new()
    );
    assert_eq!(codes(&doc(json!({}))), ["no_slots"]);
    assert_eq!(codes(&doc(json!({"tokens": "1"}))), ["unknown_slot"]);
    assert_eq!(
        codes(&doc(json!({"completion": "total * 2"}))),
        ["invalid_expr"]
    );
    assert_eq!(
        codes(&doc(json!({"completion": "total / 2"}))),
        ["invalid_expr"]
    );
    assert_eq!(codes(&doc(json!({"completion": "1.5"}))), ["invalid_expr"]);
    assert_eq!(codes(&doc(json!({"completion": "nope"}))), ["invalid_expr"]);
    assert_eq!(codes(&doc(json!({"completion": ""}))), ["invalid_expr"]);
    assert_eq!(codes(&doc(json!({"completion": 5}))), ["invalid_rule"]);
}

#[test]
fn each_is_bounded_and_graded_by_the_absolute_path() {
    let each = |path: &str, ops: Value| json!([{"id": "t", "do": [{"op": "each", "path": path, "do": ops}]}]);
    let ok = json!([{"op": "remove", "paths": ["/cache_control"]}]);
    assert_eq!(codes(&each("/messages", ok.clone())), Vec::<&str>::new());
    assert_eq!(codes(&each("/messages/*", ok.clone())), ["wildcard_leaf"]);
    assert_eq!(codes(&each("/messages", json!([]))), ["no_ops"]);
    // 子操作的路径相对元素，分级按拼起来的绝对路径：往 /messages/*/content 写是敏感的
    let write = validate(&each(
        "/messages",
        json!([{"op": "set", "path": "/content", "value": "x"}]),
    ))
    .expect("valid");
    assert_eq!(write.sensitive().len(), 1);
    let nested_remove = validate(&each(
        "/messages",
        json!([{"op": "remove", "paths": ["/content"]}]),
    ))
    .expect("valid");
    assert!(
        nested_remove.sensitive().is_empty(),
        "只删字段不该要求敏感权限"
    );
    // 通配的绝对路径保守地当作可能落到计费关键项上
    assert_eq!(
        codes(&each(
            "/stream_options",
            json!([{"op": "remove", "paths": ["/include_usage"]}])
        )),
        ["billing_critical_ack_required"]
    );
    // 不能放进 each 的操作
    for op in [
        json!({"op": "keep_only", "paths": ["/a"]}),
        json!({"op": "role_map", "from": "a", "to": "b"}),
    ] {
        assert_eq!(
            codes(&each("/messages", json!([op]))),
            ["op_not_allowed_in_each"]
        );
    }
    // 嵌套至多两层
    let two = json!([{"op": "each", "path": "/b", "do": [{"op": "remove", "paths": ["/c"]}]}]);
    assert_eq!(codes(&each("/a", two.clone())), Vec::<&str>::new());
    let three = json!([{"op": "each", "path": "/b", "do": [{"op": "each", "path": "/c", "do": [{"op": "remove", "paths": ["/d"]}]}]}]);
    assert_eq!(codes(&each("/a", three)), ["each_too_deep"]);
    // 子操作也算进 32 个操作的总数
    let many: Vec<Value> = (0..40)
        .map(|i| json!({"op": "remove", "paths": [format!("/f{i}")]}))
        .collect();
    assert_eq!(codes(&each("/a", Value::Array(many))), ["too_many_ops"]);
    // 子操作写错路径照样报
    assert_eq!(
        codes(&each(
            "/a",
            json!([{"op": "set", "path": "no-slash", "value": 1}])
        )),
        ["invalid_path"]
    );
    assert_eq!(
        codes(&each("/a", json!([{"op": "teleport"}]))),
        ["unknown_op"]
    );
}

// ----------------------------------------------------------------- behaviour

fn run_response(effective: &EffectiveRules, doc: &Value) -> (Value, Report) {
    let mut out = doc.clone();
    let report = effective.apply_response_value(&ctx(), &mut out);
    (out, report)
}

#[test]
fn a_response_rule_can_alias_reasoning_and_map_finish_reasons() {
    let effective = rules(&response_rule(&json!([
        {"op": "each", "path": "/choices", "do": [
            {"op": "copy", "from": "/message/reasoning", "to": "/message/reasoning_content", "mode": "if_absent"},
            {"op": "map_value", "path": "/finish_reason", "table": {"eos": "stop", "max": "length"}}
        ]}
    ])));
    let doc = json!({"choices": [
        {"message": {"role": "assistant", "content": "hi", "reasoning": "think"}, "finish_reason": "eos"},
        {"message": {"role": "assistant", "content": "yo", "reasoning": "other", "reasoning_content": "kept"}, "finish_reason": "max"},
        {"message": {"role": "assistant", "content": "z"}, "finish_reason": "stop"}
    ]});
    let (after, report) = run_response(&effective, &doc);
    assert_eq!(after["choices"][0]["message"]["reasoning_content"], "think");
    assert_eq!(
        after["choices"][0]["message"]["reasoning"], "think",
        "copy 不删来源"
    );
    assert_eq!(after["choices"][0]["finish_reason"], "stop");
    assert_eq!(
        after["choices"][1]["message"]["reasoning_content"], "kept",
        "if_absent 不覆盖"
    );
    assert_eq!(after["choices"][1]["finish_reason"], "length");
    assert!(
        after["choices"][2]["message"]
            .get("reasoning_content")
            .is_none()
    );
    assert_eq!(report.fired.len(), 1);
    assert_eq!(report.fired[0].rule, "r");
    assert_eq!(
        report.fired[0].changes, 3,
        "一处别名 + 两处映射，真正改变的节点才算"
    );
    // 幂等：应用两次等于一次
    let (again, report) = run_response(&effective, &after);
    assert_eq!(again, after);
    assert!(report.fired.is_empty(), "{:?}", report.fired);
}

#[test]
fn streaming_deltas_are_just_documents_too() {
    let effective = rules(&response_rule(&json!([
        {"op": "each", "path": "/choices", "do": [
            {"op": "rename", "from": "/delta/reasoning", "to": "/delta/reasoning_content"}
        ]}
    ])));
    let chunk = json!({"object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"reasoning": "x"}}]});
    let (after, report) = run_response(&effective, &chunk);
    assert_eq!(
        after["choices"][0]["delta"],
        json!({"reasoning_content": "x"})
    );
    assert_eq!(report.fired.len(), 1);
    // 没有 choices 的帧（带 usage 的最后一帧）原样放过
    let tail = json!({"choices": [], "usage": {"total_tokens": 3}});
    let (same, report) = run_response(&effective, &tail);
    assert_eq!(same, tail);
    assert!(report.fired.is_empty() && report.skipped.is_empty());
}

#[test]
fn usage_map_recomputes_slots_from_the_reported_usage() {
    let mut doc = response_rule(&json!([
        {"op": "usage_map", "slots": {"completion": "total - prompt"}}
    ]));
    doc[0]["billing_critical_ack"] = json!(true);
    let effective = rules(&doc);
    // xAI：completion_tokens 不含推理 token，total 才是全部
    let usage = json!({"usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 25,
        "completion_tokens_details": {"reasoning_tokens": 10}}});
    let (after, report) = run_response(&effective, &usage);
    assert_eq!(after["usage"]["completion_tokens"], 15);
    assert_eq!(after["usage"]["prompt_tokens"], 10);
    assert_eq!(after["usage"]["total_tokens"], 25);
    assert_eq!(
        after["usage"]["completion_tokens_details"]["reasoning_tokens"],
        10
    );
    assert_eq!(report.fired[0].changes, 1);
    // 幂等
    let (again, report) = run_response(&effective, &after);
    assert_eq!(again, after);
    assert!(report.fired.is_empty(), "{:?}", report.fired);
    // 上游已经把推理算进 completion：total - prompt 与原值相同，什么都不动
    let consistent =
        json!({"usage": {"prompt_tokens": 10, "completion_tokens": 15, "total_tokens": 25}});
    let (same, report) = run_response(&effective, &consistent);
    assert_eq!(same, consistent);
    assert!(report.fired.is_empty(), "{:?}", report.fired);
    // 读不到的槽位：整个槽位不写（失败开放），不会写出 0
    for partial in [
        json!({"usage": {"prompt_tokens": 10}}),
        json!({"usage": {"total_tokens": 25}}),
        json!({"usage": {"prompt_tokens": 1.5, "total_tokens": 25}}),
        json!({"usage": null}),
        json!({"id": "no usage at all"}),
    ] {
        let (same, report) = run_response(&effective, &partial);
        assert_eq!(same, partial, "{partial}");
        assert!(report.fired.is_empty(), "{partial}");
    }
}

#[test]
fn usage_map_reads_the_usage_as_reported_not_as_half_rewritten() {
    // 所有槽位的新值都先按应用前的用量算出来再一起写：互换两个槽位真的是互换
    let mut doc = response_rule(&json!([
        {"op": "usage_map", "slots": {"prompt": "completion", "completion": "prompt"}}
    ]));
    doc[0]["billing_critical_ack"] = json!(true);
    let effective = rules(&doc);
    let (after, report) = run_response(
        &effective,
        &json!({"usage": {"prompt_tokens": 10, "completion_tokens": 5}}),
    );
    assert_eq!(
        after["usage"],
        json!({"prompt_tokens": 5, "completion_tokens": 10})
    );
    assert_eq!(report.fired[0].changes, 2);
}

#[test]
fn usage_map_can_create_missing_detail_objects() {
    let mut doc =
        response_rule(&json!([{"op": "usage_map", "slots": {"cache_read": "prompt - 40"}}]));
    doc[0]["billing_critical_ack"] = json!(true);
    let effective = rules(&doc);
    let (after, _) = run_response(&effective, &json!({"usage": {"prompt_tokens": 100}}));
    assert_eq!(after["usage"]["prompt_tokens_details"]["cached_tokens"], 60);
}

#[test]
fn response_application_on_bytes_skips_parsing_without_rules_and_reports_bad_bodies() {
    let none = resolve(Vec::new());
    let outcome = none.apply_response(&ctx(), b"not json at all");
    assert!(outcome.body.is_none() && outcome.report.skipped.is_empty());

    let effective = rules(&response_rule(
        &json!([{"op": "set", "path": "/note", "value": "x"}]),
    ));
    let outcome = effective.apply_response(&ctx(), b"not json at all");
    assert!(outcome.body.is_none());
    assert_eq!(outcome.report.skipped.len(), 1);
    assert_eq!(outcome.report.skipped[0].reason, "body_not_json");

    let outcome = effective.apply_response(&ctx(), br#"{"id":"1"}"#);
    let body: Value = serde_json::from_slice(&outcome.body.expect("changed")).expect("json");
    assert_eq!(body, json!({"id": "1", "note": "x"}));

    let outcome = effective.apply_response(&ctx(), br#"{"id":"1","note":"x"}"#);
    assert!(outcome.body.is_none(), "没有变化就沿用原字节");

    // 不是对象的 JSON 记 skipped，不 panic
    let outcome = effective.apply_response(&ctx(), b"[1,2]");
    assert!(outcome.body.is_none());
    assert_eq!(outcome.report.skipped[0].reason, "body_not_object");
}

#[test]
fn response_selectors_and_layers_work_like_request_ones() {
    let vendor = json!([{"id": "alias", "phase": "response", "select": {"dialect": ["openai.chat"], "model": ["grok-*"]},
        "do": [{"op": "set", "path": "/via", "value": "vendor"}]}]);
    let channel = json!([{"id": "alias", "phase": "response", "select": {"dialect": ["openai.chat"]},
        "do": [{"op": "set", "path": "/via", "value": "channel"}]}]);
    let effective = resolve(vec![
        layer(Layer::Vendor, "builtin:xai", &vendor),
        layer(Layer::Channel, "channel#7@v1", &channel),
    ]);
    let (after, report) = run_response(&effective, &json!({}));
    assert_eq!(after["via"], "channel", "同 id 后面的层覆盖前面的");
    assert_eq!(report.fired[0].layer, Layer::Channel);
    let mut other = ctx();
    other.model = "gpt-4o";
    let mut doc = json!({});
    let report = effective.apply_response_value(&other, &mut doc);
    assert_eq!(doc["via"], "channel");
    assert_eq!(report.fired.len(), 1);
    // 流式开关也能选
    let streaming_only = rules(
        &json!([{"id": "s", "phase": "response", "select": {"dialect": ["openai.chat"], "stream": true},
        "do": [{"op": "set", "path": "/s", "value": 1}]}]),
    );
    assert!(!streaming_only.selects_response(&ctx()));
    let mut streaming = ctx();
    streaming.stream = true;
    assert!(streaming_only.selects_response(&streaming));
}

#[test]
fn phases_do_not_leak_into_each_other() {
    let doc = json!([
        {"id": "req", "do": [{"op": "set", "path": "/from_request", "value": 1}]},
        {"id": "resp", "phase": "response", "select": {"dialect": ["openai.chat"]}, "do": [{"op": "set", "path": "/from_response", "value": 1}]},
        {"id": "cap", "phase": "capability", "do": [{"op": "capability", "facts": {"vision": false}}]},
        {"id": "err", "phase": "error", "when": {"status": [404]}, "do": [{"op": "failover"}]}
    ]);
    let effective = rules(&doc);
    let request = effective.apply_request(&ctx(), b"{}").expect("apply");
    let body: Value = serde_json::from_slice(&request.body.expect("changed")).expect("json");
    assert_eq!(body, json!({"from_request": 1}));
    let response = effective.apply_response(&ctx(), b"{}");
    let body: Value = serde_json::from_slice(&response.body.expect("changed")).expect("json");
    assert_eq!(body, json!({"from_response": 1}));
    assert_eq!(effective.capabilities(&ctx()).len(), 1);
    assert!(effective.react(&ctx(), 404, b"").is_some());
    assert!(effective.has_phase(Phase::Response) && effective.has_phase(Phase::Error));
    assert!(!rules(&json!([])).has_phase(Phase::Request));
    // manages_top_level 只看请求阶段
    assert!(!effective.manages_top_level(&ctx(), "from_response"));
}

#[test]
fn capabilities_merge_later_layers_over_earlier_ones() {
    let vendor = json!([
        {"id": "no-vision", "phase": "capability", "select": {"vendor": ["xai"]}, "do": [{"op": "capability", "facts": {"vision": false, "tools": true}}]},
        {"id": "small-no-tools", "phase": "capability", "select": {"model": ["grok-mini*"]}, "do": [{"op": "capability", "facts": {"tools": false}}]}
    ]);
    let channel = json!([
        {"id": "vision-again", "phase": "capability", "do": [{"op": "capability", "facts": {"vision": true}}]}
    ]);
    let effective = resolve(vec![
        layer(Layer::Vendor, "builtin:xai", &vendor),
        layer(Layer::Channel, "channel#1@v1", &channel),
    ]);
    let facts = effective.capabilities(&ctx());
    assert_eq!(facts.get("vision"), Some(&true), "渠道层压过厂商层");
    assert_eq!(facts.get("tools"), Some(&true));
    let mut mini = ctx();
    mini.model = "grok-mini-fast";
    let facts = effective.capabilities(&mini);
    assert_eq!(facts.get("tools"), Some(&false));
    let mut other_vendor = ctx();
    other_vendor.vendor = Some("builtin:deepseek");
    assert_eq!(
        effective
            .capabilities(&other_vendor)
            .into_iter()
            .collect::<Vec<_>>(),
        [("vision".to_owned(), true)]
    );
    assert!(resolve(Vec::new()).capabilities(&ctx()).is_empty());
}

#[test]
fn error_rules_match_on_status_code_and_body() {
    let effective = rules(&error_rule(
        &json!({"status": [400, 422], "code": ["Unsupported_Parameter"], "body_contains": ["USER"]}),
        &json!({"op": "failover"}),
    ));
    let body =
        br#"{"error":{"code":"unsupported_parameter","message":"the user field is not allowed"}}"#;
    assert!(
        effective.react(&ctx(), 400, body).is_some(),
        "字段之间 AND，全满足"
    );
    assert!(effective.react(&ctx(), 422, body).is_some(), "状态码是 OR");
    assert!(
        effective.react(&ctx(), 404, body).is_none(),
        "状态码不在列表里"
    );
    assert!(
        effective
            .react(
                &ctx(),
                400,
                br#"{"error":{"code":"other","message":"the user field"}}"#
            )
            .is_none(),
        "码不符"
    );
    assert!(
        effective
            .react(
                &ctx(),
                400,
                br#"{"error":{"code":"unsupported_parameter","message":"nope"}}"#
            )
            .is_none(),
        "子串不符"
    );
    // 码可以出现在 error.type、顶层 code / type，也可以是数字
    let by_type = rules(&error_rule(
        &json!({"code": ["invalid_request_error"]}),
        &json!({"op": "failover"}),
    ));
    assert!(
        by_type
            .react(
                &ctx(),
                400,
                br#"{"error":{"type":"invalid_request_error"}}"#
            )
            .is_some()
    );
    assert!(
        by_type
            .react(&ctx(), 400, br#"{"type":"invalid_request_error"}"#)
            .is_some()
    );
    assert!(
        by_type
            .react(&ctx(), 400, br#"{"code":"INVALID_REQUEST_ERROR"}"#)
            .is_some()
    );
    let numeric = rules(&error_rule(
        &json!({"code": ["1210"]}),
        &json!({"op": "failover"}),
    ));
    assert!(
        numeric
            .react(&ctx(), 400, br#"{"error":{"code":1210}}"#)
            .is_some()
    );
    assert!(numeric.react(&ctx(), 400, b"not json").is_none());
    // 只看错误体前 8 KiB 的子串
    let far = rules(&error_rule(
        &json!({"body_contains": ["needle"]}),
        &json!({"op": "failover"}),
    ));
    let mut late = vec![b' '; 9000];
    late.extend_from_slice(b"needle");
    assert!(far.react(&ctx(), 400, &late).is_none());
    let mut early = b"needle".to_vec();
    early.extend(vec![b' '; 9000]);
    assert!(far.react(&ctx(), 400, &early).is_some());
}

#[test]
fn react_picks_the_first_matching_rule_in_execution_order() {
    let vendor = json!([
        {"id": "a", "phase": "error", "when": {"status": [404]}, "do": [{"op": "failover"}]}
    ]);
    let channel = json!([
        {"id": "b", "phase": "error", "when": {"status": [404]}, "do": [{"op": "fail", "status": 410, "code": "gone"}]},
        {"id": "c", "phase": "error", "when": {"status": [409]}, "do": [{"op": "failover", "cooldown_secs": 30}]}
    ]);
    let effective = resolve(vec![
        layer(Layer::Vendor, "builtin:xai", &vendor),
        layer(Layer::Channel, "channel#1@v1", &channel),
    ]);
    let reaction = effective
        .react(&ctx(), 404, b"")
        .expect("a vendor rule fires first");
    assert_eq!(reaction.rule, "a");
    assert_eq!(reaction.layer, Layer::Vendor);
    assert!(matches!(
        reaction.action,
        Action::Failover {
            cooldown_secs: None
        }
    ));
    assert_eq!(
        reaction.summary(),
        json!({"rule": "a", "layer": "vendor", "action": "failover"})
    );
    let reaction = effective.react(&ctx(), 409, b"").expect("channel rule");
    assert!(matches!(
        reaction.action,
        Action::Failover {
            cooldown_secs: Some(30)
        }
    ));
    assert!(effective.react(&ctx(), 422, b"").is_none());
    // 渠道层同 id 覆盖：把厂商的 a 换成 fail
    let override_a = json!([{"id": "a", "phase": "error", "when": {"status": [404]}, "do": [{"op": "fail", "status": 410, "code": "gone"}]}]);
    let effective = resolve(vec![
        layer(Layer::Vendor, "builtin:xai", &vendor),
        layer(Layer::Channel, "channel#1@v1", &override_a),
    ]);
    let reaction = effective.react(&ctx(), 404, b"").expect("overridden");
    match reaction.action {
        Action::Fail { status, code } => {
            assert_eq!((status, code), (Some(410), Some("gone")));
        }
        other => panic!("{other:?}"),
    }
    // 选择器也起作用
    let mut other = ctx();
    other.vendor = Some("builtin:deepseek");
    let scoped = rules(
        &json!([{"id": "s", "phase": "error", "select": {"vendor": ["xai"]}, "when": {"status": [404]}, "do": [{"op": "failover"}]}]),
    );
    assert!(scoped.react(&ctx(), 404, b"").is_some());
    assert!(scoped.react(&other, 404, b"").is_none());
}

#[test]
fn a_retry_mutation_edits_the_request_and_refuses_pointless_resends() {
    let effective = rules(&error_rule(
        &json!({"status": [400], "body_contains": ["unsupported parameter"]}),
        &json!({"op": "retry_same", "mutate": [
            {"op": "remove", "paths": ["/user", "/logit_bias"]},
            {"op": "rename", "from": "/max_tokens", "to": "/max_completion_tokens", "mode": "if_absent"}
        ], "max": 2}),
    ));
    let reaction = effective
        .react(
            &ctx(),
            400,
            br#"{"error":{"message":"Unsupported parameter: user"}}"#,
        )
        .expect("fires");
    let Action::RetrySame { mutation, max } = reaction.action else {
        panic!("retry_same expected");
    };
    assert_eq!(max, 2);
    let request = br#"{"model":"m","user":"u","logit_bias":{"1":2},"max_tokens":9,"messages":[]}"#;
    let resent: Value =
        serde_json::from_slice(&mutation.apply(request).expect("changed")).expect("json");
    assert_eq!(
        resent,
        json!({"model": "m", "max_completion_tokens": 9, "messages": []})
    );
    // 改完发现没有变化（该删的字段本来就没有）：放弃重发，免得同一个请求再得到同一个错误
    assert!(mutation.apply(br#"{"model":"m","messages":[]}"#).is_none());
    assert!(mutation.apply(b"not json").is_none());
    assert!(mutation.apply(b"[1]").is_none());
    assert_eq!(reaction.summary()["action"], "retry_same");
}

#[test]
fn reactions_lists_every_matching_rule_in_order_so_the_gateway_can_skip_exhausted_ones() {
    let effective = rules(&json!([
        {"id": "first", "phase": "error", "when": {"status": [400]}, "do": [{"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/a"]}]}]},
        {"id": "second", "phase": "error", "when": {"status": [400]}, "do": [{"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/b"]}]}]},
        {"id": "other", "phase": "error", "when": {"status": [404]}, "do": [{"op": "failover"}]}
    ]));
    let ids: Vec<&str> = effective
        .reactions(&ctx(), 400, b"")
        .iter()
        .map(|r| r.rule)
        .collect();
    assert_eq!(ids, ["first", "second"]);
    assert_eq!(
        effective.react(&ctx(), 400, b"").map(|r| r.rule),
        Some("first")
    );
    assert!(effective.reactions(&ctx(), 422, b"").is_empty());
}

#[test]
fn the_default_retry_budget_is_one() {
    let effective = rules(&error_rule(
        &json!({"status": [400]}),
        &json!({"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}]}),
    ));
    let reaction = effective.react(&ctx(), 400, b"").expect("fires");
    assert!(matches!(reaction.action, Action::RetrySame { max: 1, .. }));
}
