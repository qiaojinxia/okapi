//! MCP 的适配规则工具（设计见 docs/compat-rules.md §17.7）。
//!
//! 五个只读工具让 AI 看清「引擎能做什么、这个渠道现在套着什么、某条请求为什么被改成这样、
//! 一份草稿在样本上会怎样」；一个写工具 `compat_propose` **只落草稿**，由人在控制台批准发布
//! （批准需要 `compat.publish`，草稿里新增的敏感写入还要批准的人有 `compat.sensitive`）。
//! 核心闭环：上游 400 → `compat_explain` 读诊断 → 起草规则 → `compat_dry_run` 在样本上回放 →
//! `compat_propose` 提交草稿 → 人批准。
//!
//! 实现全部复用 REST 的内核（`compat::channel_view` / `run_dry` / `compat_profiles::explain_request` /
//! `compat_sets::store_draft`），所以两条通道看到的、能做的事情一致。

use super::compat::{DryRunReq, channel_view, run_dry};
use super::compat_profiles::explain_request;
use super::compat_sets::{Access, DraftSpec, Target, check, store_draft};
use super::mcp::mcp_audit;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use okapi_api::permissions;
use okapi_store::AuthedKey;
use serde_json::{Value, json};
use uuid::Uuid;

fn bad(param: &'static str) -> AppError {
    AppError::bad_request().with_param(param)
}

pub(super) fn catalog_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

pub(super) fn effective_schema() -> Value {
    json!({"type": "object", "required": ["channel_id"], "properties": {
        "channel_id": {"type": "integer"}}})
}

pub(super) fn validate_schema() -> Value {
    json!({"type": "object", "required": ["rules"], "properties": {
        "rules": {"type": "array", "description": "Rule set: array of rule objects (see compat_catalog)",
                  "items": {"type": "object"}}}})
}

pub(super) fn dry_run_schema() -> Value {
    json!({"type": "object", "required": ["context"], "properties": {
        "channel_id": {"type": "integer",
            "description": "Use this channel's published vendor / channel layer / guard"},
        "vendor": {"type": "string", "description": "builtin id (deepseek) or custom:<slug>; overrides the channel's"},
        "rules": {"type": "array", "items": {"type": "object"},
            "description": "Channel-layer draft to try instead of the channel's published one"},
        "phase": {"type": "string", "enum": ["request", "response", "error", "capability"], "default": "request"},
        "context": {"type": "object", "properties": {
            "model": {"type": "string"}, "client_model": {"type": "string"},
            "dialect": {"type": "string", "default": "openai.chat"},
            "operation": {"type": "string", "default": "chat"},
            "vendor": {"type": "string"}, "host": {"type": "string"},
            "stream": {"type": "boolean"}, "client": {"type": "string"}}},
        "request": {"type": "object", "description": "request phase: the JSON body sent upstream; error phase: optional, shows the retry_same result"},
        "response": {"type": "object", "description": "response phase: an OpenAI chat response or one stream frame"},
        "error": {"type": "object", "required": ["status"], "properties": {
            "status": {"type": "integer"}, "body": {"description": "JSON or raw text of the upstream error body"}}}}})
}

pub(super) fn explain_schema() -> Value {
    json!({"type": "object", "required": ["request_id"], "properties": {
        "request_id": {"type": "string", "format": "uuid"}}})
}

pub(super) fn propose_schema() -> Value {
    json!({"type": "object", "required": ["scope", "rules", "rationale"], "properties": {
        "scope": {"type": "string", "enum": ["channel", "profile", "guard"]},
        "id": {"type": "integer", "description": "channel id or profile id; omit for guard"},
        "rules": {"type": "array", "items": {"type": "object"},
            "description": "The COMPLETE rule set for that scope (it replaces the current draft and, once approved, the published set)"},
        "rationale": {"type": "string", "maxLength": 1000,
            "description": "What failed, which request / error showed it, and why these rules fix it. Shown to the approver."},
        "note": {"type": "string", "maxLength": 255},
        "base_version": {"type": "integer",
            "description": "Published version the draft was written against (compat_effective shows it); defaults to the current one"}}})
}

pub(super) fn catalog() -> Value {
    okapi_compat::catalog::catalog()
}

pub(super) async fn effective(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let id = args
        .get("channel_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("channel_id"))?;
    channel_view(
        state,
        key,
        key.permission_scope(permissions::CHANNEL_READ),
        id,
    )
    .await
}

pub(super) fn validate(args: &Value) -> Result<Value, AppError> {
    let rules = args.get("rules").ok_or_else(|| bad("rules"))?;
    Ok(match okapi_compat::validate(rules) {
        Ok(valid) => json!({
            "valid": true, "count": valid.len(), "rules": valid.to_value(),
            "sensitive": valid.sensitive(),
        }),
        Err(issues) => json!({"valid": false, "issues": issues}),
    })
}

pub(super) async fn dry_run(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let req: DryRunReq = serde_json::from_value(args.clone()).map_err(|_| bad("arguments"))?;
    run_dry(
        state,
        key,
        key.permission_scope(permissions::CHANNEL_READ),
        req,
    )
    .await
}

pub(super) async fn explain(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let request_id = args
        .get("request_id")
        .and_then(Value::as_str)
        .and_then(|text| Uuid::parse_str(text).ok())
        .ok_or_else(|| bad("request_id"))?;
    explain_request(state, request_id).await
}

/// 只落草稿，从不发布。三道闸：全局开关与 `mcp.write` 在 `tools_call` 里已验，这里是第三道——
/// 与 REST 存草稿同一处的行级检查（渠道要属主 / 全量范围，档案与护栏要各自的写权限点）。
pub(super) async fn propose(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("scope"))?;
    let id = args.get("id").and_then(Value::as_i64).unwrap_or(0);
    let target = Target::parse(scope, id)?;
    check(state, key, target, Access::Draft).await?;
    let rules = args
        .get("rules")
        .filter(|rules| rules.is_array())
        .ok_or_else(|| bad("rules"))?;
    // 依据必填：批准的人要靠它判断，没有依据的提议没法审
    let rationale = args
        .get("rationale")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad("rationale"))?;
    let base_version = match args.get("base_version") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(|| bad("base_version"))?,
        ),
    };
    let (version, base) = store_draft(
        state,
        key,
        target,
        DraftSpec {
            rules,
            base_version,
            note: args.get("note").and_then(Value::as_str),
            rationale: Some(rationale),
            source: "mcp",
        },
    )
    .await?;
    mcp_audit(
        state,
        key,
        "compat.draft.save",
        &target.label(),
        json!({"version": version, "source": "mcp", "base_version": base}),
    )
    .await;
    Ok(json!({"ok": true, "status": "draft", "version": version, "needs_approval": true}))
}
