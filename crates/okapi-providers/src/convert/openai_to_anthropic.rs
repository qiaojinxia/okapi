//! OpenAI 协议客户端 → Anthropic 上游：
//! 请求 OpenAI→Anthropic；响应与事件流 Anthropic→OpenAI（chunk 形状），
//! 让 gateway 的首字缓冲/泵送/结算对 provider 完全无感。
//!
//! usage 映射基线：Anthropic 的 `input_tokens` 不含缓存读写，OpenAI 的
//! `prompt_tokens` 含缓存 —— 故 prompt = input + cache_read + cache_creation，
//! `cached_tokens` = cache_read，`cache_write_tokens` = cache_creation（支持 5m / 1h TTL 拆分计价）。

use crate::anthropic::{AnthropicEvent, AnthropicUpstream, MessagesResponse};
use crate::error::UpstreamError;
use crate::openai::{ChatResponse, StreamHandle};
use crate::types::ChatEvent;
use bytes::Bytes;
use futures::StreamExt;
use okapi_api::UsageProbe;
use serde_json::{Value, json};

// ---- 请求转换 ----

/// OpenAI chat 请求 → Anthropic messages 请求。
/// `default_max_tokens`：OpenAI 侧未显式给上限时的兜底（Anthropic 必填 max_tokens）。
pub fn request_openai_to_anthropic(
    body: &Bytes,
    upstream_model: &str,
    default_max_tokens: u32,
) -> Result<Bytes, UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let out = request_openai_to_anthropic_value(&src, upstream_model, default_max_tokens)?;
    serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 在已解析 `Value` 上完成 OpenAI → Anthropic 请求转换（两跳转换直接传入内存对象）。
pub fn request_openai_to_anthropic_value(
    src: &Value,
    upstream_model: &str,
    default_max_tokens: u32,
) -> Result<Value, UpstreamError> {
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    super::single_choice(src.get("n"), "n")?;

    let mut out = serde_json::Map::new();
    out.insert("model".into(), json!(upstream_model));

    let max_tokens = src
        .get("max_completion_tokens")
        .or_else(|| src.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(default_max_tokens));
    out.insert("max_tokens".into(), json!(max_tokens));

    // 顶层 cache_control（Anthropic 自动缓存控制）：原样透传
    let mut has_explicit_cache_control = false;
    if let Some(cc) = src.get("cache_control").filter(|v| v.is_object()) {
        out.insert("cache_control".into(), cc.clone());
        has_explicit_cache_control = true;
    }

    // messages：system/developer 抽为顶层 system；tool 消息并入 user turn；
    // 连续同角色合并（Anthropic 要求 user/assistant 交替）
    let (system_blocks, messages) = convert_messages(src, &mut has_explicit_cache_control)?;
    if !system_blocks.is_empty() {
        if system_blocks.iter().any(|b| b.cache_control.is_some()) {
            has_explicit_cache_control = true;
            let blocks: Vec<Value> = system_blocks
                .into_iter()
                .map(|b| {
                    let mut obj = json!({"type": "text", "text": b.text});
                    attach_cache_control(&mut obj, b.cache_control);
                    obj
                })
                .collect();
            out.insert("system".into(), Value::Array(blocks));
        } else {
            let joined = system_blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            out.insert("system".into(), json!(joined));
        }
    }
    out.insert("messages".into(), Value::Array(messages));

    convert_sampling(src, &mut out);
    match src.get("stop") {
        Some(Value::String(s)) => {
            out.insert("stop_sequences".into(), json!([s]));
        }
        Some(Value::Array(a)) => {
            out.insert("stop_sequences".into(), Value::Array(a.clone()));
        }
        _ => {}
    }

    if convert_tools(src, &mut out) {
        has_explicit_cache_control = true;
    }

    // OpenAI / Responses 客户端用 prompt_cache_key 显式声明会话前缀缓存（如 Codex CLI 两跳打 Anthropic）；
    // 若请求未自带显式 cache_control，则折算为标准 5m ephemeral 断点（tools / system / 末条消息，至多 3 处）。
    if !has_explicit_cache_control
        && src
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    {
        apply_prompt_cache_breakpoints(&mut out);
    }

    Ok(Value::Object(out))
}

fn convert_messages(
    src: &serde_json::Map<String, Value>,
    has_explicit_cache_control: &mut bool,
) -> Result<(Vec<SystemBlock>, Vec<Value>), UpstreamError> {
    let mut system_blocks: Vec<SystemBlock> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for msg in src
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        let msg_cc = cache_control_of(msg);
        match role {
            "system" | "developer" => {
                collect_system_blocks(msg.get("content"), msg_cc, &mut system_blocks);
            }
            "tool" => {
                // 分段内容按块搬（tool_result 能装 text / image / document）：只取 text 会把工具返回的
                // 截图静默丢掉，模型在看不到图的情况下作答；装不下的 part 照常报 400
                let content = match msg.get("content").unwrap_or(&Value::Null) {
                    Value::Array(_) => Value::Array(content_to_blocks(
                        msg.get("content").unwrap_or(&Value::Null),
                        has_explicit_cache_control,
                    )?),
                    other => json!(content_to_text(other)),
                };
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": msg.get("tool_call_id").and_then(Value::as_str).unwrap_or(""),
                    "content": content,
                });
                if attach_cache_control(&mut block, msg_cc) {
                    *has_explicit_cache_control = true;
                }
                push_merged(&mut messages, "user", vec![block]);
            }
            "assistant" => {
                let mut blocks = content_to_blocks(
                    msg.get("content").unwrap_or(&Value::Null),
                    has_explicit_cache_control,
                )?;
                for call in msg
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let f = call.get("function").cloned().unwrap_or(Value::Null);
                    let input = super::tool_arguments(f.get("arguments").and_then(Value::as_str));
                    let mut tool_use = json!({
                        "type": "tool_use",
                        "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                        "name": f.get("name").and_then(Value::as_str).unwrap_or(""),
                        "input": input,
                    });
                    let call_cc = cache_control_of(call).or_else(|| cache_control_of(&f));
                    if attach_cache_control(&mut tool_use, call_cc) {
                        *has_explicit_cache_control = true;
                    }
                    blocks.push(tool_use);
                }
                if let Some(last) = blocks.last_mut()
                    && attach_cache_control(last, msg_cc)
                {
                    *has_explicit_cache_control = true;
                }
                if !blocks.is_empty() {
                    push_merged(&mut messages, "assistant", blocks);
                }
            }
            _ => {
                let mut blocks = content_to_blocks(
                    msg.get("content").unwrap_or(&Value::Null),
                    has_explicit_cache_control,
                )?;
                if let Some(last) = blocks.last_mut()
                    && attach_cache_control(last, msg_cc)
                {
                    *has_explicit_cache_control = true;
                }
                if !blocks.is_empty() {
                    push_merged(&mut messages, "user", blocks);
                }
            }
        }
    }
    Ok((system_blocks, messages))
}

struct SystemBlock {
    text: String,
    cache_control: Option<Value>,
}

fn cache_control_of(value: &Value) -> Option<Value> {
    value
        .get("cache_control")
        .filter(|v| v.is_object())
        .cloned()
}

fn attach_cache_control(block: &mut Value, cc: Option<Value>) -> bool {
    let Some(cc) = cc else {
        return false;
    };
    if let Some(map) = block.as_object_mut()
        && !map.contains_key("cache_control")
    {
        map.insert("cache_control".into(), cc);
        return true;
    }
    false
}

fn collect_system_blocks(
    content: Option<&Value>,
    msg_cc: Option<Value>,
    out: &mut Vec<SystemBlock>,
) {
    match content {
        Some(Value::String(s)) if !s.is_empty() => {
            out.push(SystemBlock {
                text: s.clone(),
                cache_control: msg_cc,
            });
        }
        Some(Value::Array(parts)) => {
            let start = out.len();
            for p in parts {
                if let Some(text) = p
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    out.push(SystemBlock {
                        text: text.to_owned(),
                        cache_control: cache_control_of(p),
                    });
                }
            }
            if let Some(cc) = msg_cc
                && out.len() > start
                && let Some(last) = out.last_mut()
                && last.cache_control.is_none()
            {
                last.cache_control = Some(cc);
            }
        }
        _ => {}
    }
}

fn apply_prompt_cache_breakpoints(out: &mut serde_json::Map<String, Value>) {
    let ephemeral = json!({"type": "ephemeral"});
    if let Some(last_tool) = out
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.last_mut())
    {
        attach_cache_control(last_tool, Some(ephemeral.clone()));
    }
    match out.get_mut("system") {
        Some(Value::String(text)) if !text.is_empty() => {
            let text = std::mem::take(text);
            out.insert(
                "system".into(),
                json!([{"type": "text", "text": text, "cache_control": ephemeral.clone()}]),
            );
        }
        Some(Value::Array(blocks)) => {
            if let Some(last) = blocks.last_mut() {
                attach_cache_control(last, Some(ephemeral.clone()));
            }
        }
        _ => {}
    }
    if let Some(last_block) = out
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|msgs| msgs.last_mut())
        .and_then(|msg| msg.get_mut("content"))
        .and_then(Value::as_array_mut)
        .and_then(|blocks| {
            blocks.iter_mut().rev().find(|b| {
                b.is_object()
                    && !matches!(
                        b.get("type").and_then(Value::as_str),
                        Some("thinking" | "redacted_thinking")
                    )
            })
        })
    {
        attach_cache_control(last_block, Some(ephemeral));
    }
}

/// 采样参数透传，唯 temperature 按方言契约收敛：OpenAI 合法到 2.0，Anthropic
/// messages 全程 0–1，直传 >1 上游必 400 并烧 failover。钳到 1.0 恰是 Anthropic
/// 缺省温度，采样行为不劣化。
fn convert_sampling(
    src: &serde_json::Map<String, Value>,
    out: &mut serde_json::Map<String, Value>,
) {
    for key in ["top_p", "stream"] {
        if let Some(v) = src.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    match src.get("temperature") {
        Some(v) if v.is_number() => {
            let t = v.as_f64().unwrap_or(1.0).clamp(0.0, 1.0);
            out.insert("temperature".into(), json!(t));
        }
        Some(v) => {
            out.insert("temperature".into(), v.clone());
        }
        None => {}
    }
}

/// tools / tool_choice 映射；tool_choice="none" 时整体不带 tools。
/// 返回工具定义里是否携带了显式 `cache_control`。
fn convert_tools(
    src: &serde_json::Map<String, Value>,
    out: &mut serde_json::Map<String, Value>,
) -> bool {
    let choice = src.get("tool_choice");
    if choice.and_then(Value::as_str) == Some("none") {
        return false;
    }
    let mut saw_cache_control = false;
    let tools: Vec<Value> = src
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let f = t.get("function")?;
            let mut tool = json!({
                "name": f.get("name").and_then(Value::as_str)?,
                "description": f.get("description").and_then(Value::as_str).unwrap_or(""),
                "input_schema": f.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
            });
            let cc = cache_control_of(t).or_else(|| cache_control_of(f));
            if attach_cache_control(&mut tool, cc) {
                saw_cache_control = true;
            }
            Some(tool)
        })
        .collect();
    if tools.is_empty() {
        return false;
    }
    out.insert("tools".into(), Value::Array(tools));
    let mut tool_choice = match choice {
        Some(Value::String(s)) if s == "required" => Some(json!({"type": "any"})),
        Some(Value::Object(o)) => o
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .map(|name| json!({"type": "tool", "name": name})),
        _ => None, // auto 缺省
    };
    // parallel_tool_calls:false 对应 tool_choice.disable_parallel_tool_use；缺省的 auto 也要显式写出来，
    // 否则要求一次只调一个工具的 agent 在 Anthropic 上游被放开并行
    if src.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false) {
        let mut choice = tool_choice.unwrap_or_else(|| json!({"type": "auto"}));
        choice["disable_parallel_tool_use"] = json!(true);
        tool_choice = Some(choice);
    }
    if let Some(tool_choice) = tool_choice {
        out.insert("tool_choice".into(), tool_choice);
    }
    saw_cache_control
}

/// 连续同角色 turn 合并（内容块级追加）。
fn push_merged(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.extend(blocks);
        return;
    }
    messages.push(json!({"role": role, "content": blocks}));
}

/// OpenAI content（string | parts 数组）→ Anthropic 内容块；表达不了的 part 报错（见
/// [`super::unsupported_part`]）。空文本 part 跳过：Anthropic 拒收空 text 块。
fn content_to_blocks(
    content: &Value,
    has_cache_control: &mut bool,
) -> Result<Vec<Value>, UpstreamError> {
    match content {
        Value::String(s) if !s.is_empty() => Ok(vec![json!({"type": "text", "text": s})]),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part_to_block(part, has_cache_control).transpose())
            .collect(),
        _ => Ok(Vec::new()),
    }
}

fn part_to_block(
    part: &Value,
    has_cache_control: &mut bool,
) -> Result<Option<Value>, UpstreamError> {
    let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
    let cc = cache_control_of(part);
    let block = match kind {
        // assistant 历史里的 refusal part 是纯文本
        "text" | "refusal" => {
            return Ok(part
                .get(kind)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(|text| {
                    let mut obj = json!({"type": "text", "text": text});
                    if attach_cache_control(&mut obj, cc) {
                        *has_cache_control = true;
                    }
                    obj
                }));
        }
        "image_url" => part
            .pointer("/image_url/url")
            .and_then(Value::as_str)
            .and_then(|url| match url.strip_prefix("data:") {
                Some(rest) => rest.split_once(";base64,").map(|(media_type, data)| {
                    json!({"type": "image",
                        "source": {"type": "base64", "media_type": media_type, "data": data}})
                }),
                None => Some(json!({"type": "image", "source": {"type": "url", "url": url}})),
            }),
        // 内联 PDF → document 块；file_id 引用的是 OpenAI 那边的文件，这里取不到
        "file" => part
            .pointer("/file/file_data")
            .and_then(Value::as_str)
            .and_then(|url| url.strip_prefix("data:application/pdf;base64,"))
            .map(|data| {
                json!({"type": "document",
                    "source": {"type": "base64", "media_type": "application/pdf", "data": data}})
            }),
        _ => None,
    };
    block
        .map(|mut b| {
            if attach_cache_control(&mut b, cc) {
                *has_cache_control = true;
            }
            Some(b)
        })
        .ok_or_else(|| super::unsupported_part(kind))
}

fn content_to_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

// ---- 响应转换（非流式） ----

/// chat 形状装不下的 content block：成功响应已经计费，不为它整单报错，但要留痕——
/// 上游新增块类型或内置工具意外生效时，日志里看得到内容被丢了。
fn unknown_block(kind: Option<&str>) {
    tracing::warn!(
        block_type = kind.unwrap_or("<missing>"),
        "anthropic content block has no chat equivalent; dropped"
    );
}

/// Anthropic message 响应 → OpenAI chat.completion（含 usage 探针）。
pub fn response_anthropic_to_openai(
    body: &Bytes,
) -> Result<(Bytes, Option<UsageProbe>), UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for block in src
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("thinking") => {
                reasoning.push_str(block.get("thinking").and_then(Value::as_str).unwrap_or(""));
            }
            Some("tool_use") => tool_calls.push(json!({
                "id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                "type": "function",
                "function": {
                    "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                    "arguments": super::tool_input_arguments(block.get("input")),
                }
            })),
            // 加密思考 chat 没有承载位；内置工具块（server_tool_use 等）请求侧不发内置工具、正常不会出现
            Some("redacted_thinking") => {}
            other => unknown_block(other),
        }
    }

    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    let stop_reason = src.get("stop_reason").and_then(Value::as_str);
    let usage = usage_from_anthropic(src.get("usage"));
    let out = json!({
        "id": src.get("id").and_then(Value::as_str).unwrap_or("msg"),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": src.get("model").and_then(Value::as_str).unwrap_or(""),
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": map_stop_reason(stop_reason),
        }],
        "usage": usage.map(usage_json),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, usage))
}

fn map_stop_reason(stop: Option<&str>) -> &'static str {
    match stop {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        Some("refusal") => "content_filter",
        // end_turn / stop_sequence / 缺省
        _ => "stop",
    }
}

/// Anthropic usage 对象 → OpenAI 口径探针（prompt 含缓存读写；cached = cache_read）。
#[must_use]
pub fn usage_from_anthropic(usage: Option<&Value>) -> Option<UsageProbe> {
    crate::anthropic_usage::parse(usage)
}

fn usage_json(u: UsageProbe) -> Value {
    u.chat_json()
}

// ---- 事件流转换 ----

/// Anthropic SSE 事件 → OpenAI chunk 的有状态转换器。
/// 事件序列：message_start → content_block_* → message_delta（stop_reason+usage）→ message_stop。
pub struct StreamState {
    id: String,
    model: String,
    created: i64,
    usage: crate::anthropic_usage::StreamUsage,
    /// OpenAI tool_calls 数组下标（Anthropic content block index 与其不同构）。
    tool_index: i64,
    /// 当前 Anthropic block index → 是否 tool_use（input_json_delta 归属判定）。
    current_block_is_tool: bool,
}

impl StreamState {
    #[must_use]
    pub fn new(fallback_model: &str) -> Self {
        Self {
            id: "msg".to_owned(),
            model: fallback_model.to_owned(),
            created: chrono::Utc::now().timestamp(),
            usage: crate::anthropic_usage::StreamUsage::default(),
            tool_index: -1,
            current_block_is_tool: false,
        }
    }

    /// 处理一条上游事件，产出 0..n 条 OpenAI 形状事件。
    pub fn step(
        &mut self,
        item: Result<AnthropicEvent, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        let ev = match item {
            Ok(ev) => ev,
            Err(err) => return vec![Err(err)],
        };
        let data: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
        match ev.event.as_str() {
            "message_start" => self.on_message_start(&data),
            "content_block_start" => self.on_block_start(&data),
            "content_block_delta" => self.on_block_delta(&data),
            "message_delta" => self.on_message_delta(&data),
            "message_stop" => {
                let mut events = Vec::new();
                if let Some(usage) = self.usage.observe("message_stop", &data) {
                    events.push(Ok(self.usage_event(Some(usage))));
                }
                events.push(Ok(ChatEvent::Done));
                events
            }
            // 与直通同一套类型映射：overloaded / rate_limit 走 529 / 429 的退避与冷却
            "error" => vec![Err(crate::anthropic::stream_error(
                &data,
                ev.data.as_bytes(),
            ))],
            // ping / content_block_stop / 未知事件
            _ => Vec::new(),
        }
    }

    fn on_message_start(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        if let Some(m) = data.get("message") {
            if let Some(id) = m.get("id").and_then(Value::as_str) {
                id.clone_into(&mut self.id);
            }
            if let Some(model) = m.get("model").and_then(Value::as_str) {
                model.clone_into(&mut self.model);
            }
        }
        let usage = self.usage.observe("message_start", data);
        // 角色 chunk（content 为空串：不触发首字判定）
        vec![Ok(self.data_event(
            &json!({"role": "assistant", "content": ""}),
            None,
            false,
            0,
            usage,
        ))]
    }

    fn on_block_start(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let block = data.get("content_block");
        let kind = block.and_then(|b| b.get("type")).and_then(Value::as_str);
        if !matches!(
            kind,
            Some("text" | "thinking" | "redacted_thinking" | "tool_use")
        ) {
            unknown_block(kind);
        }
        self.current_block_is_tool = kind == Some("tool_use");
        if !self.current_block_is_tool {
            return Vec::new();
        }
        self.tool_index += 1;
        let delta = json!({"tool_calls": [{
            "index": self.tool_index,
            "id": block.and_then(|b| b.get("id")).and_then(Value::as_str).unwrap_or(""),
            "type": "function",
            "function": {
                "name": block.and_then(|b| b.get("name")).and_then(Value::as_str).unwrap_or(""),
                "arguments": "",
            }
        }]});
        vec![Ok(self.data_event(&delta, None, true, 0, None))]
    }

    fn on_block_delta(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(delta) = data.get("delta") else {
            return Vec::new();
        };
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                let chars = text.chars().count();
                vec![Ok(self.data_event(
                    &json!({"content": text}),
                    None,
                    true,
                    chars,
                    None,
                ))]
            }
            Some("thinking_delta") => {
                let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                let chars = text.chars().count();
                vec![Ok(self.data_event(
                    &json!({"reasoning_content": text}),
                    None,
                    true,
                    chars,
                    None,
                ))]
            }
            // 没有打开的 tool_use 块就没有可挂的槽位（server_tool_use 等被丢弃的块也会流 JSON 增量）：
            // 硬挂到 index 0 会把参数拼进别的工具调用里
            Some("input_json_delta") if self.current_block_is_tool => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let chars = partial.chars().count();
                let d = json!({"tool_calls": [{
                    "index": self.tool_index,
                    "function": {"arguments": partial},
                }]});
                vec![Ok(self.data_event(&d, None, true, chars, None))]
            }
            // signature_delta 等
            _ => Vec::new(),
        }
    }

    fn on_message_delta(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let stop_reason = data
            .get("delta")
            .and_then(|d| d.get("stop_reason"))
            .and_then(Value::as_str);
        let usage = self.usage.observe("message_delta", data);
        vec![
            Ok(self.data_event(
                &json!({}),
                Some(map_stop_reason(stop_reason)),
                false,
                0,
                None,
            )),
            Ok(self.usage_event(usage)),
        ]
    }

    /// 常规 chunk（choices[0].delta）。
    fn data_event(
        &self,
        delta: &Value,
        finish_reason: Option<&str>,
        has_output: bool,
        content_chars: usize,
        usage: Option<UsageProbe>,
    ) -> ChatEvent {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }],
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output,
            content_chars,
            usage,
        }
    }

    /// 终端 usage chunk（choices 为空，OpenAI include_usage 形状）。
    fn usage_event(&self, usage: Option<UsageProbe>) -> ChatEvent {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [],
            "usage": usage.map(usage_json),
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage,
        }
    }
}

// ---- 组合入口（gateway 用） ----

/// OpenAI 协议入口 + Anthropic 上游 的完整一跳：
/// `body_anthropic` 已由 `request_openai_to_anthropic` 转换（forward 循环按候选构造）。
/// 返回 OpenAI 形状的 `ChatResponse`，gateway 泵送与结算零改动。
pub async fn chat(
    upstream: &AnthropicUpstream,
    api_base: &str,
    credential: &str,
    body_anthropic: Bytes,
    upstream_model: &str,
    stream: bool,
    outbound: &crate::http::Outbound,
) -> Result<ChatResponse, UpstreamError> {
    let resp = upstream
        .messages(api_base, credential, body_anthropic, stream, outbound)
        .await?;
    wrap_messages(resp, upstream_model)
}

/// 已取到的 Anthropic 响应 → OpenAI 形状（纯转换半跳）。Bedrock / Vertex 这类只换传输的
/// 上游取到 `MessagesResponse` 后走这里，与直连共享同一套转换（IMPLEMENTATION §11.35）。
pub fn wrap_messages(
    resp: MessagesResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    match resp {
        MessagesResponse::Json {
            status,
            upstream_request_id,
            body,
        } => {
            let (body, usage) = response_anthropic_to_openai(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            })
        }
        MessagesResponse::Stream(h) => {
            let mut st = StreamState::new(upstream_model);
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(st.step(item)));
            Ok(ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            }))
        }
    }
}
