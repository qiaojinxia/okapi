//! 切渠自动缓存压缩（Failover Cache Compaction L0 / L1）。
//!
//! 当会话发生跨渠道 / 跨 key 切换（Prompt Cache 必然冷启动）且输入超过阈值时，
//! 在不破坏语义与协议结构的前提下做确定性、零额外模型调用的上下文减负：
//! - **绝不触碰** `system` 消息与尾部最近 `keep_recent_messages` 条消息；
//! - **绝不删除**消息条或破坏 `tool_call_id` / `tool_use_id` / 角色交替不变量；
//! - **L0 清理**：移除旧消息上的冗余 `cache_control` 断点，避免在新渠道无谓支付高价中间缓存写；
//! - **L1 折叠**：将旧轮次中超过 `max_old_tool_chars` 的 `tool` / `tool_result` 输出（以及超长旧文本）
//!   按 UTF-8 字符安全边界折叠为「头部 2/3 + 省略标记 + 尾部 1/3」。

use okapi_domain::pool_policy::ResolvedCompactionPolicy;
use serde::Serialize;
use serde_json::Value;

/// 单次压缩统计报告（写入请求诊断 `cache_compaction` 字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CompactionReport {
    pub applied: bool,
    pub chars_saved: usize,
    pub est_tokens_saved: u32,
    pub folded_blocks: usize,
    pub stripped_breakpoints: usize,
}

/// UTF-8 安全字符串头尾折叠；若无需折叠返回 `None`。
fn fold_text(text: &str, max_chars: usize) -> Option<(String, usize)> {
    // 先做 L0 连续空行压缩
    let normalized = collapse_blank_lines(text);
    let total_chars = normalized.chars().count();
    let orig_chars = text.chars().count();
    if total_chars <= max_chars {
        let saved = orig_chars.saturating_sub(total_chars);
        return (saved >= 64).then_some((normalized, saved));
    }

    let head_len = (max_chars * 2) / 3;
    let tail_len = max_chars.saturating_sub(head_len);
    let omitted = total_chars.saturating_sub(head_len + tail_len);
    if omitted < 32 {
        return None;
    }

    let head: String = normalized.chars().take(head_len).collect();
    let tail: String = normalized
        .chars()
        .skip(total_chars.saturating_sub(tail_len))
        .collect();
    let marker = format!("\n...[compacted {omitted} chars on channel switch]...\n");
    let compacted = format!("{head}{marker}{tail}");
    let compacted_chars = compacted.chars().count();
    let saved = orig_chars.saturating_sub(compacted_chars);
    (saved > 0).then_some((compacted, saved))
}

fn collapse_blank_lines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut blank_run = 0u8;
    for line in input.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end.is_empty() {
            blank_run = blank_run.saturating_add(1);
            if blank_run <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(trimmed_end);
        }
    }
    out
}

/// 对 OpenAI Chat 或 Anthropic Messages 请求 JSON 做原地 L0/L1 压缩。
pub fn compact_request_value(
    body: &mut Value,
    policy: &ResolvedCompactionPolicy,
) -> CompactionReport {
    let mut report = CompactionReport::default();
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return report;
    };
    let keep = usize::from(policy.keep_recent_messages);
    if messages.len() <= keep {
        return report;
    }
    let cutoff = messages.len() - keep;
    let tool_limit = usize::try_from(policy.max_old_tool_chars).unwrap_or(600);
    // 普通旧文本消息放宽到工具阈值的 4 倍，只折叠极长的历史大段粘贴文本
    let text_limit = tool_limit.saturating_mul(4);

    for msg in &mut messages[..cutoff] {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        let role = obj.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "system" || role == "developer" {
            continue;
        }
        let is_tool_msg = role == "tool";
        if policy.strip_stale_cache_breakpoints && obj.remove("cache_control").is_some() {
            report.stripped_breakpoints += 1;
        }

        let Some(content) = obj.get_mut("content") else {
            continue;
        };
        match content {
            Value::String(text) => {
                let limit = if is_tool_msg { tool_limit } else { text_limit };
                if let Some((folded, saved)) = fold_text(text, limit) {
                    *text = folded;
                    report.chars_saved = report.chars_saved.saturating_add(saved);
                    report.folded_blocks += 1;
                }
            }
            Value::Array(parts) => {
                for part in parts {
                    compact_content_part(
                        part,
                        is_tool_msg,
                        tool_limit,
                        text_limit,
                        policy.strip_stale_cache_breakpoints,
                        &mut report,
                    );
                }
            }
            _ => {}
        }
    }

    report.applied = report.folded_blocks > 0 || report.stripped_breakpoints > 0;
    // 粗略按 4 字符 ≈ 1 token 估算节省量（仅用于诊断展示，不参与计费）
    report.est_tokens_saved = u32::try_from(report.chars_saved / 4).unwrap_or(u32::MAX);
    report
}

fn compact_content_part(
    part: &mut Value,
    parent_is_tool: bool,
    tool_limit: usize,
    text_limit: usize,
    strip_breakpoints: bool,
    report: &mut CompactionReport,
) {
    let Some(obj) = part.as_object_mut() else {
        return;
    };
    if strip_breakpoints && obj.remove("cache_control").is_some() {
        report.stripped_breakpoints += 1;
    }
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "tool_result" => {
            if let Some(inner) = obj.get_mut("content") {
                match inner {
                    Value::String(text) => {
                        if let Some((folded, saved)) = fold_text(text, tool_limit) {
                            *text = folded;
                            report.chars_saved = report.chars_saved.saturating_add(saved);
                            report.folded_blocks += 1;
                        }
                    }
                    Value::Array(subparts) => {
                        for sub in subparts {
                            compact_content_part(
                                sub,
                                true,
                                tool_limit,
                                text_limit,
                                strip_breakpoints,
                                report,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        "text" => {
            if let Some(Value::String(text)) = obj.get_mut("text") {
                let limit = if parent_is_tool {
                    tool_limit
                } else {
                    text_limit
                };
                if let Some((folded, saved)) = fold_text(text, limit) {
                    *text = folded;
                    report.chars_saved = report.chars_saved.saturating_add(saved);
                    report.folded_blocks += 1;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compacts_old_openai_tool_outputs_while_preserving_system_and_recent_turns() {
        let big_log = "INFO line of verbose build output 1234567890\n".repeat(120);
        let recent_log = "RECENT verbose tool output\n".repeat(120);
        let mut body = json!({
            "model": "gpt-5",
            "messages": [
                { "role": "system", "content": "x".repeat(5000) },
                { "role": "user", "content": "run tests" },
                { "role": "tool", "tool_call_id": "call_1", "content": big_log },
                { "role": "assistant", "content": "done" },
                { "role": "user", "content": "run again" },
                { "role": "tool", "tool_call_id": "call_2", "content": recent_log },
                { "role": "user", "content": "explain" }
            ]
        });
        let policy = ResolvedCompactionPolicy {
            mode: "on_channel_switch".into(),
            min_prompt_tokens: 1_000,
            keep_recent_messages: 3,
            max_old_tool_chars: 300,
            strip_stale_cache_breakpoints: true,
        };
        let report = compact_request_value(&mut body, &policy);
        assert!(report.applied);
        assert_eq!(report.folded_blocks, 1);
        assert!(report.chars_saved > 4000);
        // system 和最近 3 条（含 call_2）原样保留
        assert_eq!(body["messages"][0]["content"].as_str().unwrap().len(), 5000);
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
        assert!(
            body["messages"][2]["content"]
                .as_str()
                .unwrap()
                .contains("compacted")
        );
        assert_eq!(body["messages"][5]["content"].as_str().unwrap(), recent_log);
    }

    #[test]
    fn compacts_anthropic_tool_results_and_strips_stale_cache_breakpoints() {
        let huge = "中文日志输出内容测试，包含多字节字符！".repeat(200);
        let mut body = json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_old",
                            "content": huge,
                            "cache_control": { "type": "ephemeral" }
                        }
                    ]
                },
                { "role": "assistant", "content": [{"type": "text", "text": "ok"}] },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "text",
                            "text": "latest question",
                            "cache_control": { "type": "ephemeral" }
                        }
                    ]
                }
            ]
        });
        let policy = ResolvedCompactionPolicy {
            mode: "on_channel_switch".into(),
            min_prompt_tokens: 1_000,
            keep_recent_messages: 2,
            max_old_tool_chars: 200,
            strip_stale_cache_breakpoints: true,
        };
        let report = compact_request_value(&mut body, &policy);
        assert!(report.applied);
        assert_eq!(report.folded_blocks, 1);
        assert_eq!(report.stripped_breakpoints, 1);
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        // 最新轮次的 cache_control 必须保留
        assert!(
            body["messages"][2]["content"][0]
                .get("cache_control")
                .is_some()
        );
    }
}
