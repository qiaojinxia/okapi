//! 规则的自描述目录：阶段、操作、选择器、用量槽位、能力事实、受保护路径与限额。
//!
//! 蓝图（节点库）、规则编辑表单和 MCP 的 `compat_catalog` 都读这一份，所以 Rust 新增一个操作，
//! 它们自动多一个节点 / 一个表单项 / 一条说明，不会各抄一份再漂移。说明文字是英文的稳定描述
//! （给 AI 和文档用）；界面按操作名走自己的语言包。

use crate::expr::Slot;
use crate::protect::{self, PathClass};
use crate::rule::{CAPABILITY_FACTS, OP_NAMES, Phase, SCHEMA_VERSION};
use crate::select::{DIALECTS, OPERATIONS};
use crate::validate::{
    MAX_COOLDOWN_SECS, MAX_DOC_BYTES, MAX_EACH_DEPTH, MAX_OPS, MAX_PATHS_PER_OP, MAX_RETRY,
    MAX_RULES, MAX_WHEN_ITEMS,
};
use serde_json::{Value, json};

struct Field {
    name: &'static str,
    /// `pointer` `pointer[]` `json` `number` `string` `string[]` `table` `mode` `ops` `slots` `facts` `int` `bool`
    kind: &'static str,
    required: bool,
    note: &'static str,
}

const fn field(
    name: &'static str,
    kind: &'static str,
    required: bool,
    note: &'static str,
) -> Field {
    Field {
        name,
        kind,
        required,
        note,
    }
}

struct OpInfo {
    name: &'static str,
    phases: &'static [Phase],
    summary: &'static str,
    fields: &'static [Field],
}

const JSON_PHASES: &[Phase] = &[Phase::Request, Phase::Response];

const OPS: &[OpInfo] = &[
    OpInfo {
        name: "remove",
        phases: JSON_PHASES,
        summary: "Delete the given paths. Missing paths are ignored.",
        fields: &[field(
            "paths",
            "pointer[]",
            true,
            "RFC 6901 pointers; `*` matches any array element or object member (not as the last segment)",
        )],
    },
    OpInfo {
        name: "keep_only",
        phases: &[Phase::Request],
        summary: "Allow-list: keep only the listed paths. Protected fields and the usage option are always kept.",
        fields: &[field("paths", "pointer[]", true, "paths to keep")],
    },
    OpInfo {
        name: "set",
        phases: JSON_PHASES,
        summary: "Write a value at a path, creating missing parent objects.",
        fields: &[
            field("path", "pointer", true, "target"),
            field(
                "value",
                "json",
                true,
                "any JSON value; numbers keep their spelling",
            ),
            field("mode", "mode", false, "`always` (default) or `if_absent`"),
        ],
    },
    OpInfo {
        name: "rename",
        phases: JSON_PHASES,
        summary: "Move a value to another path. A missing source does nothing.",
        fields: &[
            field(
                "from",
                "pointer",
                true,
                "source (no wildcard; use `each` for batches)",
            ),
            field("to", "pointer", true, "target"),
            field(
                "mode",
                "mode",
                false,
                "`if_absent` leaves everything untouched when the target exists",
            ),
        ],
    },
    OpInfo {
        name: "copy",
        phases: JSON_PHASES,
        summary: "Copy a value to another path; the source stays.",
        fields: &[
            field("from", "pointer", true, "source"),
            field("to", "pointer", true, "target"),
            field("mode", "mode", false, "`always` (default) or `if_absent`"),
        ],
    },
    OpInfo {
        name: "clamp",
        phases: JSON_PHASES,
        summary: "Pull a number into [min, max] using exact decimal comparison. Integers stay integers.",
        fields: &[
            field("path", "pointer", true, "numeric field"),
            field("min", "number", false, "lower bound"),
            field(
                "max",
                "number",
                false,
                "upper bound (at least one bound is required)",
            ),
        ],
    },
    OpInfo {
        name: "map_value",
        phases: JSON_PHASES,
        summary: "Replace string values through a lookup table.",
        fields: &[
            field("path", "pointer", true, "string field"),
            field("table", "table", true, "{from: to}, 1 to 256 entries"),
        ],
    },
    OpInfo {
        name: "role_map",
        phases: &[Phase::Request],
        summary: "Rename a conversation role in `/messages/*/role` and `/input/*/role`.",
        fields: &[
            field("from", "string", true, "role to replace"),
            field("to", "string", true, "replacement role"),
        ],
    },
    OpInfo {
        name: "each",
        phases: JSON_PHASES,
        summary: "Run sub-operations on every element of an array (or object). Sub-operation paths are relative to the element.",
        fields: &[
            field(
                "path",
                "pointer",
                true,
                "the array or object itself (not `*`)",
            ),
            field(
                "do",
                "ops",
                true,
                "sub-operations; no keep_only / role_map / usage_map; nesting is limited",
            ),
        ],
    },
    OpInfo {
        name: "usage_map",
        phases: &[Phase::Response],
        summary: "Rewrite usage slots from integer expressions over the reported usage. Billing-critical: needs billing_critical_ack.",
        fields: &[field(
            "slots",
            "slots",
            true,
            "{slot: expression}; expressions use slot names, integers, + - ( ), max(a, b), min(a, b)",
        )],
    },
    OpInfo {
        name: "capability",
        phases: &[Phase::Capability],
        summary: "Declare capability facts for matching channel candidates. The channel's own declarations win.",
        fields: &[field("facts", "facts", true, "{fact: bool}")],
    },
    OpInfo {
        name: "retry_same",
        phases: &[Phase::Error],
        summary: "Edit the request body and resend it on the same candidate (no failover, no key penalty).",
        fields: &[
            field(
                "mutate",
                "ops",
                true,
                "request-phase operations applied to the upstream request body",
            ),
            field(
                "max",
                "int",
                false,
                "resends per request, 1 to 3 (default 1)",
            ),
        ],
    },
    OpInfo {
        name: "failover",
        phases: &[Phase::Error],
        summary: "Treat this upstream error as a reason to try the next candidate.",
        fields: &[field(
            "cooldown_secs",
            "int",
            false,
            "also cool this key down for that long",
        )],
    },
    OpInfo {
        name: "fail",
        phases: &[Phase::Error],
        summary: "Rewrite the error returned to the client; no retry. With `code` the client gets the gateway's stable error envelope (code only, localised by the client); with only `status` the upstream body is kept.",
        fields: &[
            field("status", "int", false, "400 to 599"),
            field(
                "code",
                "string",
                false,
                "error code, lowercase letters, digits and . _ -",
            ),
        ],
    },
];

fn phase_json(phase: Phase) -> Value {
    let (summary, note) = match phase {
        Phase::RequestPre => ("Before dialect conversion.", "reserved"),
        Phase::Request => (
            "The JSON sent upstream, after dialect conversion.",
            "runs on every attempt; rules must be idempotent",
        ),
        Phase::Wire => ("Headers, endpoint and authentication.", "reserved"),
        Phase::Response => (
            "The upstream response before it is converted back to the client's dialect.",
            "OpenAI chat dialect upstreams only; streaming runs per frame",
        ),
        Phase::Error => (
            "A non-retryable upstream HTTP error.",
            "one action per rule; needs `when`; only statuses the gateway's retry matrix leaves alone",
        ),
        Phase::Capability => (
            "Capability facts merged into a channel candidate when it is loaded.",
            "selectors limited to vendor / host / model",
        ),
    };
    json!({
        "id": phase.as_str(), "enabled": phase.enabled(), "summary": summary, "note": note,
        "ops": OPS.iter().filter(|op| op.phases.contains(&phase)).map(|op| op.name).collect::<Vec<_>>(),
    })
}

fn registry_json(phase: Phase) -> Value {
    let class = |class: PathClass| match class {
        PathClass::Free => "free",
        PathClass::Sensitive => "sensitive",
        PathClass::BillingCritical => "billing_critical",
        PathClass::Protected => "protected",
    };
    Value::Array(
        protect::describe(phase)
            .into_iter()
            .map(|entry| {
                json!({
                    "path": entry.path, "class": class(entry.class),
                    "node_only": entry.node_only,
                })
            })
            .collect(),
    )
}

/// 整份目录（JSON）。
#[must_use]
pub fn catalog() -> Value {
    debug_assert_eq!(OPS.len(), OP_NAMES.len());
    let ops: Vec<Value> = OPS
        .iter()
        .map(|op| {
            json!({
                "name": op.name,
                "phases": op.phases.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
                "summary": op.summary,
                "fields": op.fields.iter().map(|f| json!({
                    "name": f.name, "type": f.kind, "required": f.required, "note": f.note,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "schema_version": SCHEMA_VERSION,
        "phases": Phase::ALL.iter().map(|p| phase_json(*p)).collect::<Vec<_>>(),
        "ops": ops,
        "selector": {
            "fields": [
                {"name": "vendor", "type": "string[]", "note": "builtin:<id> or custom:<slug>; a bare id means builtin"},
                {"name": "host", "type": "string[]", "note": "host of the channel's api_base; `*` globs"},
                {"name": "model", "type": "string[]", "note": "the upstream model after the channel's model mapping; `*` globs"},
                {"name": "client_model", "type": "string[]", "note": "the model name the client asked for"},
                {"name": "dialect", "type": "string[]", "values": DIALECTS},
                {"name": "operation", "type": "string[]", "values": OPERATIONS},
                {"name": "stream", "type": "bool"},
                {"name": "client", "type": "string[]", "note": "client family recognised from the user agent, e.g. claude-code, codex"},
            ],
            "capability_fields": ["vendor", "host", "model"],
            "response_dialects": ["openai.chat"],
        },
        "when": {
            "fields": [
                {"name": "status", "type": "int[]", "note": "400 to 499 except 401 402 403 408 429"},
                {"name": "code", "type": "string[]", "note": "error.code / error.type / top-level code or type of a JSON error body"},
                {"name": "body_contains", "type": "string[]", "note": "case-insensitive substring of the first 8 KiB of the error body"},
            ],
            "semantics": "all given fields must match; within a field any value matches",
        },
        "usage_slots": Slot::ALL.iter().map(|s| json!({"name": s.name(), "pointer": s.pointer()})).collect::<Vec<_>>(),
        "capability_facts": CAPABILITY_FACTS,
        "layers": [
            {"id": "vendor", "rank": 100}, {"id": "profile", "rank": 200},
            {"id": "channel", "rank": 300}, {"id": "guard", "rank": 900},
        ],
        "protected_paths": {
            "request": registry_json(Phase::Request),
            "response": registry_json(Phase::Response),
            "classes": {
                "protected": "never writable or removable",
                "billing_critical": "needs billing_critical_ack on the rule (and compat.sensitive in the response phase)",
                "sensitive": "writes need compat.sensitive; removals are free",
                "free": "anything else",
            },
        },
        "limits": {
            "max_rules": MAX_RULES, "max_ops_per_rule": MAX_OPS, "max_paths_per_op": MAX_PATHS_PER_OP,
            "max_doc_bytes": MAX_DOC_BYTES, "max_each_depth": MAX_EACH_DEPTH, "max_when_items": MAX_WHEN_ITEMS,
            "max_retry": MAX_RETRY, "max_cooldown_secs": MAX_COOLDOWN_SECS,
        },
        "recipes": recipes(),
    })
}

/// 常见需求的现成写法（给 AI 和新手抄的起点，也是目录的一部分，由测试保证每一条都能通过校验）。
#[must_use]
pub fn recipes() -> Value {
    json!([
        {
            "id": "strip-fields",
            "title": "Remove request fields the upstream rejects",
            "rule": {"id": "strip-fields", "do": [{"op": "remove", "paths": ["/user", "/logit_bias"]}]},
        },
        {
            "id": "fixed-temperature",
            "title": "Force a fixed temperature for one model",
            "rule": {"id": "fixed-temperature", "select": {"model": ["kimi-k2.6"]},
                     "do": [{"op": "clamp", "path": "/temperature", "min": 1, "max": 1}]},
        },
        {
            "id": "developer-role",
            "title": "Upstream rejects the developer role",
            "rule": {"id": "developer-role", "select": {"dialect": ["openai.chat"]},
                     "do": [{"op": "role_map", "from": "developer", "to": "system"}]},
        },
        {
            "id": "rename-token-limit",
            "title": "Use max_completion_tokens when only max_tokens is sent",
            "rule": {"id": "rename-token-limit", "select": {"model": ["o*"]},
                     "do": [{"op": "rename", "from": "/max_tokens", "to": "/max_completion_tokens", "mode": "if_absent"}]},
        },
        {
            "id": "alias-reasoning",
            "title": "Expose `reasoning` as `reasoning_content` in responses",
            "rule": {"id": "alias-reasoning", "phase": "response", "select": {"dialect": ["openai.chat"]},
                     "do": [{"op": "each", "path": "/choices", "do": [
                         {"op": "copy", "from": "/message/reasoning", "to": "/message/reasoning_content", "mode": "if_absent"},
                         {"op": "copy", "from": "/delta/reasoning", "to": "/delta/reasoning_content", "mode": "if_absent"}]}]},
        },
        {
            "id": "completion-includes-reasoning",
            "title": "Upstream leaves reasoning tokens out of completion_tokens",
            "rule": {"id": "completion-includes-reasoning", "phase": "response", "select": {"dialect": ["openai.chat"]},
                     "billing_critical_ack": true,
                     "do": [{"op": "usage_map", "slots": {"completion": "total - prompt"}}]},
        },
        {
            "id": "drop-unsupported-param",
            "title": "On 'unsupported parameter' errors, resend once without the field",
            "rule": {"id": "drop-unsupported-param", "phase": "error",
                     "when": {"status": [400], "body_contains": ["unsupported parameter"]},
                     "do": [{"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}]}]},
        },
        {
            "id": "model-not-on-this-account",
            "title": "Try the next channel when this account lacks the model",
            "rule": {"id": "model-not-on-this-account", "phase": "error",
                     "when": {"status": [404], "code": ["model_not_found"]},
                     "do": [{"op": "failover"}]},
        },
        {
            "id": "no-vision",
            "title": "This model cannot take images",
            "rule": {"id": "no-vision", "phase": "capability", "select": {"model": ["deepseek-*"]},
                     "do": [{"op": "capability", "facts": {"vision": false}}]},
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::Op;

    #[test]
    fn the_table_covers_every_op_in_order() {
        let names: Vec<&str> = OPS.iter().map(|op| op.name).collect();
        assert_eq!(names, OP_NAMES);
    }

    #[test]
    fn the_phase_columns_agree_with_the_validator() {
        let sample = |name: &str| -> Op {
            serde_json::from_value(match name {
                "remove" | "keep_only" => json!({"op": name, "paths": ["/x"]}),
                "set" => json!({"op": "set", "path": "/x", "value": 1}),
                "rename" | "copy" => json!({"op": name, "from": "/a", "to": "/b"}),
                "clamp" => json!({"op": "clamp", "path": "/x", "min": 1}),
                "map_value" => json!({"op": "map_value", "path": "/x", "table": {"a": "b"}}),
                "role_map" => json!({"op": "role_map", "from": "a", "to": "b"}),
                "each" => json!({"op": "each", "path": "/x", "do": []}),
                "usage_map" => json!({"op": "usage_map", "slots": {}}),
                "capability" => json!({"op": "capability", "facts": {}}),
                "retry_same" => json!({"op": "retry_same"}),
                "failover" => json!({"op": "failover"}),
                "fail" => json!({"op": "fail"}),
                other => panic!("{other}"),
            })
            .expect("sample")
        };
        for info in OPS {
            let op = sample(info.name);
            for phase in Phase::ALL {
                assert_eq!(
                    op.allowed_in(phase),
                    info.phases.contains(&phase),
                    "{} in {}",
                    info.name,
                    phase.as_str()
                );
            }
        }
    }

    #[test]
    fn every_recipe_passes_the_validator() {
        let recipes = recipes();
        let list = recipes.as_array().expect("array");
        assert!(list.len() >= 8);
        let mut ids = std::collections::HashSet::new();
        for recipe in list {
            let id = recipe["id"].as_str().expect("id");
            assert!(ids.insert(id), "重复的配方 {id}");
            let rule = recipe["rule"].clone();
            assert_eq!(rule["id"], recipe["id"]);
            crate::validate(&json!([rule])).unwrap_or_else(|issues| panic!("{id}: {issues:?}"));
        }
    }

    #[test]
    fn the_catalog_lists_what_the_engine_knows() {
        let doc = catalog();
        assert_eq!(doc["ops"].as_array().expect("ops").len(), OP_NAMES.len());
        assert_eq!(
            doc["phases"].as_array().expect("phases").len(),
            Phase::ALL.len()
        );
        let enabled: Vec<&str> = doc["phases"]
            .as_array()
            .expect("phases")
            .iter()
            .filter(|p| p["enabled"] == true)
            .filter_map(|p| p["id"].as_str())
            .collect();
        assert_eq!(enabled, ["request", "response", "error", "capability"]);
        assert_eq!(
            doc["usage_slots"].as_array().expect("slots").len(),
            Slot::ALL.len()
        );
        assert_eq!(
            doc["capability_facts"].as_array().expect("facts").len(),
            CAPABILITY_FACTS.len()
        );
        assert!(
            doc["protected_paths"]["request"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        );
        assert!(
            doc["protected_paths"]["response"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        );
        // 请求阶段的操作列表来自同一张表
        let request_ops = doc["phases"][1]["ops"].as_array().expect("ops");
        assert!(
            request_ops.contains(&json!("role_map")) && !request_ops.contains(&json!("usage_map"))
        );
    }
}
