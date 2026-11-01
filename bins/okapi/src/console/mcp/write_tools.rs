//! 写工具（需开启 mcp_write）：渠道创建 / 启停 / 测活、用户调余额 / 封禁、定价试算与发布、死信重投、兑换码、缓存清理。
use super::mcp_audit;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use okapi_api::permissions;
use okapi_store::auth::{AuthedKey, PermScope};
use serde_json::{Value, json};

pub(super) async fn channel_create(
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
    // 与 REST 的 create_channel 同一套入参校验与归一化：两条通道建同一种东西，不能一宽一窄
    // （此前这里认任意 provider、不规整名称与地址、OAuth 凭证原样入库，建出来的渠道是坏的）
    if okapi_providers::registry::lookup(provider).is_none() {
        return Err(AppError::bad_request().with_param("provider"));
    }
    let api_base = super::super::channel_creation::endpoint(
        state,
        provider,
        args.get("api_base").and_then(Value::as_str),
    )
    .await?;
    super::super::admin::ensure_azure_api_base(provider, Some(api_base))?;
    let credential = args
        .get("credential")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request().with_param("credential"))?;
    crate::gateway::ssrf::validate_credential(&state.pg, credential).await?;
    let credential = super::super::channel_credentials::normalize(provider, credential)?;
    let name = super::super::identifiers::ensure_display_name("name", name, 128)?;
    let requested: Vec<String> = args
        .get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    // 与 REST 同一套规整：去空白、去重、空项拒绝
    let models = super::super::admin::normalize_models(&requested)?;
    if models.is_empty() {
        return Err(AppError::bad_request().with_param("models"));
    }
    let models: Vec<&str> = models.iter().map(String::as_str).collect();
    // own 范围建的渠道不入池（admin::ensure_pool_scope）：同一事务里建成孤儿，不留"先进 default 再摘"的窗口
    let orphan: &[okapi_store::admin::PoolMember] = &[];
    let pools =
        (key.permission_scope(permissions::CHANNEL_WRITE) != PermScope::All).then_some(orphan);
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel_configured(
        &state.pg,
        okapi_store::provision::ChannelCreate {
            name,
            provider,
            api_base,
            credential: &credential,
            models: &models,
            trust_upstream_usage: false,
            owner_id: Some(key.user_id),
            settings: None,
            priority: 0,
            max_concurrency: None,
            cost_milli: None,
            pools,
            egress: None,
            egress_preassigned: None,
        },
        state.master_key.as_deref(),
    )
    .await?;
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

pub(super) async fn channel_toggle(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let channel_id = args
        .get("channel_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request().with_param("channel_id"))?;
    let enable = args.get("enable").and_then(Value::as_bool).unwrap_or(true);
    super::super::admin::ensure_channel_owner(
        state,
        channel_id,
        key,
        key.permission_scope(permissions::CHANNEL_WRITE),
    )
    .await?;
    let status: i16 = if enable { 1 } else { 2 };
    // all 范围不查属主，不存在的渠道在这里才露馅：与 REST 一样回 404，别报成功还写一条审计
    if !okapi_store::admin::set_channel_status(&state.pg, channel_id, status).await? {
        return Err(AppError::new(
            axum::http::StatusCode::NOT_FOUND,
            okapi_api::codes::NOT_FOUND,
        ));
    }
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

pub(super) async fn mcp_channel_test(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let channel_id = args
        .get("channel_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request().with_param("channel_id"))?;
    // 可选 model：给了就真验这个模型调不调得通，不给只验凭证与连通性
    super::super::admin::ensure_channel_owner(
        state,
        channel_id,
        key,
        key.permission_scope(permissions::CHANNEL_WRITE),
    )
    .await?;
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let result = super::super::admin::probe_channel(state, channel_id, model).await?;
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

pub(super) async fn user_adjust_balance(
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
    super::super::admin::ensure_adjust_amount(amount_micro)?;
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
    let balance_after = okapi_ledger::operations::credit(
        &state.pg,
        &state.ledger,
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
    Ok(
        json!({"dry_run": false, "balance_after_micro": balance_after.balance_after.map(okapi_domain::Money::as_micros), "operation_id": balance_after.operation_id, "pending": balance_after.balance_after.is_none()}),
    )
}

pub(super) async fn user_ban(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let user_id = args
        .get("user_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request().with_param("user_id"))?;
    super::super::user_management::validate(
        state,
        key,
        user_id,
        okapi_store::mutate::UserAction::Ban,
    )
    .await?;
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({"dry_run": true, "user_id": user_id, "action": "ban"}));
    }
    super::super::user_management::apply(state, key, user_id, okapi_store::mutate::UserAction::Ban)
        .await?;
    mcp_audit(state, key, "user.ban", &user_id.to_string(), json!({})).await;
    Ok(json!({"dry_run": false, "banned": true}))
}

pub(super) async fn simulate_pricing(state: &AppState) -> Result<Value, AppError> {
    let rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let source = crate::gateway::pricing_loader::build_source(&rows);
    let models = source.models.len();
    let groups = source.groups.len();
    let base = crate::gateway::pricing_loader::draft_base_price(&state.pg).await?;
    match crate::gateway::pricing_loader::compile_rows(&rows, base) {
        Ok(_) => Ok(json!({"ok": true, "models": models, "groups": groups})),
        Err(err) => Ok(json!({"ok": false, "compile_error": err.to_string()})),
    }
}

pub(super) async fn apply_pricing(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let sim = simulate_pricing(state).await?;
    if sim.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(sim);
    }
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({"dry_run": true, "validation": sim}));
    }
    let rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let base = crate::gateway::pricing_loader::draft_base_price(&state.pg).await?;
    crate::gateway::pricing_loader::compile_rows(&rows, base)
        .map_err(|err| AppError::bad_request().with_param(format!("compile: {err}")))?;
    let mut snapshot = serde_json::to_value(&rows).map_err(|_| AppError::internal())?;
    snapshot["base_price_per_1m_micro"] = json!(base);
    let epoch = okapi_store::admin::publish_epoch(&state.pg, key.user_id, &snapshot).await?;
    if let Some(nats) = &state.nats {
        let _ = nats
            .publish("pricing.epoch", epoch.to_string().into())
            .await;
    }
    mcp_audit(state, key, "pricing.publish", &epoch.to_string(), json!({})).await;
    Ok(json!({"dry_run": false, "epoch": epoch}))
}

pub(super) async fn dlq_requeue(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let ids: Vec<i64> = args
        .get("ids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(AppError::bad_request().with_param("ids"));
    }
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        let members = super::super::dlq::requeue_scope(&state.pg, &ids).await?;
        return Ok(json!({"dry_run": true, "ids": ids,"requeue_members":members}));
    }
    // 与 HTTP /admin/dlq/requeue 同一函数：AI 与人执行的必须是同一个动作
    let outcome = super::super::dlq::requeue(&state.pg, &ids).await?;
    mcp_audit(
        state,
        key,
        "billing.dlq_requeue",
        "batch",
        json!({"ids": ids, "requeued": outcome.count,
            "affected_ids":outcome.dlq_ids,"delivery_batch_ids":outcome.batch_ids}),
    )
    .await;
    Ok(json!({"dry_run": false, "requeued": outcome.count,"delivery_batch_ids":outcome.batch_ids}))
}

pub(super) async fn redemption_create(
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
        .filter(|v| (1..=okapi_ledger::holds::MAXIMUM_MICROS).contains(v))
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

pub(super) async fn mcp_cache_flush(
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
