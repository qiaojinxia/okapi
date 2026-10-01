//! Atomic model draft writes. Metadata describes the model; it never enables a route.
use crate::{StoreError, admin::RatioAxes};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;

pub const CAPABILITIES: &[&str] = &[
    "vision",
    "tools",
    "parallel_tools",
    "json",
    "structured_output",
    "reasoning",
    "audio",
    "video",
    "embedding",
    "realtime",
    "streaming",
    "prompt_cache",
    "system_prompt",
    "temperature",
    "web_search",
    "computer_use",
];
pub const MODEL_KINDS: &[&str] = &[
    "chat",
    "completion",
    "embedding",
    "rerank",
    "image_generation",
    "speech_to_text",
    "text_to_speech",
    "realtime",
    "video_generation",
    "moderation",
    "search",
];

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelMetadata {
    pub display_name: Option<String>,
    pub vendor: Option<String>,
    pub description: Option<String>,
    pub kind: Option<String>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub capabilities: serde_json::Map<String, Value>,
    pub context_window: Option<i32>,
    pub max_output: Option<i32>,
}

impl ModelMetadata {
    pub fn validate(&self) -> Result<(), &'static str> {
        for (value, limit, param) in [
            (&self.display_name, 128, "metadata.display_name"),
            (&self.vendor, 64, "metadata.vendor"),
            (&self.description, 2000, "metadata.description"),
        ] {
            if value.as_ref().is_some_and(|s| s.chars().count() > limit) {
                return Err(param);
            }
        }
        if self
            .kind
            .as_deref()
            .is_some_and(|s| !MODEL_KINDS.contains(&s))
        {
            return Err("metadata.kind");
        }
        for values in [&self.input_modalities, &self.output_modalities] {
            if values.len() > 4
                || values
                    .iter()
                    .any(|s| !["text", "image", "audio", "video"].contains(&s.as_str()))
                || values
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != values.len()
            {
                return Err("metadata.modalities");
            }
        }
        if self
            .capabilities
            .iter()
            .any(|(key, v)| !CAPABILITIES.contains(&key.as_str()) || !v.is_boolean())
        {
            return Err("metadata.capabilities");
        }
        if self.context_window.is_some_and(|v| v <= 0) || self.max_output.is_some_and(|v| v <= 0) {
            return Err("metadata.limits");
        }
        Ok(())
    }

    fn catalog(&self) -> Value {
        json!({ "kind": self.kind, "description": self.description,
            "input_modalities": self.input_modalities, "output_modalities": self.output_modalities })
    }
}

pub struct ModelDraft<'a> {
    pub name: &'a str,
    pub axes: RatioAxes<'a>,
    /// None preserves the existing mode (a new model defaults to ratio).
    pub mode: Option<&'a str>,
    pub tier_expr: Option<&'a str>,
    pub per_call_price_micro: Option<i64>,
    pub tier_ratios: Option<&'a Value>,
    pub fallbacks: Option<&'a [String]>,
    pub metadata: Option<&'a ModelMetadata>,
}

pub async fn save(pool: &PgPool, draft: ModelDraft<'_>) -> Result<i64, StoreError> {
    let mut tx = pool.begin().await?;
    let chain = if let Some(raw) = draft.fallbacks {
        let mut chain = Vec::<String>::new();
        for item in raw {
            let name = item.trim();
            if !name.is_empty() && name != draft.name && !chain.iter().any(|s| s == name) {
                chain.push(name.to_owned());
            }
        }
        if chain.len() > 8 {
            return Err(StoreError::InvalidData("fallback_models"));
        }
        let known: i64 =
            sqlx::query_scalar("SELECT count(*) FROM models WHERE model_name = ANY($1)")
                .bind(&chain)
                .fetch_one(&mut *tx)
                .await?;
        if usize::try_from(known).unwrap_or(0) != chain.len() {
            return Err(StoreError::InvalidData("fallback_models"));
        }
        Some(json!(chain))
    } else {
        None
    };
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO models (model_name, vendor) VALUES ($1, $2) ON CONFLICT (model_name) DO UPDATE SET vendor = COALESCE(models.vendor, EXCLUDED.vendor), updated_at = now() RETURNING id"
    ).bind(draft.name).bind(crate::vendor::classify(draft.name)).fetch_one(&mut *tx).await?;
    if let Some(meta) = draft.metadata {
        sqlx::query("UPDATE models SET display_name = $2, vendor = $3, capabilities = $4, context_window = $5, max_output = $6, catalog_config = $7 WHERE id = $1")
            .bind(id).bind(&meta.display_name)
            .bind(meta.vendor.as_deref().or_else(|| crate::vendor::classify(draft.name)))
            .bind(Value::Object(meta.capabilities.clone())).bind(meta.context_window)
            .bind(meta.max_output).bind(meta.catalog()).execute(&mut *tx).await?;
    }
    if let Some(chain) = chain {
        sqlx::query("UPDATE models SET fallback_models = $2 WHERE id = $1")
            .bind(id)
            .bind(chain)
            .execute(&mut *tx)
            .await?;
    }
    let a = draft.axes;
    sqlx::query(r"INSERT INTO model_pricing (model_id, pricing_mode, model_ratio, completion_ratio,
        cache_ratio, cache_write_ratio, audio_ratio, audio_completion_ratio, image_ratio,
        modality_ratios, tier_expr, per_call_price_micro, tier_ratios)
        VALUES ($1, COALESCE($2, 'ratio'), ($3::text)::numeric, ($4::text)::numeric,
        ($5::text)::numeric, ($6::text)::numeric, ($7::text)::numeric, ($8::text)::numeric,
        ($9::text)::numeric, COALESCE($10, '{}'::jsonb), $11, $12, NULLIF($13, '{}'::jsonb))
        ON CONFLICT (model_id) DO UPDATE SET
        pricing_mode = COALESCE($2, model_pricing.pricing_mode),
        model_ratio = EXCLUDED.model_ratio, completion_ratio = EXCLUDED.completion_ratio,
        cache_ratio = EXCLUDED.cache_ratio, cache_write_ratio = EXCLUDED.cache_write_ratio,
        audio_ratio = EXCLUDED.audio_ratio, audio_completion_ratio = EXCLUDED.audio_completion_ratio,
        image_ratio = EXCLUDED.image_ratio,
        modality_ratios = COALESCE($10, model_pricing.modality_ratios),
        tier_expr = CASE WHEN $2 = 'tiered' THEN $11 WHEN $2 IS NOT NULL THEN NULL ELSE model_pricing.tier_expr END,
        per_call_price_micro = CASE WHEN $2 = 'per_call' THEN $12 WHEN $2 IS NOT NULL THEN NULL ELSE model_pricing.per_call_price_micro END,
        tier_ratios = CASE WHEN $13::jsonb IS NULL THEN model_pricing.tier_ratios ELSE NULLIF($13, '{}'::jsonb) END,
        updated_at = now()")
        .bind(id).bind(draft.mode).bind(a.model).bind(a.completion).bind(a.cache).bind(a.cache_write)
        .bind(a.audio).bind(a.audio_completion).bind(a.image).bind(a.modality_ratios)
        .bind(draft.tier_expr).bind(draft.per_call_price_micro).bind(draft.tier_ratios)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_preserves_unknown_vs_false_and_validates_bounds() {
        let mut m: ModelMetadata = serde_json::from_value(json!({"kind":"chat", "input_modalities":["text","image"], "capabilities":{"vision":true,"tools":false}, "context_window":128_000})).unwrap();
        assert!(m.validate().is_ok());
        assert!(!m.capabilities.contains_key("reasoning"));
        m.max_output = Some(0);
        assert_eq!(m.validate(), Err("metadata.limits"));
        m.max_output = None;
        m.input_modalities.push("image".into());
        assert_eq!(m.validate(), Err("metadata.modalities"));
    }
}
