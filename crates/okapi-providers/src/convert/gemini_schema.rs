//! OpenAI 工具参数的 JSON Schema → Gemini `functionDeclarations[].parameters` 收得下的 OpenAPI 子集。
//!
//! Gemini 的 `Schema` 只认一小撮字段，多一个就整条请求 400（`Unknown name "additionalProperties" at 'tools[0]…'`）。
//! OpenAI 侧的客户端却几乎每个对象都带 `additionalProperties` / `$schema`，pydantic / zod 导出的还有 `$ref` / `$defs`、
//! `anyOf: [{…}, {type: null}]`、`type: ["string", "null"]`、整数 enum、`format: uri`、不在 `properties` 里的 `required`……
//! 这里只留 Gemini 认的字段，能等价改写的改写（引用内联、null 变 `nullable`、`oneOf` 变 `anyOf`、`allOf` 合并），
//! 不能的丢掉。丢掉的是「约束」而不是「结构」：模型照样看得到有哪些参数、各是什么类型。
//!
//! 约束与已知的有损之处：
//! - 递归类型（`$ref` 绕回自己）和外部 / 悬空引用收成一个裸 `{"type": "object"}`；
//! - 没有 `properties` 的对象（自由形式的字典）Gemini 描述不了，只剩 `{"type": "object"}`；
//! - 数组没写元素类型时补 `items: {"type": "string"}`，没有任何类型线索的叶子当字符串。

use serde_json::{Map, Value, json};

/// Gemini `Schema` 认的字段；别的一律不发。
const ALLOWED: [&str; 22] = [
    "anyOf",
    "default",
    "description",
    "enum",
    "example",
    "format",
    "items",
    "maxItems",
    "maxLength",
    "maxProperties",
    "maximum",
    "minItems",
    "minLength",
    "minProperties",
    "minimum",
    "nullable",
    "pattern",
    "properties",
    "propertyOrdering",
    "required",
    "title",
    "type",
];
/// proto 里是 int64 的边界：JSON 里写成 `3.0` 的要改成 `3`。
const INTEGER_BOUNDS: [&str; 6] = [
    "maxItems",
    "maxLength",
    "maxProperties",
    "minItems",
    "minLength",
    "minProperties",
];
/// 嵌套深度上限：再深的只留类型，不再往下展开。
const MAX_DEPTH: usize = 32;
/// 沿 `$ref` 往下展开的层数上限。
const MAX_REF_DEPTH: usize = 6;

/// 清洗一个函数的参数 schema。函数没有参数（对象且没有属性）返回 `None`：Gemini 不收 `properties: {}`，
/// 这样的函数不带 `parameters` 字段。
#[must_use]
pub fn function_parameters(schema: &Value) -> Option<Value> {
    let cleaned = Cleaner { root: schema }.clean(schema, "object", 0, &mut Vec::new());
    let object = cleaned.as_object()?;
    let no_properties = object
        .get("properties")
        .and_then(Value::as_object)
        .is_none_or(Map::is_empty);
    if object.get("type").and_then(Value::as_str) == Some("object") && no_properties {
        return None;
    }
    Some(cleaned)
}

struct Cleaner<'a> {
    /// 整份 schema：`$ref: "#/$defs/X"` 从这里取。
    root: &'a Value,
}

impl Cleaner<'_> {
    /// `fallback`：没有任何类型线索时用的类型（函数顶层是 object，其余叶子是 string）。
    fn clean(&self, node: &Value, fallback: &str, depth: usize, refs: &mut Vec<String>) -> Value {
        let Some(src) = node.as_object() else {
            // 布尔 schema 之类：收成一个对象，别让整条请求为它失败
            return json!({"type": "object"});
        };
        if depth >= MAX_DEPTH {
            return Self::cut(src);
        }
        let mut own = self.keywords(src, depth, refs);
        if let Some(reference) = src.get("$ref").and_then(Value::as_str) {
            // `$ref` 旁边的关键字（description / default 之类）补充目标
            let mut resolved = self.follow(reference, depth, refs);
            for (key, value) in own {
                resolved.insert(key, value);
            }
            own = resolved;
        }
        Self::finish(own, fallback)
    }

    /// 展开一个 `$ref`：只认文档内的 JSON pointer；循环、过深、外部或悬空的收成裸对象。
    fn follow(&self, reference: &str, depth: usize, refs: &mut Vec<String>) -> Map<String, Value> {
        let target = reference
            .strip_prefix('#')
            .and_then(|pointer| self.root.pointer(pointer));
        let resolved = match target {
            Some(target)
                if refs.len() < MAX_REF_DEPTH && !refs.iter().any(|seen| seen == reference) =>
            {
                refs.push(reference.to_owned());
                let cleaned = self.clean(target, "object", depth + 1, refs);
                refs.pop();
                cleaned
            }
            _ => json!({"type": "object"}),
        };
        match resolved {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    /// 一个节点自己的关键字（不含类型兜底）。
    fn keywords(
        &self,
        src: &Map<String, Value>,
        depth: usize,
        refs: &mut Vec<String>,
    ) -> Map<String, Value> {
        let mut out = Map::new();
        // allOf：各段清洗后合并进来，节点自己写的关键字随后覆盖
        for element in src
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Value::Object(part) = self.clean(element, "object", depth + 1, refs) {
                merge(&mut out, part);
            }
        }
        let mut own = Map::new();
        if let Some(value) = src.get("type") {
            apply_type(value, &mut own);
        }
        if src.get("nullable") == Some(&Value::Bool(true)) {
            own.insert("nullable".into(), Value::Bool(true));
        }
        for key in ["description", "title", "pattern"] {
            if let Some(Value::String(text)) = src.get(key) {
                own.insert(key.into(), Value::String(text.clone()));
            }
        }
        if let Some(format) = src.get("format").and_then(Value::as_str) {
            own.insert("format".into(), json!(format));
        }
        if let Some(value) = src.get("default")
            && matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
        {
            own.insert("default".into(), value.clone());
        }
        if let Some(value) = src.get("example") {
            own.insert("example".into(), value.clone());
        }
        for key in ["minimum", "maximum"] {
            if let Some(number @ Value::Number(_)) = src.get(key) {
                own.insert(key.into(), number.clone());
            }
        }
        for key in INTEGER_BOUNDS {
            if let Some(bound) = src.get(key).and_then(whole_number) {
                own.insert(key.into(), json!(bound));
            }
        }
        if let Some(Value::Array(values)) = src.get("enum") {
            let mut texts: Vec<Value> = Vec::new();
            let mut all_strings = true;
            for value in values {
                match value {
                    Value::Null => {
                        own.insert("nullable".into(), Value::Bool(true));
                    }
                    Value::String(_) => texts.push(value.clone()),
                    _ => all_strings = false,
                }
            }
            if all_strings && !texts.is_empty() {
                own.insert("enum".into(), Value::Array(texts));
            }
        } else if let Some(Value::String(only)) = src.get("const") {
            own.insert("enum".into(), json!([only]));
        }
        if let Some(Value::Object(properties)) = src.get("properties") {
            let cleaned: Map<String, Value> = properties
                .iter()
                .map(|(name, schema)| (name.clone(), self.clean(schema, "string", depth + 1, refs)))
                .collect();
            own.insert("properties".into(), Value::Object(cleaned));
        }
        if let Some(Value::Array(names)) = src.get("required") {
            own.insert("required".into(), Value::Array(names.clone()));
        }
        if let Some(Value::Array(order)) = src.get("propertyOrdering") {
            own.insert("propertyOrdering".into(), Value::Array(order.clone()));
        }
        match src.get("items") {
            Some(item @ Value::Object(_)) => {
                own.insert("items".into(), self.clean(item, "string", depth + 1, refs));
            }
            // 元组写法：Gemini 只有一个元素类型，取第一个
            Some(Value::Array(items)) if !items.is_empty() => {
                own.insert(
                    "items".into(),
                    self.clean(&items[0], "string", depth + 1, refs),
                );
            }
            _ => {}
        }
        self.alternatives(src, &mut own, depth, refs);
        merge_over(&mut out, own);
        out
    }

    /// `anyOf` / `oneOf`：`{"type": "null"}` 的分支变成 `nullable`；只剩一个分支就并进本节点；多于一个留 `anyOf`。
    fn alternatives(
        &self,
        src: &Map<String, Value>,
        own: &mut Map<String, Value>,
        depth: usize,
        refs: &mut Vec<String>,
    ) {
        let variants: Vec<&Value> = ["anyOf", "oneOf"]
            .iter()
            .filter_map(|key| src.get(*key).and_then(Value::as_array))
            .flatten()
            .collect();
        if variants.is_empty() {
            return;
        }
        let mut kept: Vec<Value> = Vec::new();
        for variant in variants {
            if is_null_schema(variant) {
                own.insert("nullable".into(), Value::Bool(true));
            } else {
                kept.push(self.clean(variant, "string", depth + 1, refs));
            }
        }
        match kept.len() {
            0 => {}
            1 => {
                if let Some(Value::Object(only)) = kept.pop() {
                    // 节点自己写的关键字（description / default 之类）优先于分支里的
                    let mut merged = only;
                    for (key, value) in std::mem::take(own) {
                        merged.insert(key, value);
                    }
                    *own = merged;
                }
            }
            _ => {
                own.insert("anyOf".into(), Value::Array(kept));
            }
        }
    }

    /// 清洗完一个节点之后统一收尾：补类型、补 `items`、去掉不成立的 `required` / `enum` / `format`。
    fn finish(mut map: Map<String, Value>, fallback: &str) -> Value {
        map.retain(|key, _| ALLOWED.contains(&key.as_str()));
        if !map.contains_key("type") && !map.contains_key("anyOf") {
            let inferred = if map.contains_key("properties") {
                "object"
            } else if map.contains_key("items") {
                "array"
            } else {
                fallback
            };
            map.insert("type".into(), json!(inferred));
        }
        let kind = map.get("type").and_then(Value::as_str).map(str::to_owned);
        match kind.as_deref() {
            Some("array") => {
                map.entry("items")
                    .or_insert_with(|| json!({"type": "string"}));
            }
            Some("object") => {
                let names: Vec<String> = map
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|properties| properties.keys().cloned().collect())
                    .unwrap_or_default();
                if names.is_empty() {
                    map.remove("properties");
                    map.remove("required");
                    map.remove("propertyOrdering");
                } else {
                    for key in ["required", "propertyOrdering"] {
                        if let Some(Value::Array(listed)) = map.get_mut(key) {
                            listed.retain(|name| {
                                name.as_str()
                                    .is_some_and(|name| names.iter().any(|n| n == name))
                            });
                            if listed.is_empty() {
                                map.remove(key);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if kind.as_deref().is_some_and(|kind| kind != "string") {
            map.remove("enum");
        }
        let allowed_format: &[&str] = match kind.as_deref() {
            Some("string") => &["date-time", "enum"],
            Some("integer") => &["int32", "int64"],
            Some("number") => &["float", "double"],
            _ => &[],
        };
        if map
            .get("format")
            .and_then(Value::as_str)
            .is_some_and(|format| !allowed_format.contains(&format))
        {
            map.remove("format");
        }
        Value::Object(map)
    }

    /// 超出深度上限的节点：只留类型和说明。
    fn cut(src: &Map<String, Value>) -> Value {
        let mut out = Map::new();
        if let Some(value) = src.get("type") {
            apply_type(value, &mut out);
        }
        if let Some(Value::String(text)) = src.get("description") {
            out.insert("description".into(), Value::String(text.clone()));
        }
        Self::finish(out, "object")
    }
}

/// `type` 可以是字符串，也可以是 `["string", "null"]`：第一个非 null 的当类型，有 null 就 `nullable`。
fn apply_type(value: &Value, out: &mut Map<String, Value>) {
    let names: Vec<&str> = match value {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
        _ => return,
    };
    for name in names {
        match name.trim().to_ascii_lowercase().as_str() {
            "null" => {
                out.insert("nullable".into(), Value::Bool(true));
            }
            kind @ ("object" | "array" | "string" | "integer" | "number" | "boolean") => {
                out.entry("type").or_insert_with(|| json!(kind));
            }
            _ => {}
        }
    }
}

fn is_null_schema(value: &Value) -> bool {
    match value.get("type") {
        Some(Value::String(name)) => name.trim().eq_ignore_ascii_case("null"),
        Some(Value::Array(names)) => {
            !names.is_empty()
                && names.iter().all(|name| {
                    name.as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case("null"))
                })
        }
        _ => false,
    }
}

/// 整数值（`3` 或 `3.0`）；别的都不是边界。
#[allow(clippy::cast_possible_truncation)]
fn whole_number(value: &Value) -> Option<i64> {
    if let Some(integer) = value.as_i64() {
        return (integer >= 0).then_some(integer);
    }
    let float = value.as_f64()?;
    (float >= 0.0 && float.fract() == 0.0 && float <= 9.0e15).then_some(float as i64)
}

/// 把 `part` 合并进 `into`：`properties` 取并集，`required` 取并集，其余先到先得。
fn merge(into: &mut Map<String, Value>, part: Map<String, Value>) {
    for (key, value) in part {
        match (key.as_str(), into.get_mut(&key)) {
            ("properties", Some(Value::Object(existing))) => {
                if let Value::Object(more) = value {
                    for (name, schema) in more {
                        existing.entry(name).or_insert(schema);
                    }
                }
            }
            ("required", Some(Value::Array(existing))) => {
                if let Value::Array(more) = value {
                    for name in more {
                        if !existing.contains(&name) {
                            existing.push(name);
                        }
                    }
                }
            }
            (_, Some(_)) => {}
            (_, None) => {
                into.insert(key, value);
            }
        }
    }
}

/// 把 `own`（节点自己的关键字）盖到 `into`（allOf 合并出来的底）上：`properties`、`required` 取并集，其余 `own` 优先。
fn merge_over(into: &mut Map<String, Value>, own: Map<String, Value>) {
    for (key, value) in own {
        match (key.as_str(), into.get_mut(&key)) {
            ("properties", Some(Value::Object(existing))) => {
                if let Value::Object(more) = value {
                    for (name, schema) in more {
                        existing.insert(name, schema);
                    }
                }
            }
            ("required", Some(Value::Array(existing))) => {
                if let Value::Array(more) = value {
                    for name in more {
                        if !existing.contains(&name) {
                            existing.push(name);
                        }
                    }
                }
            }
            _ => {
                into.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pydantic / zod 导出的典型形状：严格模式的 `additionalProperties`、`$schema`、`$defs` + `$ref`（含自引用）、
    /// `anyOf` 里的 null、类型数组、不在属性里的 required、Gemini 不认的 format。
    #[test]
    fn a_typical_exported_schema_is_reduced_to_the_gemini_subset() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Args",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "city": {"type": "string", "description": "City", "minLength": 1},
                "unit": {"anyOf": [{"type": "string", "enum": ["c", "f"]}, {"type": "null"}], "default": null},
                "when": {"type": "string", "format": "date-time"},
                "link": {"type": "string", "format": "uri"},
                "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 3.0},
                "home": {"$ref": "#/$defs/Address", "description": "Where"},
                "mixed": {"type": ["integer", "null"]}
            },
            "required": ["city", "ghost"],
            "$defs": {
                "Address": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "street": {"type": "string"},
                        "next": {"$ref": "#/$defs/Address"}
                    },
                    "required": ["street"]
                }
            }
        });
        assert_eq!(
            function_parameters(&schema),
            Some(json!({
                "title": "Args",
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "City", "minLength": 1},
                    "unit": {"type": "string", "enum": ["c", "f"], "nullable": true},
                    "when": {"type": "string", "format": "date-time"},
                    "link": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 3},
                    "home": {
                        "type": "object",
                        "description": "Where",
                        "properties": {
                            "street": {"type": "string"},
                            "next": {"type": "object"}
                        },
                        "required": ["street"]
                    },
                    "mixed": {"type": "integer", "nullable": true}
                },
                "required": ["city"]
            }))
        );
    }

    #[test]
    fn a_function_without_arguments_has_no_parameters() {
        for schema in [
            json!({}),
            json!({"type": "object"}),
            json!({"type": "object", "properties": {}}),
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            json!(true),
        ] {
            assert_eq!(function_parameters(&schema), None, "{schema}");
        }
        assert!(
            function_parameters(
                &json!({"type": "object", "properties": {"a": {"type": "string"}}})
            )
            .is_some()
        );
    }

    #[test]
    fn all_of_is_merged_and_one_of_becomes_any_of() {
        let merged = function_parameters(&json!({
            "allOf": [
                {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
                {"type": "object", "properties": {"b": {"type": "integer"}}, "required": ["b"]}
            ],
            "properties": {"c": {"type": "boolean"}}
        }))
        .unwrap();
        assert_eq!(
            merged,
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": "string"},
                    "b": {"type": "integer"},
                    "c": {"type": "boolean"}
                },
                "required": ["a", "b"]
            })
        );
        let either = function_parameters(&json!({
            "type": "object",
            "properties": {"v": {"oneOf": [{"type": "string"}, {"type": "integer"}]}}
        }))
        .unwrap();
        assert_eq!(
            either["properties"]["v"],
            json!({"anyOf": [{"type": "string"}, {"type": "integer"}]})
        );
    }

    #[test]
    fn enums_keep_only_what_gemini_accepts() {
        let cleaned = function_parameters(&json!({
            "type": "object",
            "properties": {
                "level": {"type": "integer", "enum": [1, 2, 3]},
                "mode": {"type": "string", "enum": ["fast", "slow", null]},
                "flag": {"const": "yes"},
                "mixed": {"type": "string", "enum": ["a", 1]},
                "digits": {"type": "integer", "enum": ["1", "2"]}
            }
        }))
        .unwrap();
        let properties = &cleaned["properties"];
        assert_eq!(properties["level"], json!({"type": "integer"}));
        assert_eq!(
            properties["digits"],
            json!({"type": "integer"}),
            "Gemini only takes enums on strings"
        );
        assert_eq!(
            properties["mode"],
            json!({"type": "string", "enum": ["fast", "slow"], "nullable": true})
        );
        assert_eq!(
            properties["flag"],
            json!({"type": "string", "enum": ["yes"]})
        );
        assert_eq!(properties["mixed"], json!({"type": "string"}));
    }

    #[test]
    fn untyped_and_degenerate_nodes_get_a_type_gemini_can_use() {
        let cleaned = function_parameters(&json!({
            "properties": {
                "anything": {"title": "Value"},
                "list": {"type": "array"},
                "tuple": {"type": "array", "items": [{"type": "number"}, {"type": "string"}]},
                "dangling": {"$ref": "#/$defs/Missing"},
                "outside": {"$ref": "https://example.com/schema.json"},
                "nothing": true
            }
        }))
        .unwrap();
        let properties = &cleaned["properties"];
        assert_eq!(
            properties["anything"],
            json!({"title": "Value", "type": "string"})
        );
        assert_eq!(
            properties["list"],
            json!({"type": "array", "items": {"type": "string"}})
        );
        assert_eq!(
            properties["tuple"],
            json!({"type": "array", "items": {"type": "number"}})
        );
        for name in ["dangling", "outside", "nothing"] {
            assert_eq!(properties[name], json!({"type": "object"}), "{name}");
        }
        assert_eq!(
            cleaned["type"], "object",
            "a function's parameters are always an object"
        );
    }

    fn only_allowed_keys(value: &Value, at: &str) {
        let Some(object) = value.as_object() else {
            return;
        };
        for (key, child) in object {
            assert!(ALLOWED.contains(&key.as_str()), "{key} at {at}");
            match key.as_str() {
                "properties" => {
                    for (name, schema) in child.as_object().unwrap() {
                        only_allowed_keys(schema, &format!("{at}/{name}"));
                    }
                }
                "items" => only_allowed_keys(child, &format!("{at}/items")),
                "anyOf" => {
                    for (index, variant) in child.as_array().unwrap().iter().enumerate() {
                        only_allowed_keys(variant, &format!("{at}/anyOf/{index}"));
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn nothing_outside_the_gemini_fields_survives_and_cleaning_is_idempotent() {
        let nasty = json!({
            "$id": "x", "$schema": "s", "$comment": "c", "definitions": {"D": {"type": "string"}},
            "type": "object", "additionalProperties": {"type": "string"}, "patternProperties": {"^a": {}},
            "unevaluatedProperties": false, "not": {}, "if": {}, "then": {}, "else": {},
            "properties": {
                "a": {"type": "number", "exclusiveMinimum": 0, "multipleOf": 2, "minimum": 1, "readOnly": true, "deprecated": true},
                "b": {"type": "string", "contentEncoding": "base64", "examples": ["x"], "format": "email"},
                "c": {"type": "array", "uniqueItems": true, "items": {"$ref": "#/definitions/D"}, "contains": {}},
                "d": {"$ref": "#/definitions/D", "x-extension": 1}
            }
        });
        let once = function_parameters(&nasty).unwrap();
        only_allowed_keys(&once, "");
        assert_eq!(
            once["properties"]["c"]["items"],
            json!({"type": "string"}),
            "definitions are resolved too"
        );
        assert_eq!(
            function_parameters(&once),
            Some(once.clone()),
            "a cleaned schema is a fixed point"
        );
    }

    #[test]
    fn a_very_deep_schema_terminates() {
        let mut schema = json!({"type": "string", "description": "leaf"});
        for _ in 0..200 {
            schema = json!({"type": "object", "properties": {"next": schema}});
        }
        let cleaned = function_parameters(&schema).unwrap();
        only_allowed_keys(&cleaned, "");
        let mut depth = 0;
        let mut node = &cleaned;
        while let Some(next) = node.pointer("/properties/next") {
            node = next;
            depth += 1;
        }
        assert!(depth <= MAX_DEPTH, "cut at the depth limit, got {depth}");
    }
}
