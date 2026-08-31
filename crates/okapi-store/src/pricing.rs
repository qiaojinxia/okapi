//! 定价配置装载：PG 定价表 → 行对象（倍率一律以 ×1e6 定点整数出库，浮点不出 SQL 层）。
//! 行 → PriceBookSource 的翻译在装配层（bins），okapi-pricing 不依赖存储。

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelPricingRow {
    pub model_name: String,
    pub pricing_mode: String,
    pub model_ratio_scaled: Option<i64>,
    pub completion_ratio_scaled: i64,
    pub cache_ratio_scaled: i64,
    /// 缓存写入倍率（缺省 1.0 = 按常规输入计）。
    pub cache_write_ratio_scaled: i64,
    /// 音频输入倍率（相对文本；缺省 1.0）。
    pub audio_ratio_scaled: i64,
    /// 音频输出倍率（叠乘在 audio 之上；缺省 1.0）。
    pub audio_completion_ratio_scaled: i64,
    /// 图片输入倍率（相对文本；缺省 1.0）。
    pub image_ratio_scaled: i64,
    pub per_call_price_micro: Option<i64>,
    pub tier_expr: Option<String>,
    /// service_tier 档位倍率（JSONB，如 {"flex":"0.5"}；NULL=全档 1.0）。
    pub tier_ratios: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GroupRow {
    pub group_code: String,
    pub ratio_scaled: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserPricingRow {
    pub user_id: i64,
    pub model_name: String,
    pub override_kind: String,
    pub custom_model_ratio_scaled: Option<i64>,
    pub custom_completion_ratio_scaled: Option<i64>,
    pub custom_cache_ratio_scaled: Option<i64>,
    pub custom_cache_write_ratio_scaled: Option<i64>,
    pub custom_input_per_1m_micro: Option<i64>,
    pub custom_output_per_1m_micro: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RuleRow {
    pub rule_code: String,
    pub rule_type: String,
    pub scope: serde_json::Value,
    pub params: serde_json::Value,
    pub priority: i32,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PricingSourceRows {
    pub epoch: i64,
    pub models: Vec<ModelPricingRow>,
    pub groups: Vec<GroupRow>,
    pub overrides: Vec<UserPricingRow>,
    pub rules: Vec<RuleRow>,
}

/// 启用中的模型名列表（/v1/models）。
pub async fn list_active_models(pool: &PgPool) -> Result<Vec<String>, StoreError> {
    let names = sqlx::query_scalar!(
        r#"SELECT model_name FROM models WHERE status = 1 ORDER BY sort_order, model_name"#
    )
    .fetch_all(pool)
    .await?;
    Ok(names)
}

/// 全量装载定价配置（gateway 启动与 epoch 热更时调用）。
// 四段直线 SQL 装载，拆分反而降低可读性
#[allow(clippy::too_many_lines)]
pub async fn load_pricing_source_rows(pool: &PgPool) -> Result<PricingSourceRows, StoreError> {
    let models = sqlx::query!(
        r#"
        SELECT m.model_name,
               p.pricing_mode,
               (p.model_ratio * 1000000)::bigint AS model_ratio_scaled,
               p.tier_ratios,
               (p.completion_ratio * 1000000)::bigint AS "completion_ratio_scaled!",
               (p.cache_ratio * 1000000)::bigint AS "cache_ratio_scaled!",
               (p.cache_write_ratio * 1000000)::bigint AS "cache_write_ratio_scaled!",
               (p.audio_ratio * 1000000)::bigint AS "audio_ratio_scaled!",
               (p.audio_completion_ratio * 1000000)::bigint AS "audio_completion_ratio_scaled!",
               (p.image_ratio * 1000000)::bigint AS "image_ratio_scaled!",
               p.per_call_price_micro,
               p.tier_expr
        FROM model_pricing p
        JOIN models m ON m.id = p.model_id
        WHERE m.status = 1
          AND (p.effective_from IS NULL OR p.effective_from <= now())
        "#
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| ModelPricingRow {
        model_name: r.model_name,
        pricing_mode: r.pricing_mode,
        model_ratio_scaled: r.model_ratio_scaled,
        completion_ratio_scaled: r.completion_ratio_scaled,
        cache_ratio_scaled: r.cache_ratio_scaled,
        cache_write_ratio_scaled: r.cache_write_ratio_scaled,
        audio_ratio_scaled: r.audio_ratio_scaled,
        audio_completion_ratio_scaled: r.audio_completion_ratio_scaled,
        image_ratio_scaled: r.image_ratio_scaled,
        per_call_price_micro: r.per_call_price_micro,
        tier_expr: r.tier_expr,
        tier_ratios: r.tier_ratios,
    })
    .collect();

    let groups = sqlx::query!(
        r#"SELECT group_code, (group_ratio * 1000000)::bigint AS "ratio_scaled!" FROM price_groups"#
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| GroupRow {
        group_code: r.group_code,
        ratio_scaled: r.ratio_scaled,
    })
    .collect();

    let overrides = sqlx::query!(
        r#"
        SELECT up.user_id,
               m.model_name,
               up.override_kind,
               (up.custom_model_ratio * 1000000)::bigint AS custom_model_ratio_scaled,
               (up.custom_completion_ratio * 1000000)::bigint AS custom_completion_ratio_scaled,
               (up.custom_cache_ratio * 1000000)::bigint AS custom_cache_ratio_scaled,
               (up.custom_cache_write_ratio * 1000000)::bigint AS custom_cache_write_ratio_scaled,
               up.custom_input_per_1m_micro,
               up.custom_output_per_1m_micro
        FROM user_pricing up
        JOIN models m ON m.id = up.model_id
        WHERE up.expires_at IS NULL OR up.expires_at > now()
        "#
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| UserPricingRow {
        user_id: r.user_id,
        model_name: r.model_name,
        override_kind: r.override_kind,
        custom_model_ratio_scaled: r.custom_model_ratio_scaled,
        custom_completion_ratio_scaled: r.custom_completion_ratio_scaled,
        custom_cache_ratio_scaled: r.custom_cache_ratio_scaled,
        custom_cache_write_ratio_scaled: r.custom_cache_write_ratio_scaled,
        custom_input_per_1m_micro: r.custom_input_per_1m_micro,
        custom_output_per_1m_micro: r.custom_output_per_1m_micro,
    })
    .collect();

    let rules = sqlx::query!(
        r#"
        SELECT rule_code, rule_type, scope, params, priority, valid_from, valid_to
        FROM pricing_rules
        WHERE enabled
        "#
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| RuleRow {
        rule_code: r.rule_code,
        rule_type: r.rule_type,
        scope: r.scope,
        params: r.params,
        priority: r.priority,
        valid_from: r.valid_from,
        valid_to: r.valid_to,
    })
    .collect();

    let epoch =
        sqlx::query!(r#"SELECT COALESCE(MAX(epoch), 1)::bigint AS "epoch!" FROM pricing_epochs"#)
            .fetch_one(pool)
            .await?
            .epoch;

    Ok(PricingSourceRows {
        epoch,
        models,
        groups,
        overrides,
        rules,
    })
}
