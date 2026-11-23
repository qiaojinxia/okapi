//! 定价发布安全闭环：历史版本列表、草稿/历史差异 Diff、事务级快照回滚、发布前计价试算。
use super::super::query::{PageQuery, Query};
use super::{audit, guard};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_domain::{GroupCode, ModelCode, TokenUsage, UserId};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::{Json as ExtractJson, Path};
use okapi_pricing::{CalcContext, PriceBook, RatioFp};
use okapi_store::pricing::PricingSourceRows;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(sqlx::FromRow)]
struct EpochRow {
    epoch: i64,
    published_by: Option<i64>,
    published_at: chrono::DateTime<chrono::Utc>,
    diff_summary: Option<Value>,
    snapshot: Value,
}

/// GET /admin/pricing/epochs：查看已发布的价簿历史快照摘要。
pub async fn list_pricing_epochs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let slice = q.slice();
    let (limit, offset) = (slice.capped_limit(), slice.offset);
    let (rows, total) = tokio::try_join!(
        sqlx::query_as::<_, EpochRow>(
            "SELECT epoch, published_by, published_at, diff_summary, snapshot \
             FROM pricing_epochs ORDER BY epoch DESC LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pg),
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*)::bigint FROM pricing_epochs")
            .fetch_one(&state.pg),
    )
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let count = |k: &str| {
                r.snapshot
                    .get(k)
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            };
            json!({
                "epoch": r.epoch,
                "published_by": r.published_by,
                "published_at": r.published_at.to_rfc3339(),
                "diff_summary": r.diff_summary,
                "base_price_per_1m_micro": r.snapshot.get("base_price_per_1m_micro"),
                "models_count": count("models"),
                "groups_count": count("groups"),
                "rules_count": count("rules"),
                "overrides_count": count("overrides"),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "total": total })))
}

#[derive(Deserialize, Default)]
pub struct DiffPricingQuery {
    #[serde(default)]
    pub from_epoch: Option<i64>,
    #[serde(default)]
    pub to_epoch: Option<i64>,
}

async fn load_epoch_snapshot(
    pg: &sqlx::PgPool,
    epoch: i64,
) -> Result<(PricingSourceRows, i64), AppError> {
    let snapshot: Option<Value> =
        sqlx::query_scalar("SELECT snapshot FROM pricing_epochs WHERE epoch = $1")
            .bind(epoch)
            .fetch_optional(pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    let Some(snapshot) = snapshot else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    let base = snapshot
        .get("base_price_per_1m_micro")
        .and_then(Value::as_i64)
        .unwrap_or(okapi_pricing::book::BASE_PRICE_PER_1M_MICRO);
    let mut source: PricingSourceRows = serde_json::from_value(snapshot)
        .map_err(|_| okapi_store::StoreError::InvalidData("pricing_snapshot"))?;
    source.epoch = epoch;
    Ok((source, base))
}

fn diff_maps(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> (Value, bool) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (k, v) in after {
        match before.get(k) {
            None => added.push(json!({ "key": k, "value": v })),
            Some(old) if old != v => {
                changed.push(json!({ "key": k, "before": old, "after": v }));
            }
            Some(_) => {}
        }
    }
    for (k, v) in before {
        if !after.contains_key(k) {
            removed.push(json!({ "key": k, "value": v }));
        }
    }
    let dirty = !added.is_empty() || !removed.is_empty() || !changed.is_empty();
    (
        json!({ "added": added, "removed": removed, "changed": changed }),
        dirty,
    )
}

fn index_source(rows: &PricingSourceRows) -> [BTreeMap<String, Value>; 4] {
    let models = rows
        .models
        .iter()
        .map(|m| (m.model_name.clone(), json!(m)))
        .collect();
    let groups = rows
        .groups
        .iter()
        .map(|g| (g.group_code.clone(), json!(g)))
        .collect();
    let rules = rows
        .rules
        .iter()
        .map(|r| (r.rule_code.clone(), json!(r)))
        .collect();
    let overrides = rows
        .overrides
        .iter()
        .map(|o| (format!("{}:{}", o.user_id, o.model_name), json!(o)))
        .collect();
    [models, groups, rules, overrides]
}

/// GET /admin/pricing/diff：对比两个定价版本（缺省：最新已发布 epoch vs 当前草稿）。
pub async fn diff_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DiffPricingQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let (from_epoch, from_rows, from_base) = if let Some(ep) = q.from_epoch {
        let (rows, base) = load_epoch_snapshot(&state.pg, ep).await?;
        (ep, rows, base)
    } else {
        let mut conn = state
            .pg
            .acquire()
            .await
            .map_err(okapi_store::StoreError::from)?;
        let pub_p = okapi_store::pricing::published_pricing(&mut conn).await?;
        let base = okapi_kernel::pricing_loader::publication_base_price(&pub_p)?;
        (pub_p.source.epoch, pub_p.source, base)
    };
    let (to_epoch, to_rows, to_base) = if let Some(ep) = q.to_epoch {
        let (rows, base) = load_epoch_snapshot(&state.pg, ep).await?;
        (Some(ep), rows, base)
    } else {
        let rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
        let base = okapi_kernel::pricing_loader::draft_base_price(&state.pg).await?;
        (None, rows, base)
    };
    let [m1, g1, r1, o1] = index_source(&from_rows);
    let [m2, g2, r2, o2] = index_source(&to_rows);
    let (models, md) = diff_maps(&m1, &m2);
    let (groups, gd) = diff_maps(&g1, &g2);
    let (rules, rd) = diff_maps(&r1, &r2);
    let (overrides, od) = diff_maps(&o1, &o2);
    let base_changed = from_base != to_base;
    Ok(Json(json!({
        "from_epoch": from_epoch,
        "to_epoch": to_epoch,
        "has_changes": base_changed || md || gd || rd || od,
        "base_price_per_1m_micro": if base_changed {
            json!({ "before": from_base, "after": to_base })
        } else {
            Value::Null
        },
        "models": models,
        "groups": groups,
        "rules": rules,
        "overrides": overrides,
    })))
}

fn scaled_to_decimal(scaled: i64) -> String {
    RatioFp::from_scaled(scaled).map_or_else(|| "1".to_owned(), |r| r.to_string())
}

/// POST /admin/pricing/epochs/{epoch}/rollback：将历史快照事务性恢复到草稿表并发布新 epoch。
pub async fn rollback_pricing_epoch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_epoch): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_PUBLISH).await?;
    let (rows, base) = load_epoch_snapshot(&state.pg, target_epoch).await?;
    if let Err(err) = okapi_kernel::pricing_loader::compile_rows(&rows, base) {
        return Err(AppError::bad_request().with_param(format!("compile: {err}")));
    }

    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    restore_draft_in_tx(&mut tx, &rows, base, actor.actor_user_id()).await?;

    let mut snapshot = serde_json::to_value(&rows).map_err(|_| AppError::internal())?;
    snapshot["base_price_per_1m_micro"] = json!(base);
    let diff_summary = json!({ "rollback_to_epoch": target_epoch });
    let new_epoch: i64 = sqlx::query_scalar(
        "INSERT INTO pricing_epochs (snapshot, diff_summary, published_by) \
         VALUES ($1, $2, $3) RETURNING epoch",
    )
    .bind(&snapshot)
    .bind(&diff_summary)
    .bind(actor.actor_user_id())
    .fetch_one(&mut *tx)
    .await
    .map_err(okapi_store::StoreError::from)?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;

    if let Some(nats) = &state.nats {
        let _ = nats
            .publish("pricing.epoch", new_epoch.to_string().into())
            .await;
    }
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "pricing.rollback",
        &new_epoch.to_string(),
        json!({ "target_epoch": target_epoch, "new_epoch": new_epoch }),
    )
    .await;
    Ok(Json(json!({
        "epoch": new_epoch,
        "rolled_back_to": target_epoch,
    })))
}

async fn restore_draft_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &PricingSourceRows,
    base: i64,
    actor_id: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_by) VALUES ($1, $2, $3) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(okapi_kernel::pricing_loader::BASE_PRICE_SETTING)
    .bind(json!(base))
    .bind(actor_id)
    .execute(&mut **tx)
    .await
    .map_err(okapi_store::StoreError::from)?;

    for g in &rows.groups {
        sqlx::query(
            "INSERT INTO price_groups (group_code, group_ratio, pool_code) \
             VALUES ($1, ($2::text)::numeric, 'default') \
             ON CONFLICT (group_code) DO UPDATE SET group_ratio = EXCLUDED.group_ratio",
        )
        .bind(&g.group_code)
        .bind(scaled_to_decimal(g.ratio_scaled))
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }

    restore_models_in_tx(tx, rows).await?;
    restore_rules_and_overrides_in_tx(tx, rows).await?;
    Ok(())
}

async fn restore_models_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &PricingSourceRows,
) -> Result<(), AppError> {
    let mut restored_model_ids: Vec<i64> = Vec::with_capacity(rows.models.len());
    for m in &rows.models {
        let model_id: i64 = sqlx::query_scalar(
            "INSERT INTO models (model_name, status) VALUES ($1, 1) \
             ON CONFLICT (model_name) DO UPDATE SET status = 1 RETURNING id",
        )
        .bind(&m.model_name)
        .fetch_one(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
        restored_model_ids.push(model_id);
        sqlx::query(
            r"INSERT INTO model_pricing (
                  model_id, pricing_mode, model_ratio, completion_ratio, cache_ratio,
                  cache_write_ratio, audio_ratio, audio_completion_ratio, image_ratio,
                  modality_ratios, server_tool_prices, per_call_price_micro, tier_expr, tier_ratios
              ) VALUES (
                  $1, $2, ($3::text)::numeric, ($4::text)::numeric, ($5::text)::numeric,
                  ($6::text)::numeric, ($7::text)::numeric, ($8::text)::numeric, ($9::text)::numeric,
                  $10, $11, $12, $13, $14
              )
              ON CONFLICT (model_id) DO UPDATE SET
                  pricing_mode = EXCLUDED.pricing_mode,
                  model_ratio = EXCLUDED.model_ratio,
                  completion_ratio = EXCLUDED.completion_ratio,
                  cache_ratio = EXCLUDED.cache_ratio,
                  cache_write_ratio = EXCLUDED.cache_write_ratio,
                  audio_ratio = EXCLUDED.audio_ratio,
                  audio_completion_ratio = EXCLUDED.audio_completion_ratio,
                  image_ratio = EXCLUDED.image_ratio,
                  modality_ratios = EXCLUDED.modality_ratios,
                  server_tool_prices = EXCLUDED.server_tool_prices,
                  per_call_price_micro = EXCLUDED.per_call_price_micro,
                  tier_expr = EXCLUDED.tier_expr,
                  tier_ratios = EXCLUDED.tier_ratios",
        )
        .bind(model_id)
        .bind(&m.pricing_mode)
        .bind(m.model_ratio_scaled.map(scaled_to_decimal))
        .bind(scaled_to_decimal(m.completion_ratio_scaled))
        .bind(scaled_to_decimal(m.cache_ratio_scaled))
        .bind(scaled_to_decimal(m.cache_write_ratio_scaled))
        .bind(scaled_to_decimal(m.audio_ratio_scaled))
        .bind(scaled_to_decimal(m.audio_completion_ratio_scaled))
        .bind(scaled_to_decimal(m.image_ratio_scaled))
        .bind(&m.modality_ratios)
        .bind(&m.server_tool_prices)
        .bind(m.per_call_price_micro)
        .bind(&m.tier_expr)
        .bind(&m.tier_ratios)
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    sqlx::query("DELETE FROM model_pricing WHERE NOT (model_id = ANY($1))")
        .bind(&restored_model_ids)
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(())
}

async fn restore_rules_and_overrides_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &PricingSourceRows,
) -> Result<(), AppError> {
    sqlx::query("UPDATE pricing_rules SET enabled = false")
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    for r in &rows.rules {
        sqlx::query(
            "INSERT INTO pricing_rules (rule_code, rule_type, scope, params, priority, enabled, valid_from, valid_to) \
             VALUES ($1, $2, $3, $4, $5, true, $6, $7) \
             ON CONFLICT (rule_code) DO UPDATE SET \
               rule_type = EXCLUDED.rule_type, scope = EXCLUDED.scope, params = EXCLUDED.params, \
               priority = EXCLUDED.priority, enabled = true, valid_from = EXCLUDED.valid_from, valid_to = EXCLUDED.valid_to",
        )
        .bind(&r.rule_code)
        .bind(&r.rule_type)
        .bind(&r.scope)
        .bind(&r.params)
        .bind(r.priority)
        .bind(r.valid_from)
        .bind(r.valid_to)
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }

    sqlx::query("DELETE FROM user_pricing")
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    for o in &rows.overrides {
        let model_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM models WHERE model_name = $1")
                .bind(&o.model_name)
                .fetch_optional(&mut **tx)
                .await
                .map_err(okapi_store::StoreError::from)?;
        let Some(model_id) = model_id else {
            continue;
        };
        let user_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
                .bind(o.user_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(okapi_store::StoreError::from)?;
        if !user_exists {
            continue;
        }
        sqlx::query(
            "INSERT INTO user_pricing ( \
                 user_id, model_id, override_kind, custom_model_ratio, custom_completion_ratio, \
                 custom_cache_ratio, custom_cache_write_ratio, custom_input_per_1m_micro, custom_output_per_1m_micro \
             ) VALUES ( \
                 $1, $2, $3, ($4::text)::numeric, ($5::text)::numeric, \
                 ($6::text)::numeric, ($7::text)::numeric, $8, $9 \
             )",
        )
        .bind(o.user_id)
        .bind(model_id)
        .bind(&o.override_kind)
        .bind(o.custom_model_ratio_scaled.map(scaled_to_decimal))
        .bind(o.custom_completion_ratio_scaled.map(scaled_to_decimal))
        .bind(o.custom_cache_ratio_scaled.map(scaled_to_decimal))
        .bind(o.custom_cache_write_ratio_scaled.map(scaled_to_decimal))
        .bind(o.custom_input_per_1m_micro)
        .bind(o.custom_output_per_1m_micro)
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct SimulatePricingReq {
    pub model: String,
    #[serde(default = "default_group")]
    pub group_code: String,
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub user_multiplier: Option<String>,
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub cached_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub monthly_tokens: u64,
    #[serde(default)]
    pub monthly_spend_micro: u64,
    #[serde(default)]
    pub surge_active: bool,
}

fn default_group() -> String {
    "default".to_owned()
}

fn quote_json(book: &PriceBook, ctx: &CalcContext, usage: TokenUsage) -> Value {
    match okapi_pricing::calculate(book, ctx, usage) {
        Ok(q) => json!({
            "epoch": book.epoch(),
            "amount_micro": q.amount.as_micros(),
            "original_micro": q.original.as_micros(),
            "discount_micro": q.discount.as_micros(),
            "list_price_micro": q.list_price.as_micros(),
            "applied_rules": q.snapshot.rules,
        }),
        Err(err) => json!({
            "epoch": book.epoch(),
            "error": err.to_string(),
        }),
    }
}

/// POST /admin/pricing/simulate：对比「当前生效价簿」与「未发布草稿」的实时计价结果与差额。
pub async fn simulate_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<SimulatePricingReq>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let user_multiplier = match req.user_multiplier.as_deref() {
        Some(raw) => raw
            .trim()
            .parse::<RatioFp>()
            .map_err(|_| AppError::bad_request().with_param("user_multiplier"))?,
        None => RatioFp::ONE,
    };
    let now = chrono::Utc::now();
    let utc_offset = now.with_timezone(&chrono::Local).offset().local_minus_utc();
    let local_min = u16::try_from(
        (now.timestamp()
            .saturating_add(i64::from(utc_offset))
            .div_euclid(60))
        .rem_euclid(1440),
    )
    .unwrap_or(0);
    let ctx = CalcContext {
        user: UserId::new(req.user_id.unwrap_or(0)),
        model: ModelCode::from(req.model.trim()),
        group: GroupCode::from(req.group_code.trim()),
        user_multiplier,
        monthly_tokens: req.monthly_tokens,
        monthly_spend_micro: req.monthly_spend_micro,
        local_minute_of_day: local_min,
        now_unix: now.timestamp(),
        utc_offset_seconds: utc_offset,
        surge_active: req.surge_active,
        service_tier: req.service_tier,
    };
    let usage = TokenUsage {
        prompt_tokens: req.prompt_tokens,
        cached_tokens: req.cached_tokens,
        cache_read_reported: req.cached_tokens > 0,
        cache_write_tokens: req.cache_write_tokens,
        cache_write_reported: req.cache_write_tokens > 0,
        completion_tokens: req.completion_tokens,
        ..Default::default()
    };

    let draft_rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let draft_base = okapi_kernel::pricing_loader::draft_base_price(&state.pg).await?;
    let draft_book = okapi_kernel::pricing_loader::compile_rows(&draft_rows, draft_base)
        .map_err(|err| AppError::bad_request().with_param(format!("compile: {err}")))?;

    let mut conn = state
        .pg
        .acquire()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let pub_p = okapi_store::pricing::published_pricing(&mut conn).await?;
    let pub_base = okapi_kernel::pricing_loader::publication_base_price(&pub_p)?;
    let pub_book = okapi_kernel::pricing_loader::compile_rows(&pub_p.source, pub_base)
        .unwrap_or_else(|_| state.pricebook.load().as_ref().clone());

    let published = quote_json(&pub_book, &ctx, usage);
    let draft = quote_json(&draft_book, &ctx, usage);
    let delta = draft["amount_micro"]
        .as_i64()
        .zip(published["amount_micro"].as_i64())
        .map(|(d, p)| d.saturating_sub(p));
    Ok(Json(json!({
        "published": published,
        "draft": draft,
        "delta_micro": delta,
    })))
}
