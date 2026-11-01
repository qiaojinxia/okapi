//! 内置 MCP 服务（IMPLEMENTATION §7）：Streamable HTTP，POST JSON-RPC 单响应模式。
//! M3 只读工具面：用户级（balance/usage/keys/pricing/explain_bill）+
//! 管理查询（platform_kpi/channel_health/usage_stats/search_logs/
//! reconciliation_status/dlq_list，RBAC 权限点过滤）。
//! 写工具（M4）走三道闸：全局开关 + mcp.write + dry_run/confirm。
//!
//! 本文件放协议、工具清单、权限过滤与分派；工具实现在 read_tools / write_tools。

use crate::gateway::state::AppState;
use crate::kernel::auth::authenticate;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_store::auth::{AuthedKey, PermScope};
use serde_json::{Value, json};
use std::sync::Arc;

mod read_tools;
mod write_tools;

use self::read_tools::{
    channel_health, dlq_list, explain_bill, list_models_pricing, list_my_keys, platform_kpi,
    query_balance, query_usage, reconciliation_status, search_logs, usage_stats,
};
use self::write_tools::{
    apply_pricing, channel_create, channel_toggle, dlq_requeue, mcp_cache_flush, mcp_channel_test,
    redemption_create, simulate_pricing, user_adjust_balance, user_ban,
};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// 工具描述（名称、说明、入参 schema、所需权限点；None = 任何合法 key 可用）。
/// `write = true` 的工具受三道闸（§7.3）：settings.mcp_write_enabled +
/// `mcp.write` 权限点 + 行级资源权限；危险操作再叠 confirm 两段式。
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    permission: Option<&'static str>,
    write: bool,
}

const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "query_balance",
        description: "Query current balance (micro-USD) and billing group of the calling key's owner.",
        schema: || json!({"type": "object", "properties": {}}),
        permission: None,
        write: false,
    },
    ToolSpec {
        name: "query_usage",
        description: "Daily usage (requests/tokens/amount) for the calling key or whole account.",
        schema: || {
            json!({"type": "object", "properties": {
                "days": {"type": "integer", "minimum": 1, "maximum": 90, "default": 7},
                "scope": {"type": "string", "enum": ["key", "user"], "default": "key"}
            }})
        },
        permission: None,
        write: false,
    },
    ToolSpec {
        name: "list_my_keys",
        description: "List all API keys of the calling user with cumulative spend.",
        schema: || json!({"type": "object", "properties": {}}),
        permission: None,
        write: false,
    },
    ToolSpec {
        name: "list_models_pricing",
        description: "List models with ratio pricing (model/completion/cache ratios as decimal strings).",
        schema: || json!({"type": "object", "properties": {}}),
        permission: None,
        write: false,
    },
    ToolSpec {
        name: "explain_bill",
        description: "Explain one billing record by request_id using its pricing_snapshot (per-modifier breakdown).",
        schema: || {
            json!({"type": "object", "required": ["request_id"], "properties": {
                "request_id": {"type": "string", "format": "uuid"}
            }})
        },
        permission: None,
        write: false,
    },
    ToolSpec {
        name: "platform_kpi",
        description: "Platform KPI for today: requests, tokens, spend (micro-USD), active users.",
        schema: || json!({"type": "object", "properties": {}}),
        permission: Some(permissions::BILLING_READ),
        write: false,
    },
    ToolSpec {
        name: "channel_health",
        description: "Channels with key states (available/cooling/disabled) for routing health.",
        schema: || json!({"type": "object", "properties": {}}),
        permission: Some(permissions::CHANNEL_READ),
        write: false,
    },
    ToolSpec {
        name: "usage_stats",
        description: "Aggregated usage by dimension (user|model|channel|group) over N days, top 20.",
        schema: || {
            json!({"type": "object", "required": ["dimension"], "properties": {
                "dimension": {"type": "string", "enum": ["user", "model", "channel", "group"]},
                "days": {"type": "integer", "minimum": 1, "maximum": 90, "default": 7}
            }})
        },
        permission: Some(permissions::BILLING_READ),
        write: false,
    },
    ToolSpec {
        name: "search_logs",
        description: "Search billing records by user_id/model/status within the last N days, newest first.",
        schema: || {
            json!({"type": "object", "properties": {
                "user_id": {"type": "integer"},
                "model": {"type": "string"},
                "status": {"type": "integer"},
                "days": {"type": "integer", "minimum": 1, "maximum": 90, "default": 7},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20}
            }})
        },
        permission: Some(permissions::BILLING_READ),
        write: false,
    },
    ToolSpec {
        name: "reconciliation_status",
        description: "Three-way reconciliation drifts (Redis vs event-replay vs PG snapshot).",
        schema: || {
            json!({"type": "object", "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 5000, "default": 1000}
            }})
        },
        permission: Some(permissions::BILLING_READ),
        write: false,
    },
    ToolSpec {
        name: "dlq_list",
        description: "List dead-letter queue entries (chsink/outbox terminal failures).",
        schema: || {
            json!({"type": "object", "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}
            }})
        },
        permission: Some(permissions::BILLING_READ),
        write: false,
    },
    ToolSpec {
        name: "diagnose",
        description: "Full-chain health: PG/Redis/CH/NATS reachability, outbox backlog, DLQ depth, cooling keys, in-flight reservations.",
        schema: || json!({"type": "object", "properties": {}}),
        permission: Some(permissions::BILLING_READ),
        write: false,
    },
    ToolSpec {
        name: "channel_create",
        description: "Create a channel with one key (writes require mcp_write_enabled).",
        schema: || {
            json!({"type": "object", "required": ["name", "api_base", "credential", "models"],
            "properties": {
                "name": {"type": "string"},
                "provider": {"type": "string", "default": "openai"},
                "api_base": {"type": "string"},
                "credential": {"type": "string"},
                "models": {"type": "array", "items": {"type": "string"}},
                "priority": {"type": "integer", "default": 0}
            }})
        },
        permission: Some(permissions::CHANNEL_WRITE),
        write: true,
    },
    ToolSpec {
        name: "channel_toggle",
        description: "Enable/disable a channel (reversible; no confirm needed).",
        schema: || {
            json!({"type": "object", "required": ["channel_id", "enable"], "properties": {
                "channel_id": {"type": "integer"}, "enable": {"type": "boolean"}}})
        },
        permission: Some(permissions::CHANNEL_WRITE),
        write: true,
    },
    ToolSpec {
        name: "channel_test",
        description: "Probe a channel. Without `model`: credential + reachability only. With `model`: sends a real 1-token completion to verify that specific model is callable (aggregator upstreams authorize per model, so a credential probe alone can report ok for a model that 403s).",
        schema: || {
            json!({"type": "object", "required": ["channel_id"], "properties": {
                "channel_id": {"type": "integer"},
                "model": {"type": "string"}}})
        },
        permission: Some(permissions::CHANNEL_WRITE),
        write: true,
    },
    ToolSpec {
        name: "user_adjust_balance",
        description: "Credit a user's balance (micro-USD, positive only). Two-phase: dry-run preview unless confirm=true.",
        schema: || {
            json!({"type": "object", "required": ["user_id", "amount_micro"], "properties": {
                "user_id": {"type": "integer"},
                "amount_micro": {"type": "integer", "minimum": 1},
                "reason": {"type": "string"},
                "confirm": {"type": "boolean", "default": false}}})
        },
        permission: Some(permissions::USER_BALANCE_ADJUST),
        write: true,
    },
    ToolSpec {
        name: "user_ban",
        description: "Disable a user (status=2) and flush auth cache. Two-phase: confirm=true required.",
        schema: || {
            json!({"type": "object", "required": ["user_id"], "properties": {
                "user_id": {"type": "integer"},
                "confirm": {"type": "boolean", "default": false}}})
        },
        permission: Some(permissions::USER_MANAGE),
        write: true,
    },
    ToolSpec {
        name: "simulate_pricing",
        description: "Compile current pricing source without publishing (validation + summary).",
        schema: || json!({"type": "object", "properties": {}}),
        permission: Some(permissions::PRICING_PUBLISH),
        write: true,
    },
    ToolSpec {
        name: "apply_pricing",
        description: "Publish pricing epoch after validation. Two-phase: confirm=true required.",
        schema: || {
            json!({"type": "object", "properties": {
                "confirm": {"type": "boolean", "default": false}}})
        },
        permission: Some(permissions::PRICING_PUBLISH),
        write: true,
    },
    ToolSpec {
        name: "dlq_requeue",
        description: "Requeue DLQ entries. Selecting a persisted CH batch member restores its whole original batch; legacy entries return to outbox. Returns actual member count. Two-phase: confirm=true required.",
        schema: || {
            json!({"type": "object", "required": ["ids"], "properties": {
                "ids": {"type": "array", "items": {"type": "integer"}},
                "confirm": {"type": "boolean", "default": false}}})
        },
        permission: Some(permissions::BILLING_REFUND),
        write: true,
    },
    ToolSpec {
        name: "redemption_create",
        description: "Generate redemption codes (plaintext returned once). Two-phase: confirm=true required.",
        schema: || {
            json!({"type": "object", "required": ["count", "amount_micro"], "properties": {
                "count": {"type": "integer", "minimum": 1, "maximum": 1000},
                "amount_micro": {"type": "integer", "minimum": 1},
                "confirm": {"type": "boolean", "default": false}}})
        },
        permission: Some(permissions::USER_BALANCE_ADJUST),
        write: true,
    },
    ToolSpec {
        name: "cache_flush",
        description: "Flush caches (scope: auth | routing | pricebook).",
        schema: || {
            json!({"type": "object", "required": ["scope"], "properties": {
                "scope": {"type": "string", "enum": ["auth", "routing", "pricebook"]}}})
        },
        permission: Some(permissions::CACHE_FLUSH),
        write: true,
    },
];

/// 写工具全局开关（三道闸第一道）。
async fn mcp_write_enabled(state: &AppState) -> bool {
    sqlx::query_scalar!(
        r#"SELECT COALESCE((value #>> '{}')::boolean, false) AS "v!"
           FROM settings WHERE key = 'mcp_write_enabled'"#
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

fn allowed(key: &AuthedKey, spec: &ToolSpec) -> bool {
    let resource_ok = match spec.permission {
        None => true,
        Some(perm)
            if matches!(
                spec.name,
                "channel_create" | "channel_toggle" | "channel_test" | "explain_bill"
            ) =>
        {
            key.permission_scope(perm) != PermScope::Denied
        }
        Some(perm) => key.has_permission(perm),
    };
    if !resource_ok {
        return false;
    }
    if spec.write {
        // 第二道闸：mcp.write 权限点（第三道 = 资源权限，上面已验）
        return key.has_permission(permissions::MCP_WRITE);
    }
    true
}

/// POST /mcp：JSON-RPC 2.0（initialize / tools/list / tools/call / ping）。
pub async fn endpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<Value>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    // 通知（无 id）：按 spec 收下即可
    if req.get("id").is_none() {
        return Ok(Json(json!({})));
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "okapi-mcp", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list(&key, mcp_write_enabled(&state).await)),
        "tools/call" => tools_call(&state, &key, &params).await,
        _ => Err((-32601, "method_not_found".to_owned())),
    };

    let body = match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    };
    Ok(Json(body))
}

fn tools_list(key: &AuthedKey, write_enabled: bool) -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .filter(|spec| (!spec.write || write_enabled) && allowed(key, spec))
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "inputSchema": (spec.schema)(),
            })
        })
        .collect();
    json!({"tools": tools})
}

type RpcResult = Result<Value, (i64, String)>;

async fn tools_call(state: &AppState, key: &Arc<AuthedKey>, params: &Value) -> RpcResult {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let Some(spec) = TOOLS.iter().find(|s| s.name == name) else {
        return Err((-32602, "unknown_tool".to_owned()));
    };
    if !allowed(key, spec) {
        return Err((-32603, "permission_denied".to_owned()));
    }
    // 第一道闸：写工具全局开关（默认 OFF）
    if spec.write && !mcp_write_enabled(state).await {
        return Err((-32603, "mcp_write_disabled".to_owned()));
    }

    let out = match name {
        "query_balance" => query_balance(state, key).await,
        "query_usage" => query_usage(state, key, &args).await,
        "list_my_keys" => list_my_keys(state, key).await,
        "list_models_pricing" => list_models_pricing(state).await,
        "explain_bill" => explain_bill(state, key, &args).await,
        "platform_kpi" => platform_kpi(state).await,
        "channel_health" => channel_health(state).await,
        "usage_stats" => usage_stats(state, &args).await,
        "search_logs" => search_logs(state, &args).await,
        "reconciliation_status" => reconciliation_status(state, &args).await,
        "dlq_list" => dlq_list(state, &args).await,
        "diagnose" => diagnose(state).await,
        "channel_create" => channel_create(state, key, &args).await,
        "channel_toggle" => channel_toggle(state, key, &args).await,
        "channel_test" => mcp_channel_test(state, key, &args).await,
        "user_adjust_balance" => user_adjust_balance(state, key, &args).await,
        "user_ban" => user_ban(state, key, &args).await,
        "simulate_pricing" => simulate_pricing(state).await,
        "apply_pricing" => apply_pricing(state, key, &args).await,
        "dlq_requeue" => dlq_requeue(state, key, &args).await,
        "redemption_create" => redemption_create(state, key, &args).await,
        "cache_flush" => mcp_cache_flush(state, key, &args).await,
        _ => return Err((-32602, "unknown_tool".to_owned())),
    };
    match out {
        Ok(value) => Ok(json!({
            "content": [{"type": "text", "text": value.to_string()}],
            "structuredContent": value,
            "isError": false,
        })),
        Err(err) => Ok(json!({
            "content": [{"type": "text", "text": err.code}],
            "isError": true,
        })),
    }
}

// ---- 写工具（§7.3 三道闸之上按需 confirm 两段式）与 diagnose ----

/// MCP 审计（actor = `mcp:{key_id}`，§7.1）。
async fn mcp_audit(state: &AppState, key: &AuthedKey, action: &str, target: &str, detail: Value) {
    if let Err(err) = okapi_store::admin::record_audit(
        &state.pg,
        &format!("mcp:{}", key.key_id),
        action,
        target,
        detail,
    )
    .await
    {
        tracing::error!(error = %err, action, "MCP 审计写入失败");
    }
}

/// 全链路健康检查（对标老仓库 make diagnose）。
/// 全链路健康快照。MCP `diagnose` 工具与 HTTP `GET /admin/diagnose` 共用——
/// AI 远程巡检与站长后台看到的必须是同一份事实。
pub(super) async fn diagnose(state: &AppState) -> Result<Value, AppError> {
    let pg_ok = sqlx::query_scalar!(r#"SELECT 1 AS "one!""#)
        .fetch_one(&state.pg)
        .await
        .is_ok();
    // Redis 探活：随手读一个余额（错误 = 不可达）
    let redis_ok = state.ledger.balance(0).await.is_ok();
    let ch_ok = match &state.ch {
        Some(ch) => Some(ch.ping().await),
        None => None,
    };
    let outbox_pending = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_outbox WHERE status = 0"#
    )
    .fetch_one(&state.pg)
    .await
    .unwrap_or(-1);
    // 只数未处理的：已丢弃的毒消息不该让健康面板永远红
    let dlq_depth = super::dlq::pending_depth(&state.pg).await.unwrap_or(-1);
    let delivery = okapi_store::delivery::backlog(&state.pg)
        .await
        .ok()
        .map(|b| {
            json!({
                "pending_events":b.pending_events,"failed_events":b.failed_events,
                "ch_pending_events":b.ch_pending_events,"oldest_pending_at":b.oldest_pending_at
            })
        });
    let cooling_keys = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM channel_keys
           WHERE status <> 1 AND cooldown_until > now()"#
    )
    .fetch_one(&state.pg)
    .await
    .unwrap_or(-1);
    Ok(json!({
        "postgres": pg_ok,
        "redis": redis_ok,
        "clickhouse": ch_ok,
        "nats_connected": state.nats.is_some(),
        "outbox_pending": outbox_pending,
        "delivery": delivery,
        "dlq_depth": dlq_depth,
        "cooling_keys": cooling_keys,
        "pricebook_epoch": state.pricebook.epoch(),
    }))
}
