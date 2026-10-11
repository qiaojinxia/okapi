//! 模拟请求体按真机的字段顺序写出（IMPLEMENTATION §11.42）。
//!
//! CLI 是 JS，请求体按对象字面量的插入顺序序列化；`serde_json::Value` 的对象按字母序（工作区没开
//! `preserve_order`，也不该为这一处全局打开），逐字节一比就露馅。这里不改 `Value`，只在最后一步
//! 换一个写法：
//! - 外层信封（顶层、消息、内容块、工具定义、`cache_control` 等）按 2.1.294 源码里拼请求体的顺序；
//! - 信封之外的调用方内容（工具参数、JSON Schema 等）照原请求体的键序（[`Shape`] 记下来的）；
//! - 两边都没有的新键（模拟补的）排在后面、按字母序。
use crate::UpstreamError;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// 原请求体里每个对象的键序。只用来排序，值本身仍以 `Value` 为准。
#[derive(Debug, Default)]
pub(super) enum Shape {
    #[default]
    Leaf,
    Object(Vec<(String, Shape)>),
    Array(Vec<Shape>),
}

impl Shape {
    fn key(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries.iter().find(|(key, _)| key == name).map(|(_, s)| s),
            _ => None,
        }
    }

    fn item(&self, index: usize) -> Option<&Self> {
        match self {
            Self::Array(items) => items.get(index),
            _ => None,
        }
    }

    fn position(&self, name: &str) -> Option<usize> {
        match self {
            Self::Object(entries) => entries.iter().position(|(key, _)| key == name),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ShapeVisitor;
        impl<'de> Visitor<'de> for ShapeVisitor {
            type Value = Shape;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
                let mut entries = Vec::new();
                while let Some((key, shape)) = map.next_entry::<String, Shape>()? {
                    entries.push((key, shape));
                }
                Ok(Shape::Object(entries))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<Shape>()? {
                    items.push(item);
                }
                Ok(Shape::Array(items))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_str<E>(self, _: &str) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_unit<E>(self) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_none<E>(self) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
        }
        deserializer.deserialize_any(ShapeVisitor)
    }
}

/// 信封里的位置。`OTHERS` 是排序表里「未列出的键」占的那一格。
#[derive(Clone, Copy)]
enum Ctx {
    Top,
    Message,
    Block,
    Tool,
    Fixed(&'static [&'static str]),
    /// `context_management`：`edits` 里每条编辑 `type` 打头（真机 `{"type":…,"keep":"all"}`）。
    ContextManagement,
    /// 调用方内容：只照原键序。
    Free,
}

const OTHERS: &str = "*";

/// 顶层：2.1.294 拼 `/v1/messages` 请求体的顺序（`stream` 由 SDK 最后补上）；
/// `temperature` 之外的采样参数等在源码里是展开进来的，落在 `safeguards` 与 `output_config` 之间。
const TOP: &[&str] = &[
    "model",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "betas",
    "metadata",
    "max_tokens",
    "thinking",
    "temperature",
    "context_management",
    "safeguards",
    OTHERS,
    "output_config",
    "speed",
    "thread",
    "diagnostics",
    "stream",
];
const MESSAGE: &[&str] = &["role", "content", OTHERS];
const BLOCK: &[&str] = &[
    "type",
    "id",
    "tool_use_id",
    "name",
    "text",
    "thinking",
    "signature",
    "data",
    "source",
    "input",
    "content",
    "is_error",
    "citations",
    OTHERS,
    "cache_control",
];
const TOOL: &[&str] = &[
    "type",
    "name",
    "description",
    "input_schema",
    OTHERS,
    "cache_control",
];
const CACHE_CONTROL: &[&str] = &["type", "ttl", "scope"];
const SOURCE: &[&str] = &["type", "media_type", "data", "url", "file_id"];

impl Ctx {
    fn rank(self) -> &'static [&'static str] {
        match self {
            Self::Top => TOP,
            Self::Message => MESSAGE,
            Self::Block => BLOCK,
            Self::Tool => TOOL,
            Self::Fixed(rank) => rank,
            Self::ContextManagement => &["edits"],
            Self::Free => &[],
        }
    }

    /// `key` 的值所处的位置（数组则是每个元素的位置）。
    fn child(self, key: &str) -> Self {
        match (self, key) {
            (Self::Top, "messages") => Self::Message,
            (Self::Top, "system") | (Self::Message | Self::Block, "content") => Self::Block,
            (Self::Top, "tools") => Self::Tool,
            (Self::Top, "metadata") => Self::Fixed(&["user_id"]),
            (Self::Top, "thinking") => Self::Fixed(&["type", "budget_tokens", "display"]),
            (Self::Top, "tool_choice") => {
                Self::Fixed(&["type", "name", "disable_parallel_tool_use"])
            }
            (Self::Top, "context_management") => Self::ContextManagement,
            (Self::ContextManagement, "edits") => Self::Fixed(&["type", OTHERS]),
            (Self::Top, "output_config") => Self::Fixed(&["effort", "format"]),
            (Self::Top, "diagnostics") => Self::Fixed(&["previous_message_id"]),
            (Self::Block | Self::Tool, "cache_control") => Self::Fixed(CACHE_CONTROL),
            (Self::Block, "source") => Self::Fixed(SOURCE),
            _ => Self::Free,
        }
    }
}

/// 按真机顺序序列化 `value`；`shape` 是原请求体的键序（读不出来就传 `Shape::Leaf`）。
pub(super) fn to_vec(value: &Value, shape: &Shape) -> Result<Vec<u8>, UpstreamError> {
    let mut out = Vec::with_capacity(4096);
    write(&mut out, value, Some(shape), Ctx::Top)?;
    Ok(out)
}

fn write(
    out: &mut Vec<u8>,
    value: &Value,
    shape: Option<&Shape>,
    ctx: Ctx,
) -> Result<(), UpstreamError> {
    match value {
        Value::Object(map) => {
            out.push(b'{');
            for (at, (key, child)) in ordered(map, shape, ctx.rank()).into_iter().enumerate() {
                if at > 0 {
                    out.push(b',');
                }
                leaf(out, &Value::String(key.clone()))?;
                out.push(b':');
                write(out, child, shape.and_then(|s| s.key(key)), ctx.child(key))?;
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (at, item) in items.iter().enumerate() {
                if at > 0 {
                    out.push(b',');
                }
                write(out, item, shape.and_then(|s| s.item(at)), ctx)?;
            }
            out.push(b']');
        }
        _ => leaf(out, value)?,
    }
    Ok(())
}

fn leaf(out: &mut Vec<u8>, value: &Value) -> Result<(), UpstreamError> {
    serde_json::to_writer(out, value).map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 先按排序表的格子，同一格（`OTHERS` 或无表）里原有的键照原顺序、新键按字母序排在后面。
fn ordered<'a>(
    map: &'a Map<String, Value>,
    shape: Option<&Shape>,
    rank: &[&str],
) -> Vec<(&'a String, &'a Value)> {
    let others = rank.iter().position(|k| *k == OTHERS).unwrap_or(rank.len());
    let mut entries: Vec<(usize, usize, &String, &Value)> = map
        .iter()
        .enumerate()
        .map(|(alphabetical, (key, value))| {
            let slot = rank.iter().position(|k| k == key).unwrap_or(others);
            let within = shape
                .and_then(|s| s.position(key))
                .unwrap_or(usize::MAX / 2 + alphabetical);
            (slot, within, key, value)
        })
        .collect();
    entries.sort_by_key(|&(slot, within, ..)| (slot, within));
    entries
        .into_iter()
        .map(|(_, _, key, value)| (key, value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire(body: &str, value: &Value) -> String {
        let shape: Shape = serde_json::from_str(body).unwrap();
        String::from_utf8(to_vec(value, &shape).unwrap()).unwrap()
    }

    #[test]
    fn envelope_follows_the_cli_and_caller_content_keeps_its_order() {
        // 调用方乱序发来（像 OpenAI 入口转换后的字母序）；模拟又补了 metadata / thinking 等
        let body = r#"{"tools":[{"input_schema":{"type":"object","properties":{"z":{"type":"string"},"a":{"type":"number"}},"required":["z"]},"name":"f","description":"d"}],
            "stream":true,"model":"m","messages":[{"content":[{"text":"hi","type":"text"},{"input":{"q":1,"b":2},"name":"f","id":"t","type":"tool_use"}],"role":"user"}],"top_p":0.5}"#;
        let mut value: Value = serde_json::from_str(body).unwrap();
        value["max_tokens"] = json!(128_000);
        value["metadata"] = json!({"user_id":"u"});
        value["thinking"] = json!({"display":"updates","type":"adaptive"});
        value["output_config"] = json!({"effort":"medium"});
        value["diagnostics"] = json!({"previous_message_id":null});
        value["system"] =
            json!([{"text":"s","type":"text","cache_control":{"ttl":"1h","type":"ephemeral"}}]);
        value["messages"][0]["content"][0]["cache_control"] = json!({"type":"ephemeral"});
        assert_eq!(
            wire(body, &value),
            r#"{"model":"m","messages":[{"role":"user","content":[{"type":"text","text":"hi","cache_control":{"type":"ephemeral"}},{"type":"tool_use","id":"t","name":"f","input":{"q":1,"b":2}}]}],"system":[{"type":"text","text":"s","cache_control":{"type":"ephemeral","ttl":"1h"}}],"tools":[{"name":"f","description":"d","input_schema":{"type":"object","properties":{"z":{"type":"string"},"a":{"type":"number"}},"required":["z"]}}],"metadata":{"user_id":"u"},"max_tokens":128000,"thinking":{"type":"adaptive","display":"updates"},"top_p":0.5,"output_config":{"effort":"medium"},"diagnostics":{"previous_message_id":null},"stream":true}"#
        );
    }

    #[test]
    fn output_is_the_same_json_and_survives_an_unreadable_shape() {
        let value: Value =
            serde_json::from_str(r#"{"b":[1,{"y":2,"x":[true,null,"\u0001"]}],"a":1.50}"#).unwrap();
        let ordered = String::from_utf8(to_vec(&value, &Shape::Leaf).unwrap()).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&ordered).unwrap(), value);
        assert_eq!(
            ordered,
            r#"{"a":1.50,"b":[1,{"x":[true,null,"\u0001"],"y":2}]}"#
        );
    }
}
