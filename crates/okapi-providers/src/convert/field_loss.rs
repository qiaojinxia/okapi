//! 跨方言转换中被丢弃的非工具请求特性。
//!
//! 工具丢失由 [`super::tool_loss`] 负责（参与路由策略 `tool_loss_policy` 决策）；
//! 本模块补充其余跨方言无法保真的关键字段（缓存断点、多轮思考历史、结构化输出等），
//! 写入请求诊断 `dropped_fields`，让管理端日志直接看到发生了哪些静默降级。
use crate::registry::ApiFormat;
use serde_json::Value;

/// 检测一次跨方言请求转换中，客户端显式携带但目标方言无法保真的特性名（去重、固定顺序）。
///
/// 同方言透传直接返回空列表；只报告**确实无法翻译**的字段，已由转换器保真或翻译的不算丢失。
#[must_use]
pub fn dropped_fields(from: ApiFormat, to: ApiFormat, source: &Value) -> Vec<&'static str> {
    if from == to {
        return Vec::new();
    }
    let Some(obj) = source.as_object() else {
        return Vec::new();
    };

    let mut dropped = Vec::new();

    // 1. 显式 cache_control 断点：仅 Anthropic Messages 原生支持（openai_to_anthropic 已保真透传）
    if to != ApiFormat::Messages && has_cache_control(from, obj) {
        dropped.push("cache_control");
    }

    // 2. prompt_cache_key：Chat / Responses 原样透传，Anthropic 翻译为 ephemeral 断点，Gemini 无对应
    if to == ApiFormat::Gemini && has_non_empty_str(obj.get("prompt_cache_key")) {
        dropped.push("prompt_cache_key");
    }

    // 3. prompt_cache_retention：Chat / Responses 透传，Anthropic / Gemini 无对应
    if !matches!(to, ApiFormat::ChatCompletions | ApiFormat::Responses)
        && obj
            .get("prompt_cache_retention")
            .is_some_and(|v| !v.is_null())
    {
        dropped.push("prompt_cache_retention");
    }

    // 4. Gemini 显式上下文缓存句柄 cachedContent：非 Gemini 上游无法引用
    if from == ApiFormat::Gemini
        && (has_non_empty_str(obj.get("cachedContent"))
            || has_non_empty_str(obj.get("cached_content")))
    {
        dropped.push("cached_content");
    }

    // 5. 多轮历史里的推理/思考块：跨方言无输入回放槽位
    if has_thinking_history(from, obj) {
        dropped.push("thinking_history");
    }

    // 6. service_tier：Chat ↔ Responses 双向透传，Anthropic / Gemini 无对应
    if !matches!(to, ApiFormat::ChatCompletions | ApiFormat::Responses)
        && has_non_empty_str(obj.get("service_tier"))
    {
        dropped.push("service_tier");
    }

    // 7. 结构化输出格式：Anthropic Messages 转换器当前不翻译 response_format / text.format / responseSchema
    if to == ApiFormat::Messages && has_structured_output(from, obj) {
        dropped.push("response_format");
    }

    // 8. Anthropic 专有扩展字段：context_management / output_config
    if from == ApiFormat::Messages {
        if obj.get("context_management").is_some_and(|v| !v.is_null()) {
            dropped.push("context_management");
        }
        if let Some(output_config) = obj.get("output_config").and_then(Value::as_object) {
            if output_config.get("format").is_some_and(|v| !v.is_null()) {
                dropped.push("output_config.format");
            }
            // 未带 thinking 时仅带 output_config.effort 不会自动打开跨方言思考，故该 effort 实际未生效
            if obj.get("thinking").is_none() && has_non_empty_str(output_config.get("effort")) {
                dropped.push("output_config.effort");
            }
        }
    }

    dropped
}

fn has_non_empty_str(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|s| !s.is_empty())
}

fn has_cache_control_obj(value: &Value) -> bool {
    value.get("cache_control").is_some_and(Value::is_object)
}

/// 精确扫描协议位置上的 `cache_control`（不误扫 `tool_use.input` 等用户载荷里的同名键）。
fn has_cache_control(from: ApiFormat, obj: &serde_json::Map<String, Value>) -> bool {
    if obj.get("cache_control").is_some_and(Value::is_object) {
        return true;
    }
    if obj
        .get("system")
        .and_then(Value::as_array)
        .is_some_and(|blocks| blocks.iter().any(has_cache_control_obj))
    {
        return true;
    }
    if obj
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|t| {
                has_cache_control_obj(t) || t.get("function").is_some_and(has_cache_control_obj)
            })
        })
    {
        return true;
    }
    let check_blocks = |blocks: &[Value]| -> bool {
        blocks.iter().any(|block| {
            has_cache_control_obj(block)
                || (block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block
                        .get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|nested| nested.iter().any(has_cache_control_obj)))
        })
    };
    match from {
        ApiFormat::Messages | ApiFormat::ChatCompletions => obj
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|msgs| {
                msgs.iter().any(|msg| {
                    has_cache_control_obj(msg)
                        || msg
                            .get("content")
                            .and_then(Value::as_array)
                            .is_some_and(|blocks| check_blocks(blocks))
                        || msg
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .is_some_and(|calls| {
                                calls.iter().any(|call| {
                                    has_cache_control_obj(call)
                                        || call.get("function").is_some_and(has_cache_control_obj)
                                })
                            })
                })
            }),
        ApiFormat::Responses | ApiFormat::ResponsesCompact => obj
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    has_cache_control_obj(item)
                        || item
                            .get("content")
                            .and_then(Value::as_array)
                            .is_some_and(|parts| parts.iter().any(has_cache_control_obj))
                })
            }),
        ApiFormat::Gemini => false,
    }
}

fn has_thinking_history(from: ApiFormat, obj: &serde_json::Map<String, Value>) -> bool {
    match from {
        ApiFormat::Messages => obj
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|msgs| {
                msgs.iter().any(|msg| {
                    msg.get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| {
                            blocks.iter().any(|b| {
                                matches!(
                                    b.get("type").and_then(Value::as_str),
                                    Some("thinking" | "redacted_thinking")
                                )
                            })
                        })
                })
            }),
        ApiFormat::Responses | ApiFormat::ResponsesCompact => obj
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            }),
        ApiFormat::Gemini => {
            obj.get("contents")
                .and_then(Value::as_array)
                .is_some_and(|contents| {
                    contents.iter().any(|content| {
                        content
                            .get("parts")
                            .and_then(Value::as_array)
                            .is_some_and(|parts| {
                                parts.iter().any(|p| {
                                    p.get("thought").and_then(Value::as_bool) == Some(true)
                                })
                            })
                    })
                })
        }
        ApiFormat::ChatCompletions => {
            obj.get("messages")
                .and_then(Value::as_array)
                .is_some_and(|msgs| {
                    msgs.iter()
                        .any(|msg| super::chat_reasoning_text(msg).is_some())
                })
        }
    }
}

fn has_structured_output(from: ApiFormat, obj: &serde_json::Map<String, Value>) -> bool {
    match from {
        ApiFormat::ChatCompletions => obj.get("response_format").is_some_and(|fmt| {
            !fmt.is_null() && fmt.get("type").and_then(Value::as_str) != Some("text")
        }),
        ApiFormat::Responses | ApiFormat::ResponsesCompact => obj
            .get("text")
            .and_then(|t| t.get("format"))
            .is_some_and(|fmt| {
                !fmt.is_null() && fmt.get("type").and_then(Value::as_str) != Some("text")
            }),
        ApiFormat::Gemini => obj
            .get("generationConfig")
            .or_else(|| obj.get("generation_config"))
            .is_some_and(|cfg| {
                cfg.get("responseSchema")
                    .or_else(|| cfg.get("response_schema"))
                    .or_else(|| cfg.get("responseJsonSchema"))
                    .or_else(|| cfg.get("response_json_schema"))
                    .is_some_and(|v| !v.is_null())
                    || cfg
                        .get("responseMimeType")
                        .or_else(|| cfg.get("response_mime_type"))
                        .and_then(Value::as_str)
                        == Some("application/json")
            }),
        ApiFormat::Messages => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_dialect_reports_nothing() {
        let body = json!({
            "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "assistant", "content": [{"type": "thinking", "thinking": "hmm"}]}],
            "context_management": {"edits": []}
        });
        assert_eq!(
            dropped_fields(ApiFormat::Messages, ApiFormat::Messages, &body),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn anthropic_to_openai_flags_cache_thinking_history_and_extensions() {
        let body = json!({
            "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "step 1", "signature": "sig"},
                    {"type": "text", "text": "done"}
                ]}
            ],
            "context_management": {"edits": []},
            "output_config": {"effort": "high", "format": {"type": "json_schema"}}
        });
        assert_eq!(
            dropped_fields(ApiFormat::Messages, ApiFormat::ChatCompletions, &body),
            vec![
                "cache_control",
                "thinking_history",
                "context_management",
                "output_config.format",
                "output_config.effort",
            ]
        );
        // 带 thinking 时 output_config.effort 已走统一推理管线翻译，不再报丢失
        let mut with_thinking = body.clone();
        with_thinking["thinking"] = json!({"type": "adaptive"});
        assert_eq!(
            dropped_fields(
                ApiFormat::Messages,
                ApiFormat::ChatCompletions,
                &with_thinking
            ),
            vec![
                "cache_control",
                "thinking_history",
                "context_management",
                "output_config.format",
            ]
        );
    }

    #[test]
    fn openai_to_anthropic_preserves_cache_control_and_prompt_cache_key() {
        let body = json!({
            "prompt_cache_key": "sess-1",
            "service_tier": "priority",
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}]},
                {"role": "user", "content": "hi"}
            ]
        });
        assert_eq!(
            dropped_fields(ApiFormat::ChatCompletions, ApiFormat::Messages, &body),
            vec!["service_tier", "response_format"]
        );
    }

    #[test]
    fn responses_to_chat_preserves_prompt_cache_key_and_service_tier() {
        let body = json!({
            "prompt_cache_key": "sess-1",
            "prompt_cache_retention": "24h",
            "service_tier": "priority",
            "input": [
                {"type": "reasoning", "id": "rs_1", "summary": []},
                {"role": "user", "content": "hi"}
            ]
        });
        assert_eq!(
            dropped_fields(ApiFormat::Responses, ApiFormat::ChatCompletions, &body),
            vec!["thinking_history"]
        );
        assert_eq!(
            dropped_fields(ApiFormat::Responses, ApiFormat::Gemini, &body),
            vec![
                "prompt_cache_key",
                "prompt_cache_retention",
                "thinking_history",
                "service_tier",
            ]
        );
    }

    #[test]
    fn tool_use_input_named_cache_control_is_not_a_false_positive() {
        let body = json!({
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "t1",
                    "name": "Edit",
                    "input": {"cache_control": {"type": "ephemeral"}}
                }]
            }]
        });
        assert_eq!(
            dropped_fields(ApiFormat::Messages, ApiFormat::ChatCompletions, &body),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn gemini_to_openai_flags_cached_content_and_thought_history() {
        let body = json!({
            "cachedContent": "cachedContents/abc",
            "contents": [
                {"role": "model", "parts": [{"text": "plan", "thought": true}, {"text": "ok"}]}
            ]
        });
        assert_eq!(
            dropped_fields(ApiFormat::Gemini, ApiFormat::ChatCompletions, &body),
            vec!["cached_content", "thinking_history"]
        );
    }
}
