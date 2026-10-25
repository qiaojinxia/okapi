//! 规则文档的数据类型（schema v1）。这是存进 `rule_sets.rules` 的形状，也是管理 API 收发的形状。
//!
//! 一个规则集就是 `Vec<Rule>`；`schema_version` 与 `kind` 放在版本表的列里，不进文档。
//! 所有结构都 `deny_unknown_fields`：拼错的字段名在写入时就报，而不是静默失效。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

pub const SCHEMA_VERSION: u32 = 1;

/// 阶段。枚举现在就写全，以后加阶段不用改规则格式；P1 只执行 `request`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// 方言转换之前（预留）。
    RequestPre,
    /// 已转成上游方言、发送之前。
    Request,
    /// 头、端点、认证声明（预留）。
    Wire,
    /// 上游响应转回客户端方言之前（预留）。
    Response,
    /// 上游失败之后的反应（预留）。
    Error,
    /// 能力事实（预留）。
    Capability,
}

impl Phase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestPre => "request_pre",
            Self::Request => "request",
            Self::Wire => "wire",
            Self::Response => "response",
            Self::Error => "error",
            Self::Capability => "capability",
        }
    }

    /// 当前版本执行的阶段。
    #[must_use]
    pub const fn enabled(self) -> bool {
        matches!(self, Self::Request)
    }
}

fn default_phase() -> Phase {
    Phase::Request
}

// 序列化时不写默认值：库里存的、API 回显的、手写的文档形态一致，也省空间。
// serde 的 skip_serializing_if 只接受 `fn(&T) -> bool`，所以下面几个按引用接收是必须的。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_request(phase: &Phase) -> bool {
    *phase == Phase::Request
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &i32) -> bool {
    *n == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// 选择器：全部字段 AND；缺省 = 不限。结构化而不是表达式，所以能建索引、能画成表单。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    /// 厂商 id（`deepseek` 或 `builtin:deepseek`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendor: Vec<String>,
    /// 渠道 `api_base` 的主机名，支持 `*`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host: Vec<String>,
    /// **映射后的上游模型名**，支持 `*`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model: Vec<String>,
    /// 客户端请求的模型名，支持 `*`（少用）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_model: Vec<String>,
    /// 上游方言：`openai.chat` / `openai.responses` / `anthropic.messages` / `gemini.generate`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dialect: Vec<String>,
    /// `chat` / `responses` / `embeddings` / `images` / `rerank` / `audio`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// 入口客户端类别（UA 识别，如 `claude-code`、`codex`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client: Vec<String>,
}

impl Selector {
    /// 没有任何约束（匹配一切）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vendor.is_empty()
            && self.host.is_empty()
            && self.model.is_empty()
            && self.client_model.is_empty()
            && self.dialect.is_empty()
            && self.operation.is_empty()
            && self.stream.is_none()
            && self.client.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDoc {
    /// 为什么需要这条规则（内置规则必填）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 上次对照来源核实的日期（`YYYY-MM-DD`；内置规则必填，按月复核）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
}

/// `set` / `rename` / `copy` 的目标已有值时怎么办。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 覆盖。
    #[default]
    Always,
    /// 目标缺失才写；已有就整条不动。
    IfAbsent,
}

impl Mode {
    #[must_use]
    pub const fn if_absent(self) -> bool {
        matches!(self, Self::IfAbsent)
    }

    #[must_use]
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 skip_serializing_if 只接受 `fn(&T) -> bool`
    pub const fn is_always(&self) -> bool {
        matches!(self, Self::Always)
    }
}

/// 请求阶段的操作。封闭集合：只增不减，表达不了的特殊要求加一个**通用**原语，不写厂商特例。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// 删除路径。
    Remove { paths: Vec<String> },
    /// 白名单：只保留列出的路径（受保护键永远保留）。
    KeepOnly { paths: Vec<String> },
    /// 写值。
    Set {
        path: String,
        value: Value,
        #[serde(default, skip_serializing_if = "Mode::is_always")]
        mode: Mode,
    },
    /// 移动：来源删掉、目标写入。来源缺失则不动。
    Rename {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Mode::is_always")]
        mode: Mode,
    },
    /// 复制：来源保留。
    Copy {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Mode::is_always")]
        mode: Mode,
    },
    /// 数值夹取；整数路径保持整数，按精确十进制比较，不经过浮点。
    Clamp {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<Number>,
    },
    /// 查表替换字符串值。
    MapValue {
        path: String,
        table: Map<String, Value>,
    },
    /// 对话角色映射，作用于 `/messages/*/role` 与 `/input/*/role`（`map_value` 的语法糖）。
    RoleMap { from: String, to: String },
}

impl Op {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Remove { .. } => "remove",
            Self::KeepOnly { .. } => "keep_only",
            Self::Set { .. } => "set",
            Self::Rename { .. } => "rename",
            Self::Copy { .. } => "copy",
            Self::Clamp { .. } => "clamp",
            Self::MapValue { .. } => "map_value",
            Self::RoleMap { .. } => "role_map",
        }
    }
}

/// 当前版本认识的操作名（校验器据此给出「未知操作」而不是一句 serde 报错）。
pub const OP_NAMES: [&str; 8] = [
    "remove",
    "keep_only",
    "set",
    "rename",
    "copy",
    "clamp",
    "map_value",
    "role_map",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// 规则集内唯一；跨层同 id = 后面的层覆盖前面的。
    pub id: String,
    #[serde(default = "default_phase", skip_serializing_if = "is_request")]
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Selector::is_empty")]
    pub select: Selector,
    /// CEL 条件。P1 不启用：保留字段，校验器返回 `cel_not_enabled`，存量规则以后不用迁移。
    #[serde(default, rename = "if", skip_serializing_if = "Option::is_none")]
    pub cond: Option<String>,
    #[serde(default, rename = "do", skip_serializing_if = "Vec::is_empty")]
    pub ops: Vec<Op>,
    /// 同层内的微调；越小越先执行。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub order: i32,
    /// 把前面层里同 id 的规则关掉（本规则自己不执行）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    /// 失败关闭：这条规则没能应用（如请求体不是 JSON 对象）就拒绝请求，而不是放行。
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// 允许这条规则动计费关键路径（见 [`crate::protect`]）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub billing_critical_ack: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<RuleDoc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rule_round_trips_with_defaults_filled() {
        let rule: Rule = serde_json::from_value(json!({
            "id": "a.b",
            "select": {"model": ["kimi-k2.6"]},
            "do": [{"op": "clamp", "path": "/temperature", "min": 1, "max": 1}]
        }))
        .expect("rule");
        assert_eq!(rule.phase, Phase::Request);
        assert!(!rule.disabled && !rule.required && !rule.billing_critical_ack);
        assert_eq!(rule.select.model, ["kimi-k2.6"]);
        let again: Rule =
            serde_json::from_value(serde_json::to_value(&rule).expect("to_value")).expect("again");
        assert_eq!(again, rule);
    }

    #[test]
    fn unknown_fields_are_rejected_everywhere() {
        for doc in [
            json!({"id": "a", "typo": 1, "do": []}),
            json!({"id": "a", "select": {"modle": ["x"]}, "do": []}),
            json!({"id": "a", "do": [{"op": "remove", "paths": [], "extra": 1}]}),
            json!({"id": "a", "doc": {"whyy": "x"}, "do": []}),
        ] {
            assert!(
                serde_json::from_value::<Rule>(doc.clone()).is_err(),
                "{doc}"
            );
        }
    }

    #[test]
    fn serialization_omits_every_default() {
        let rule: Rule = serde_json::from_value(json!({
            "id": "a", "phase": "request", "select": {}, "order": 0, "disabled": false,
            "required": false, "billing_critical_ack": false,
            "do": [{"op": "set", "path": "/x", "value": 1, "mode": "always"}]
        }))
        .expect("rule");
        assert_eq!(
            serde_json::to_value(&rule).expect("json"),
            json!({"id": "a", "do": [{"op": "set", "path": "/x", "value": 1}]})
        );
        let tombstone: Rule =
            serde_json::from_value(json!({"id": "a", "disabled": true})).expect("rule");
        assert_eq!(
            serde_json::to_value(&tombstone).expect("json"),
            json!({"id": "a", "disabled": true})
        );
        let explicit: Rule = serde_json::from_value(json!({
            "id": "a", "order": 3, "required": true, "select": {"stream": false},
            "do": [{"op": "rename", "from": "/a", "to": "/b", "mode": "if_absent"}]
        }))
        .expect("rule");
        let back = serde_json::to_value(&explicit).expect("json");
        assert_eq!(back["order"], 3);
        assert_eq!(back["required"], true);
        assert_eq!(back["select"], json!({"stream": false}));
        assert_eq!(back["do"][0]["mode"], "if_absent");
    }

    #[test]
    fn if_is_spelled_if_and_do_is_spelled_do() {
        let rule: Rule = serde_json::from_value(json!({
            "id": "a", "if": "true", "do": [{"op": "remove", "paths": ["/x"]}]
        }))
        .expect("rule");
        assert_eq!(rule.cond.as_deref(), Some("true"));
        assert_eq!(rule.ops.len(), 1);
    }

    #[test]
    fn numbers_keep_their_spelling() {
        let rule: Rule = serde_json::from_value(json!({
            "id": "a", "do": [{"op": "set", "path": "/t", "value": 1.0}]
        }))
        .expect("rule");
        let text = serde_json::to_string(&rule).expect("json");
        assert!(text.contains("1.0"), "{text}");
    }

    #[test]
    fn op_name_table_matches_the_enum() {
        let sample = [
            Op::Remove { paths: vec![] },
            Op::KeepOnly { paths: vec![] },
            Op::Set {
                path: String::new(),
                value: Value::Null,
                mode: Mode::Always,
            },
            Op::Rename {
                from: String::new(),
                to: String::new(),
                mode: Mode::Always,
            },
            Op::Copy {
                from: String::new(),
                to: String::new(),
                mode: Mode::Always,
            },
            Op::Clamp {
                path: String::new(),
                min: None,
                max: None,
            },
            Op::MapValue {
                path: String::new(),
                table: Map::new(),
            },
            Op::RoleMap {
                from: String::new(),
                to: String::new(),
            },
        ];
        let names: Vec<&str> = sample.iter().map(Op::name).collect();
        assert_eq!(names, OP_NAMES);
    }
}
