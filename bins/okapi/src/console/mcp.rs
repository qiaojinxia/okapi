//! 内置 MCP 服务（IMPLEMENTATION §7）：Streamable HTTP，POST JSON-RPC 单响应模式。
//! M3 只读工具面：用户级（balance/usage/keys/pricing/explain_bill）+
//! 管理查询（platform_kpi/channel_health/usage_stats/search_logs/
//! reconciliation_status/dlq_list，RBAC 权限点过滤）。
//! 写工具（M4）走三道闸：全局开关 + mcp.write + dry_run/confirm。

use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_store::auth::{AuthedKey, PermScope};
use serde_json::{Value, json};
use std::sync::Arc;

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
        description: "Search billing records by user_id/model/status, newest first.",
        schema: || {
            json!({"type": "object", "properties": {
                "user_id": {"type": "integer"},
                "model": {"type": "string"},
                "status": {"type": "integer"},
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
        description: "Probe channel reachability/latency (models endpoint per protocol).",
        schema: || {
            json!({"type": "object", "required": ["channel_id"], "properties": {
                "channel_id": {"type": "integer"}}})
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
        description: "Requeue DLQ entries back into billing_outbox. Two-phase: confirm=true required.",
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
        Some(perm) => !matches!(key.permission_scope(perm), PermScope::Denied),
    };
    if !resource_ok {
        return false;
    }
    if spec.write {
        // 第二道闸：mcp.write 权限点（第三道 = 资源权限，上面已验）
        return !matches!(
            key.permission_scope(permissions::MCP_WRITE),
            PermScope::Denied
        );
    }
    true
}

/// POST /mcp：JSON-RPC 2.0（initialize / tools/list / tools/call / ping）。
pub async fn endpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
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

// ---- 用户级工具 ----

async fn query_balance(state: &AppState, key: &AuthedKey) -> Result<Value, AppError> {
    let balance = state.ledger.balance(key.user_id).await?;
    Ok(json!({
        "user_id": key.user_id,
        "group": key.group_code,
        "balance_micro": balance.as_micros(),
    }))
}

async fn query_usage(state: &AppState, key: &AuthedKey, args: &Value) -> Result<Value, AppError> {
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_IMPLEMENTED, "stats_disabled"))?;
    let days = args
        .get("days")
        .and_then(Value::as_u64)
        .unwrap_or(7)
        .clamp(1, 90);
    let scope = args.get("scope").and_then(Value::as_str).unwrap_or("key");
    let sql = if scope == "user" {
        format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
             sumMerge(amount) AS amount_micro FROM mv_user_day \
             WHERE user_id = {} AND day >= today() - {days} GROUP BY day ORDER BY day",
            key.user_id
        )
    } else {
        format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
             sumMerge(amount) AS amount_micro FROM mv_apikey_day \
             WHERE api_key_id = {} AND day >= today() - {days} GROUP BY day ORDER BY day",
            key.key_id
        )
    };
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    Ok(json!({"scope": scope, "days": days, "data": rows}))
}

async fn list_my_keys(state: &AppState, key: &AuthedKey) -> Result<Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT id, name, key_prefix, status, used_micro FROM api_keys
           WHERE user_id = $1 AND deleted_at IS NULL ORDER BY id"#,
        key.user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({"id": r.id, "name": r.name, "key_prefix": r.key_prefix,
                   "status": r.status, "used_micro": r.used_micro})
        })
        .collect();
    Ok(json!({"data": data}))
}

async fn list_models_pricing(state: &AppState) -> Result<Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT m.model_name, p.pricing_mode,
                  p.model_ratio::text AS model_ratio,
                  p.completion_ratio::text AS completion_ratio,
                  p.cache_ratio::text AS cache_ratio,
                  p.per_call_price_micro
           FROM models m JOIN model_pricing p ON p.model_id = m.id
           WHERE m.status = 1 ORDER BY m.model_name"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({"model": r.model_name, "mode": r.pricing_mode,
                   "model_ratio": r.model_ratio, "completion_ratio": r.completion_ratio,
                   "cache_ratio": r.cache_ratio, "per_call_price_micro": r.per_call_price_micro})
        })
        .collect();
    Ok(json!({"data": data}))
}

/// 账单解释：非管理员只能看自己的记录（own 语义）。
async fn explain_bill(state: &AppState, key: &AuthedKey, args: &Value) -> Result<Value, AppError> {
    let request_id = args
        .get("request_id")
        .and_then(Value::as_str)
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::bad_request().with_param("request_id"))?;
    let row = sqlx::query!(
        r#"SELECT request_id, user_id, model_name, status, log_type,
                  prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
                  amount_micro, original_amount_micro, discount_micro,
                  pricing_epoch, pricing_snapshot, error_code, created_at
           FROM billing_records WHERE request_id = $1"#,
        request_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_FOUND, "record_not_found"))?;
    let is_admin = !matches!(
        key.permission_scope(permissions::BILLING_READ),
        PermScope::Denied
    );
    if row.user_id != key.user_id && !is_admin {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "permission_denied",
        ));
    }
    Ok(json!({
        "request_id": row.request_id,
        "model": row.model_name,
        "status": row.status,
        "log_type": row.log_type,
        "usage": {
            "prompt_tokens": row.prompt_tokens,
            "cached_tokens": row.cached_tokens,
            "completion_tokens": row.completion_tokens,
            "reasoning_tokens": row.reasoning_tokens,
        },
        "amount_micro": row.amount_micro,
        "original_amount_micro": row.original_amount_micro,
        "discount_micro": row.discount_micro,
        "pricing_epoch": row.pricing_epoch,
        // 计费唯一语义载体（DESIGN §3）：修饰器逐层展开由 AI 按此解释
        "pricing_snapshot": row.pricing_snapshot,
        "error_code": row.error_code,
        "created_at": row.created_at.to_rfc3339(),
    }))
}

// ---- 管理查询工具 ----

async fn platform_kpi(state: &AppState) -> Result<Value, AppError> {
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_IMPLEMENTED, "stats_disabled"))?;
    let rows = ch
        .query_json_each_row(
            "SELECT countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
             sumMerge(amount) AS amount_micro, uniqExact(user_id) AS active_users \
             FROM mv_user_day WHERE day = today()",
        )
        .await
        .map_err(AppError::from)?;
    Ok(json!({"today": rows.first().cloned().unwrap_or_else(|| json!({}))}))
}

async fn channel_health(state: &AppState) -> Result<Value, AppError> {
    let channels = okapi_store::admin::list_channels(&state.pg, None).await?;
    let keys = okapi_store::admin::list_channel_keys(&state.pg).await?;
    let data: Vec<Value> = channels
        .into_iter()
        .map(|c| {
            let keys: Vec<Value> = keys
                .iter()
                .filter(|k| k.channel_id == c.id)
                .map(
                    |k| json!({"id": k.id, "status": k.status, "cooldown_until": k.cooldown_until}),
                )
                .collect();
            json!({"id": c.id, "name": c.name, "provider": c.provider,
                   "status": c.status, "keys": keys})
        })
        .collect();
    Ok(json!({"data": data}))
}

/// 参数化维度模板（不暴露裸 SQL，继承 CH 查询护栏）。
async fn usage_stats(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_IMPLEMENTED, "stats_disabled"))?;
    let days = args
        .get("days")
        .and_then(Value::as_u64)
        .unwrap_or(7)
        .clamp(1, 90);
    let (mv, dim) = match args.get("dimension").and_then(Value::as_str) {
        Some("user") => ("mv_user_day", "user_id"),
        Some("model") => ("mv_user_model_day", "model"),
        Some("channel") => ("mv_channel_5min", "channel_id"),
        Some("group") => ("mv_group_day", "group"),
        _ => return Err(AppError::bad_request().with_param("dimension")),
    };
    let time_col = if mv == "mv_channel_5min" {
        format!("ts5 >= now() - INTERVAL {days} DAY")
    } else {
        format!("day >= today() - {days}")
    };
    let sql = format!(
        "SELECT {dim} AS dim, countMerge(requests) AS requests, \
         sumMerge(amount) AS amount_micro FROM {mv} WHERE {time_col} \
         GROUP BY dim ORDER BY amount_micro DESC LIMIT 20"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    Ok(json!({"dimension": dim, "days": days, "data": rows}))
}

async fn search_logs(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 100);
    let user_id = args.get("user_id").and_then(Value::as_i64);
    let model = args.get("model").and_then(Value::as_str);
    let status = args
        .get("status")
        .and_then(Value::as_i64)
        .and_then(|v| i16::try_from(v).ok());
    let rows = sqlx::query!(
        r#"SELECT request_id, user_id, model_name, status, amount_micro, error_code, created_at
           FROM billing_records
           WHERE ($1::bigint IS NULL OR user_id = $1)
             AND ($2::text IS NULL OR model_name = $2)
             AND ($3::smallint IS NULL OR status = $3)
           ORDER BY created_at DESC LIMIT $4"#,
        user_id,
        model,
        status,
        limit
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({"request_id": r.request_id, "user_id": r.user_id, "model": r.model_name,
                   "status": r.status, "amount_micro": r.amount_micro,
                   "error_code": r.error_code, "created_at": r.created_at.to_rfc3339()})
        })
        .collect();
    Ok(json!({"data": data}))
}

async fn reconciliation_status(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(1000)
        .clamp(1, 5000);
    let drifts = crate::worker::reconcile_balances(&state.pg, &state.ledger, limit)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "MCP 对账查询失败");
            AppError::internal()
        })?;
    let data: Vec<Value> = drifts
        .iter()
        .map(|d| {
            json!({"user_id": d.user_id, "events_sum_micro": d.events_sum_micro,
                   "redis_effective_micro": d.redis_effective_micro,
                   "pg_snapshot_micro": d.pg_snapshot_micro})
        })
        .collect();
    Ok(json!({"drift_count": data.len(), "drifts": data}))
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
        "dlq_depth": dlq_depth,
        "cooling_keys": cooling_keys,
        "pricebook_epoch": state.pricebook.epoch(),
    }))
}

async fn channel_create(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request().with_param("name"))?;
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    let api_base = args
        .get("api_base")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request().with_param("api_base"))?;
    super::ssrf::validate_api_base(state, api_base).await?;
    let credential = args
        .get("credential")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request().with_param("credential"))?;
    let models: Vec<&str> = args
        .get("models")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if models.is_empty() {
        return Err(AppError::bad_request().with_param("models"));
    }
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel(
        &state.pg,
        name,
        provider,
        api_base,
        credential,
        &models,
        false,
        state.master_key.as_deref(),
    )
    .await?;
    okapi_store::admin::set_channel_owner(&state.pg, channel_id, key.user_id).await?;
    state.invalidate_routing_caches();
    mcp_audit(
        state,
        key,
        "channel.create",
        &channel_id.to_string(),
        json!({"name": name, "provider": provider}),
    )
    .await;
    Ok(json!({"channel_id": channel_id, "channel_key_id": channel_key_id}))
}

async fn channel_toggle(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let channel_id = args
        .get("channel_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request().with_param("channel_id"))?;
    let enable = args.get("enable").and_then(Value::as_bool).unwrap_or(true);
    let status: i16 = if enable { 1 } else { 2 };
    okapi_store::admin::set_channel_status(&state.pg, channel_id, status).await?;
    state.invalidate_routing_caches();
    mcp_audit(
        state,
        key,
        "channel.status",
        &channel_id.to_string(),
        json!({"status": status}),
    )
    .await;
    Ok(json!({"ok": true, "status": status}))
}

async fn mcp_channel_test(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let channel_id = args
        .get("channel_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request().with_param("channel_id"))?;
    let result = super::admin::probe_channel(state, channel_id).await?;
    mcp_audit(
        state,
        key,
        "channel.test",
        &channel_id.to_string(),
        result.clone(),
    )
    .await;
    Ok(result)
}

async fn user_adjust_balance(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let user_id = args
        .get("user_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request().with_param("user_id"))?;
    let amount_micro = args
        .get("amount_micro")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .ok_or_else(|| AppError::bad_request().with_param("amount_micro"))?;
    let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
    let current = state.ledger.balance(user_id).await?;
    // 两段式：无 confirm 只出预览
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({
            "dry_run": true,
            "user_id": user_id,
            "balance_micro": current.as_micros(),
            "balance_after_micro": current.as_micros().saturating_add(amount_micro),
        }));
    }
    let amount = okapi_domain::Money::from_micros(amount_micro);
    let balance_after = state.ledger.credit(user_id, amount).await?;
    okapi_ledger::pg::record_credit(
        &state.pg,
        user_id,
        amount,
        "adjust",
        &format!("mcp:{}", key.key_id),
        json!({"tags": ["mcp_credit"], "reason": reason}),
    )
    .await?;
    mcp_audit(
        state,
        key,
        "user.credit",
        &user_id.to_string(),
        json!({"amount_micro": amount_micro, "reason": reason}),
    )
    .await;
    Ok(json!({"dry_run": false, "balance_after_micro": balance_after.as_micros()}))
}

async fn user_ban(state: &AppState, key: &AuthedKey, args: &Value) -> Result<Value, AppError> {
    let user_id = args
        .get("user_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request().with_param("user_id"))?;
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({"dry_run": true, "user_id": user_id, "action": "ban"}));
    }
    sqlx::query!(
        r#"UPDATE users SET status = 2, updated_at = now() WHERE id = $1"#,
        user_id
    )
    .execute(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    state.sched.auth_flush().await;
    mcp_audit(state, key, "user.ban", &user_id.to_string(), json!({})).await;
    Ok(json!({"dry_run": false, "banned": true}))
}

async fn simulate_pricing(state: &AppState) -> Result<Value, AppError> {
    let rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let source = crate::gateway::pricing_loader::build_source(&rows);
    let models = source.models.len();
    let groups = source.groups.len();
    match okapi_pricing::book::compile(source) {
        Ok(_) => Ok(json!({"ok": true, "models": models, "groups": groups})),
        Err(err) => Ok(json!({"ok": false, "compile_error": err.to_string()})),
    }
}

async fn apply_pricing(state: &AppState, key: &AuthedKey, args: &Value) -> Result<Value, AppError> {
    let sim = simulate_pricing(state).await?;
    if sim.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(sim);
    }
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({"dry_run": true, "validation": sim}));
    }
    let rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let snapshot = serde_json::to_value(&rows).map_err(|_| AppError::internal())?;
    let epoch = okapi_store::admin::publish_epoch(&state.pg, key.user_id, &snapshot).await?;
    if let Some(nats) = &state.nats {
        let _ = nats
            .publish("pricing.epoch", epoch.to_string().into())
            .await;
    }
    mcp_audit(state, key, "pricing.publish", &epoch.to_string(), json!({})).await;
    Ok(json!({"dry_run": false, "epoch": epoch}))
}

async fn dlq_requeue(state: &AppState, key: &AuthedKey, args: &Value) -> Result<Value, AppError> {
    let ids: Vec<i64> = args
        .get("ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(AppError::bad_request().with_param("ids"));
    }
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({"dry_run": true, "ids": ids}));
    }
    // 与 HTTP /admin/dlq/requeue 同一函数：AI 与人执行的必须是同一个动作
    let requeued = super::dlq::requeue(&state.pg, &ids).await?;
    mcp_audit(
        state,
        key,
        "billing.dlq_requeue",
        "batch",
        json!({"ids": ids, "requeued": requeued}),
    )
    .await;
    Ok(json!({"dry_run": false, "requeued": requeued}))
}

async fn redemption_create(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let count = args
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| (1..=1000).contains(v))
        .ok_or_else(|| AppError::bad_request().with_param("count"))?;
    let amount_micro = args
        .get("amount_micro")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .ok_or_else(|| AppError::bad_request().with_param("amount_micro"))?;
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({"dry_run": true, "count": count, "amount_micro": amount_micro}));
    }
    let codes: Vec<String> = (0..count)
        .map(|_| {
            use rand::RngExt;
            use rand::distr::Alphanumeric;
            let body: String = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(24)
                .map(char::from)
                .collect();
            format!("okapi-{body}")
        })
        .collect();
    let batch_id = okapi_store::admin::create_redemption_codes(
        &state.pg,
        key.user_id,
        amount_micro,
        &codes,
        None,
        okapi_store::admin::RedemptionOptions::default(),
    )
    .await?
    .unwrap_or_default();
    mcp_audit(
        state,
        key,
        "redemption.create",
        &batch_id.to_string(),
        json!({"count": count, "amount_micro": amount_micro}),
    )
    .await;
    Ok(json!({"dry_run": false, "batch_id": batch_id, "codes": codes}))
}

async fn mcp_cache_flush(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request().with_param("scope"))?;
    match scope {
        "auth" => state.sched.auth_flush().await,
        "routing" => state.invalidate_routing_caches(),
        "pricebook" => {
            let book = crate::gateway::pricing_loader::load_pricebook(&state.pg)
                .await
                .map_err(|_| AppError::internal())?;
            state.pricebook.replace(book);
        }
        _ => return Err(AppError::bad_request().with_param("scope")),
    }
    mcp_audit(state, key, "cache.flush", scope, json!({})).await;
    Ok(json!({"ok": true}))
}

async fn dlq_list(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 200);
    let rows = sqlx::query!(
        r#"SELECT id, source, error, retry_count, created_at FROM billing_dlq
           WHERE status = 0 ORDER BY id DESC LIMIT $1"#,
        limit
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({"id": r.id, "source": r.source, "error": r.error,
                   "retry_count": r.retry_count, "created_at": r.created_at.to_rfc3339()})
        })
        .collect();
    Ok(json!({"data": data}))
}
