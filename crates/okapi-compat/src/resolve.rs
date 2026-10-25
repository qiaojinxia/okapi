//! 分层合并与请求阶段执行。
//!
//! 有效规则 = 厂商 ⊕（档案，P2）⊕ 渠道 ⊕ 护栏：
//! - 规则有 `id`，**更靠后的层里同 id 的规则覆盖前面的**；`disabled: true` 把前面的同 id 规则关掉；
//! - 护栏排在最后，所以谁也覆盖不了它；
//! - 执行顺序：阶段 → 层级（厂商 100、档案 200、渠道 300、护栏 900）→ `order` → 文档顺序。

use crate::protect::Snapshot;
use crate::rule::{Phase, Rule};
use crate::select::Context;
use crate::validate::{ValidRule, Validated};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Vendor,
    /// 适配档案（P2）。
    Profile,
    Channel,
    Guard,
}

impl Layer {
    #[must_use]
    pub const fn rank(self) -> u16 {
        match self {
            Self::Vendor => 100,
            Self::Profile => 200,
            Self::Channel => 300,
            Self::Guard => 900,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vendor => "vendor",
            Self::Profile => "profile",
            Self::Channel => "channel",
            Self::Guard => "guard",
        }
    }
}

/// 一层的输入：规则集 + 一个能说明来源与版本的标签（如 `builtin:deepseek`、`channel#7@v3`）。
#[derive(Debug, Clone)]
pub struct LayerInput {
    pub layer: Layer,
    pub label: String,
    pub rules: Arc<Validated>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerInfo {
    pub layer: Layer,
    pub label: String,
    pub rules: usize,
}

/// 被后面的层盖住的规则（有效规则页展示「这条为什么没生效」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shadowed {
    pub id: String,
    pub layer: Layer,
    pub by: Layer,
    /// `overridden`（被同 id 的新定义替换）或 `disabled`（被同 id 的 disabled 关掉）。
    pub reason: &'static str,
}

#[derive(Debug, Clone)]
struct Active {
    layer: Layer,
    set: Arc<Validated>,
    idx: usize,
}

impl Active {
    fn valid(&self) -> &ValidRule {
        &self.set.rules[self.idx]
    }
}

/// 一条有效规则（执行顺序里的一项）。
#[derive(Debug, Clone, Copy)]
pub struct EffectiveRule<'a> {
    pub layer: Layer,
    pub rule: &'a Rule,
}

#[derive(Debug, Clone, Default)]
pub struct EffectiveRules {
    active: Vec<Active>,
    layers: Vec<LayerInfo>,
    shadowed: Vec<Shadowed>,
}

#[must_use]
pub fn resolve(mut inputs: Vec<LayerInput>) -> EffectiveRules {
    inputs.sort_by_key(|input| input.layer.rank());
    let mut layers = Vec::with_capacity(inputs.len());
    let mut slots: Vec<Active> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut shadowed = Vec::new();
    for input in inputs {
        layers.push(LayerInfo {
            layer: input.layer,
            label: input.label.clone(),
            rules: input.rules.len(),
        });
        for (idx, valid) in input.rules.rules.iter().enumerate() {
            let slot = Active {
                layer: input.layer,
                set: Arc::clone(&input.rules),
                idx,
            };
            if let Some(&pos) = by_id.get(&valid.rule.id) {
                shadowed.push(Shadowed {
                    id: valid.rule.id.clone(),
                    layer: slots[pos].layer,
                    by: input.layer,
                    reason: if valid.rule.disabled {
                        "disabled"
                    } else {
                        "overridden"
                    },
                });
                slots[pos] = slot;
            } else {
                by_id.insert(valid.rule.id.clone(), slots.len());
                slots.push(slot);
            }
        }
    }
    let mut active: Vec<Active> = slots
        .into_iter()
        .filter(|slot| !slot.valid().rule.disabled)
        .collect();
    active.sort_by_key(|slot| {
        let rule = &slot.valid().rule;
        (rule.phase, slot.layer.rank(), rule.order, slot.idx)
    });
    EffectiveRules {
        active,
        layers,
        shadowed,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fired {
    pub rule: String,
    pub layer: Layer,
    /// 真正改变的节点个数。
    pub changes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub rule: String,
    pub layer: Layer,
    pub reason: &'static str,
}

/// 一次应用的轨迹：命中并改了东西的规则，以及被跳过的规则。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub fired: Vec<Fired>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// 改写后的请求体；`None` = 没有任何变化（沿用原字节）。
    pub body: Option<Vec<u8>>,
    pub report: Report,
}

/// 只有标了 `required` 的规则没能应用时才失败（失败关闭）；其余规则出问题一律跳过并记录。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("compat_required_rule_failed: {rule}: {reason}")]
    Required { rule: String, reason: &'static str },
}

impl EffectiveRules {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    #[must_use]
    pub fn layers(&self) -> &[LayerInfo] {
        &self.layers
    }

    #[must_use]
    pub fn shadowed(&self) -> &[Shadowed] {
        &self.shadowed
    }

    /// 全部有效规则，按执行顺序。
    pub fn rules(&self) -> impl Iterator<Item = EffectiveRule<'_>> {
        self.active.iter().map(|slot| EffectiveRule {
            layer: slot.layer,
            rule: &slot.valid().rule,
        })
    }

    fn selected<'a>(&'a self, ctx: &'a Context<'a>) -> impl Iterator<Item = &'a Active> {
        self.active.iter().filter(move |slot| {
            let valid = slot.valid();
            valid.rule.phase == Phase::Request && valid.selector.matches(ctx)
        })
    }

    /// 被选中的请求阶段规则有没有接管请求顶层的 `field`（删掉、强制写值、改名走、不在白名单里）。
    #[must_use]
    pub fn manages_top_level(&self, ctx: &Context<'_>, field: &str) -> bool {
        self.selected(ctx).any(|slot| {
            slot.valid()
                .ops
                .iter()
                .any(|op| op.manages_top_level(field))
        })
    }

    /// 有没有规则会被选中。先于解析请求体调用：没有就不付解析成本（无规则零开销）。
    #[must_use]
    pub fn selects(&self, ctx: &Context<'_>) -> bool {
        self.selected(ctx).next().is_some()
    }

    /// 对一份已解析的请求体执行请求阶段规则。
    pub fn apply_request_value(
        &self,
        ctx: &Context<'_>,
        doc: &mut Value,
    ) -> Result<Report, Failure> {
        let mut report = Report::default();
        if !doc.is_object() {
            return self.refuse_all(ctx, "body_not_object").map(|skipped| {
                report.skipped = skipped;
                report
            });
        }
        let snapshot = Snapshot::capture(doc);
        for slot in self.selected(ctx) {
            let valid = slot.valid();
            let changes: usize = valid.ops.iter().map(|op| op.apply(doc)).sum();
            if !snapshot.intact(doc) {
                // 校验器已经拦住了写受保护路径的规则，这里是兜底：把主链字段恢复原样并记下
                snapshot.restore(doc);
                if valid.rule.required {
                    return Err(Failure::Required {
                        rule: valid.rule.id.clone(),
                        reason: "protected_field_changed",
                    });
                }
                report.skipped.push(Skipped {
                    rule: valid.rule.id.clone(),
                    layer: slot.layer,
                    reason: "protected_field_changed",
                });
                continue;
            }
            if changes > 0 {
                report.fired.push(Fired {
                    rule: valid.rule.id.clone(),
                    layer: slot.layer,
                    changes,
                });
            }
        }
        Ok(report)
    }

    fn refuse_all(&self, ctx: &Context<'_>, reason: &'static str) -> Result<Vec<Skipped>, Failure> {
        let mut skipped = Vec::new();
        for slot in self.selected(ctx) {
            let rule = &slot.valid().rule;
            if rule.required {
                return Err(Failure::Required {
                    rule: rule.id.clone(),
                    reason,
                });
            }
            skipped.push(Skipped {
                rule: rule.id.clone(),
                layer: slot.layer,
                reason,
            });
        }
        Ok(skipped)
    }

    /// 对原始请求体字节执行请求阶段规则：没有规则被选中时不解析、不分配；
    /// 解析后没有任何改动时 `body` 为 `None`，调用方沿用原字节。
    pub fn apply_request(&self, ctx: &Context<'_>, body: &[u8]) -> Result<Outcome, Failure> {
        if self.active.is_empty() || !self.selects(ctx) {
            return Ok(Outcome::default());
        }
        let Ok(mut doc) = serde_json::from_slice::<Value>(body) else {
            let skipped = self.refuse_all(ctx, "body_not_json")?;
            return Ok(Outcome {
                body: None,
                report: Report {
                    fired: Vec::new(),
                    skipped,
                },
            });
        };
        let mut report = self.apply_request_value(ctx, &mut doc)?;
        if report.fired.is_empty() {
            return Ok(Outcome { body: None, report });
        }
        if let Ok(bytes) = serde_json::to_vec(&doc) {
            return Ok(Outcome {
                body: Some(bytes),
                report,
            });
        }
        // 序列化一个刚解析出来的 Value 不会失败；万一失败，宁可不改也不发残缺的体
        report
            .skipped
            .extend(report.fired.drain(..).map(|f| Skipped {
                rule: f.rule,
                layer: f.layer,
                reason: "serialize_failed",
            }));
        Ok(Outcome { body: None, report })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::validate;
    use serde_json::json;

    fn set(rules: &Value) -> Arc<Validated> {
        Arc::new(validate(rules).expect("valid rules"))
    }

    // 测试夹具：调用处直接写 json! 字面量，按值接收最省事
    #[allow(clippy::needless_pass_by_value)]
    fn layer(layer: Layer, label: &str, rules: Value) -> LayerInput {
        LayerInput {
            layer,
            label: label.to_owned(),
            rules: set(&rules),
        }
    }

    fn ctx() -> Context<'static> {
        Context {
            vendor: Some("builtin:moonshot"),
            host: Some("api.moonshot.cn"),
            model: "kimi-k2.6",
            client_model: "kimi-k2.6",
            dialect: "openai.chat",
            operation: "chat",
            stream: false,
            client: None,
        }
    }

    fn run(effective: &EffectiveRules, body: &Value) -> (Value, Report) {
        let outcome = effective
            .apply_request(&ctx(), body.to_string().as_bytes())
            .expect("apply");
        let after = outcome.body.map_or_else(
            || body.clone(),
            |b| serde_json::from_slice(&b).expect("json"),
        );
        (after, outcome.report)
    }

    #[test]
    fn empty_rules_touch_nothing_and_never_parse() {
        let effective = resolve(Vec::new());
        assert!(effective.is_empty());
        let outcome = effective
            .apply_request(&ctx(), b"not json at all")
            .expect("apply");
        assert!(outcome.body.is_none());
        assert_eq!(outcome.report, Report::default());
    }

    #[test]
    fn later_layers_override_earlier_ones_by_id() {
        let effective = resolve(vec![
            layer(
                Layer::Vendor,
                "builtin:moonshot",
                json!([{"id": "t", "do": [{"op": "set", "path": "/temperature", "value": 1}]}]),
            ),
            layer(
                Layer::Channel,
                "channel#7@v2",
                json!([{"id": "t", "do": [{"op": "set", "path": "/temperature", "value": 0.5}]}]),
            ),
        ]);
        let (after, report) = run(&effective, &json!({"model": "m"}));
        assert_eq!(after["temperature"], json!(0.5));
        assert_eq!(report.fired.len(), 1);
        assert_eq!(report.fired[0].layer, Layer::Channel);
        assert_eq!(
            effective.shadowed(),
            [Shadowed {
                id: "t".into(),
                layer: Layer::Vendor,
                by: Layer::Channel,
                reason: "overridden"
            }]
        );
    }

    #[test]
    fn a_disabled_override_switches_off_the_inherited_rule() {
        let effective = resolve(vec![
            layer(
                Layer::Vendor,
                "builtin:moonshot",
                json!([{"id": "t", "do": [{"op": "set", "path": "/temperature", "value": 1}]}]),
            ),
            layer(
                Layer::Channel,
                "channel#7@v1",
                json!([{"id": "t", "disabled": true}]),
            ),
        ]);
        assert!(effective.is_empty());
        let (after, report) = run(&effective, &json!({"model": "m"}));
        assert_eq!(after, json!({"model": "m"}));
        assert!(report.fired.is_empty(), "{:?}", report.fired);
        assert_eq!(effective.shadowed()[0].reason, "disabled");
    }

    #[test]
    fn guard_rules_run_last_and_cannot_be_overridden_from_below() {
        let guard = layer(
            Layer::Guard,
            "guard@v1",
            json!([{"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}]),
        );
        let channel = layer(
            Layer::Channel,
            "channel#7@v1",
            json!([
                {"id": "no-user", "disabled": true},
                {"id": "put-user", "do": [{"op": "set", "path": "/user", "value": "u"}]}
            ]),
        );
        // 传入顺序故意颠倒：层级只看 rank
        let effective = resolve(vec![guard, channel]);
        let (after, _) = run(&effective, &json!({"model": "m"}));
        assert!(
            after.get("user").is_none(),
            "护栏最后执行，把渠道写进去的 user 又删了"
        );
        let order: Vec<&str> = effective.rules().map(|r| r.rule.id.as_str()).collect();
        assert_eq!(order, ["put-user", "no-user"]);
    }

    #[test]
    fn execution_order_is_layer_then_order_then_document_order() {
        let effective = resolve(vec![layer(
            Layer::Channel,
            "channel#1@v1",
            json!([
                {"id": "c", "order": 5, "do": [{"op": "set", "path": "/x", "value": "c"}]},
                {"id": "a", "order": 1, "do": [{"op": "set", "path": "/x", "value": "a"}]},
                {"id": "b", "order": 1, "do": [{"op": "set", "path": "/x", "value": "b"}]}
            ]),
        )]);
        let order: Vec<&str> = effective.rules().map(|r| r.rule.id.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"]);
        let (after, _) = run(&effective, &json!({}));
        assert_eq!(after["x"], json!("c"));
    }

    #[test]
    fn selectors_gate_each_rule() {
        let effective = resolve(vec![layer(
            Layer::Channel,
            "channel#1@v1",
            json!([
                {"id": "kimi", "select": {"model": ["kimi-*"]}, "do": [{"op": "set", "path": "/a", "value": 1}]},
                {"id": "other", "select": {"model": ["gpt-*"]}, "do": [{"op": "set", "path": "/b", "value": 1}]},
                {"id": "stream-only", "select": {"stream": true}, "do": [{"op": "set", "path": "/c", "value": 1}]}
            ]),
        )]);
        let (after, report) = run(&effective, &json!({}));
        assert_eq!(after, json!({"a": 1}));
        assert_eq!(report.fired.len(), 1);
        assert!(effective.selects(&ctx()));
        let mut other = ctx();
        other.model = "claude-x";
        assert!(!effective.selects(&other));
    }

    #[test]
    fn manages_top_level_reports_the_fields_rules_take_over() {
        let effective = resolve(vec![layer(
            Layer::Channel,
            "c",
            json!([
                {"id": "a", "select": {"model": ["kimi-*"]}, "do": [
                    {"op": "remove", "paths": ["/user", "/metadata/x"]},
                    {"op": "set", "path": "/thinking", "value": {}},
                    {"op": "set", "path": "/soft", "value": 1, "mode": "if_absent"},
                    {"op": "clamp", "path": "/temperature", "min": 1, "max": 1},
                    {"op": "clamp", "path": "/top_p", "min": 0, "max": 1},
                    {"op": "rename", "from": "/max_tokens", "to": "/max_completion_tokens"}
                ]},
                {"id": "other-model", "select": {"model": ["gpt-*"]}, "do": [
                    {"op": "remove", "paths": ["/seed"]}
                ]}
            ]),
        )]);
        let ctx = ctx();
        for field in ["user", "thinking", "temperature", "max_tokens"] {
            assert!(effective.manages_top_level(&ctx, field), "{field}");
        }
        for field in [
            "metadata",
            "soft",
            "top_p",
            "messages",
            "seed",
            "max_completion_tokens",
        ] {
            assert!(!effective.manages_top_level(&ctx, field), "{field}");
        }
        let whitelist = resolve(vec![layer(
            Layer::Channel,
            "c",
            json!([{"id": "w", "do": [{"op": "keep_only", "paths": ["/messages", "/temperature", "/stream_options"]}]}]),
        )]);
        assert!(whitelist.manages_top_level(&ctx, "top_p"));
        assert!(!whitelist.manages_top_level(&ctx, "temperature"));
        assert!(
            !whitelist.manages_top_level(&ctx, "model"),
            "受保护键永远保留"
        );
    }

    #[test]
    fn rules_that_change_nothing_are_not_reported_as_fired() {
        let effective = resolve(vec![layer(
            Layer::Channel,
            "channel#1@v1",
            json!([{"id": "strip", "do": [{"op": "remove", "paths": ["/user"]}]}]),
        )]);
        let outcome = effective
            .apply_request(&ctx(), br#"{"model":"m"}"#)
            .expect("apply");
        assert!(outcome.body.is_none(), "没变化就沿用原字节");
        assert!(
            outcome.report.fired.is_empty(),
            "{:?}",
            outcome.report.fired
        );
    }

    #[test]
    fn non_object_bodies_are_skipped_unless_a_rule_is_required() {
        let lenient = resolve(vec![layer(
            Layer::Channel,
            "c",
            json!([{"id": "strip", "do": [{"op": "remove", "paths": ["/user"]}]}]),
        )]);
        let outcome = lenient.apply_request(&ctx(), b"[1,2]").expect("apply");
        assert!(outcome.body.is_none());
        assert_eq!(outcome.report.skipped[0].reason, "body_not_object");
        let outcome = lenient.apply_request(&ctx(), b"not json").expect("apply");
        assert_eq!(outcome.report.skipped[0].reason, "body_not_json");

        let strict = resolve(vec![layer(
            Layer::Guard,
            "g",
            json!([{"id": "must", "required": true, "do": [{"op": "remove", "paths": ["/user"]}]}]),
        )]);
        let err = strict
            .apply_request(&ctx(), b"[1,2]")
            .expect_err("required fails closed");
        assert_eq!(
            err,
            Failure::Required {
                rule: "must".into(),
                reason: "body_not_object"
            }
        );
    }

    #[test]
    fn applying_twice_equals_applying_once() {
        let effective = resolve(vec![layer(
            Layer::Channel,
            "c",
            json!([
                {"id": "a", "do": [
                    {"op": "remove", "paths": ["/user"]},
                    {"op": "set", "path": "/thinking/type", "value": "disabled"},
                    {"op": "rename", "from": "/max_tokens", "to": "/max_completion_tokens"},
                    {"op": "clamp", "path": "/temperature", "min": 0, "max": 1},
                    {"op": "role_map", "from": "developer", "to": "system"}
                ]}
            ]),
        )]);
        let body = json!({
            "model": "m", "user": "u", "max_tokens": 5, "temperature": 3,
            "messages": [{"role": "developer", "content": "x"}]
        });
        let (once, _) = run(&effective, &body);
        let (twice, report) = run(&effective, &once);
        assert_eq!(once, twice);
        assert!(report.fired.is_empty(), "{:?}", report.fired);
    }

    #[test]
    fn layer_infos_and_labels_describe_the_sources() {
        let effective = resolve(vec![
            layer(Layer::Channel, "channel#7@v3", json!([])),
            layer(
                Layer::Vendor,
                "builtin:moonshot",
                json!([{"id": "a", "do": [{"op": "remove", "paths": ["/x"]}]}]),
            ),
        ]);
        let infos: Vec<(&str, usize)> = effective
            .layers()
            .iter()
            .map(|l| (l.label.as_str(), l.rules))
            .collect();
        assert_eq!(infos, [("builtin:moonshot", 1), ("channel#7@v3", 0)]);
    }
}
