//! 审计快照与变更前后（before/after）差异构造器。
//!
//! 所有读取均为只读辅助快照（容错返回 `Option<Value>`）：不能因快照查询失败反过来阻塞主写流程；
//! 敏感字段（如 `settings` 内嵌密钥、代理密码）一律脱敏或不读取。

use serde_json::{Map, Value, json};
use sqlx::{PgPool, Row};

/// 从 `before` 全量快照中按本次变更字段集抽出最小化的 `(before, after)` 对照对象。
pub(crate) fn diff_fields(
    before: Option<&Value>,
    changed: &Map<String, Value>,
) -> (Value, Value) {
    let mut before_map = Map::new();
    let mut after_map = Map::new();
    let prev_obj = before.and_then(Value::as_object);
    for (k, new_v) in changed {
        let old_v = prev_obj
            .and_then(|m| m.get(k))
            .cloned()
            .unwrap_or(Value::Null);
        before_map.insert(k.clone(), old_v);
        after_map.insert(k.clone(), new_v.clone());
    }
    (Value::Object(before_map), Value::Object(after_map))
}

pub(crate) async fn channel_snapshot(pg: &PgPool, id: i64) -> Option<Value> {
    let row = sqlx::query(
        "SELECT name, provider, api_base, status, models, model_mapping, settings,
                capabilities, priority, trust_upstream_usage, cost_milli
         FROM channels WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pg)
    .await
    .ok()??;
    let settings: Value = row.try_get("settings").unwrap_or(Value::Null);
    let models: Value = row.try_get("models").unwrap_or(Value::Null);
    let models_count = models.as_array().map_or(0, Vec::len);
    Some(json!({
        "name": row.try_get::<String, _>("name").ok(),
        "provider": row.try_get::<String, _>("provider").ok(),
        "api_base": row.try_get::<Option<String>, _>("api_base").ok().flatten(),
        "status": row.try_get::<i16, _>("status").ok(),
        "models": models,
        "models_count": models_count,
        "model_mapping": row.try_get::<Value, _>("model_mapping").unwrap_or(Value::Null),
        "settings": super::manage::redact_secret_fields(&settings),
        "capabilities": row.try_get::<Value, _>("capabilities").unwrap_or(Value::Null),
        "priority": row.try_get::<i32, _>("priority").ok(),
        "trust_upstream_usage": row.try_get::<bool, _>("trust_upstream_usage").ok(),
        "cost_milli": row.try_get::<Option<i64>, _>("cost_milli").ok().flatten(),
    }))
}

pub(crate) async fn channel_pools_snapshot(pg: &PgPool, channel_id: i64) -> Value {
    let rows = sqlx::query(
        "SELECT pool_code, priority_override, weight_override, role, enabled
         FROM pool_channels WHERE channel_id = $1 ORDER BY pool_code",
    )
    .bind(channel_id)
    .fetch_all(pg)
    .await
    .unwrap_or_default();
    let list: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "pool_code": r.try_get::<String, _>("pool_code").ok(),
                "priority_override": r.try_get::<Option<i32>, _>("priority_override").ok().flatten(),
                "weight_override": r.try_get::<Option<i32>, _>("weight_override").ok().flatten(),
                "role": r.try_get::<String, _>("role").ok(),
                "enabled": r.try_get::<bool, _>("enabled").ok(),
            })
        })
        .collect();
    Value::Array(list)
}

pub(crate) async fn api_key_snapshot(
    pg: &PgPool,
    id: i64,
    owner_id: Option<i64>,
) -> Option<Value> {
    let row = sqlx::query(
        "SELECT user_id, name, key_prefix, status, quota_micro, daily_quota_micro,
                monthly_quota_micro, expires_at, group_override, pool_override,
                model_allowlist, ip_allowlist, rpm_limit, tpm_limit, rpd_limit,
                daily_token_limit, max_concurrency
         FROM api_keys
         WHERE id = $1 AND ($2::bigint IS NULL OR user_id = $2) AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "user_id": row.try_get::<i64, _>("user_id").ok(),
        "name": row.try_get::<Option<String>, _>("name").ok().flatten(),
        "key_prefix": row.try_get::<String, _>("key_prefix").ok(),
        "status": row.try_get::<i16, _>("status").ok(),
        "quota_micro": row.try_get::<Option<i64>, _>("quota_micro").ok().flatten(),
        "daily_quota_micro": row.try_get::<Option<i64>, _>("daily_quota_micro").ok().flatten(),
        "monthly_quota_micro": row.try_get::<Option<i64>, _>("monthly_quota_micro").ok().flatten(),
        "expires_at": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("expires_at").ok().flatten(),
        "group_override": row.try_get::<Option<String>, _>("group_override").ok().flatten(),
        "pool_override": row.try_get::<Option<String>, _>("pool_override").ok().flatten(),
        "model_allowlist": row.try_get::<Option<Value>, _>("model_allowlist").ok().flatten(),
        "ip_allowlist": row.try_get::<Option<Value>, _>("ip_allowlist").ok().flatten(),
        "rpm_limit": row.try_get::<Option<i32>, _>("rpm_limit").ok().flatten(),
        "tpm_limit": row.try_get::<Option<i32>, _>("tpm_limit").ok().flatten(),
        "rpd_limit": row.try_get::<Option<i32>, _>("rpd_limit").ok().flatten(),
        "daily_token_limit": row.try_get::<Option<i64>, _>("daily_token_limit").ok().flatten(),
        "max_concurrency": row.try_get::<Option<i32>, _>("max_concurrency").ok().flatten(),
    }))
}

pub(crate) async fn user_snapshot(pg: &PgPool, id: i64) -> Option<Value> {
    let row = sqlx::query(
        "SELECT username, email, role, status, admin_role_id,
                price_multiplier::text AS price_multiplier, balance_expires_at
         FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "username": row.try_get::<String, _>("username").ok(),
        "email": row.try_get::<Option<String>, _>("email").ok().flatten(),
        "role": row.try_get::<i16, _>("role").ok(),
        "status": row.try_get::<i16, _>("status").ok(),
        "admin_role_id": row.try_get::<Option<i64>, _>("admin_role_id").ok().flatten(),
        "price_multiplier": row.try_get::<String, _>("price_multiplier").ok(),
        "balance_expires_at": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("balance_expires_at").ok().flatten(),
    }))
}

pub(crate) async fn role_snapshot(pg: &PgPool, role_code: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT role_code, display_name, permissions FROM admin_roles WHERE role_code = $1",
    )
    .bind(role_code)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "role_code": row.try_get::<String, _>("role_code").ok(),
        "display_name": row.try_get::<String, _>("display_name").ok(),
        "permissions": row.try_get::<Value, _>("permissions").unwrap_or(Value::Null),
    }))
}

pub(crate) async fn pool_snapshot(pg: &PgPool, pool_code: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT pool_code, description, routing_strategy, fallback_pool_code
         FROM channel_pools WHERE pool_code = $1",
    )
    .bind(pool_code)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "pool_code": row.try_get::<String, _>("pool_code").ok(),
        "description": row.try_get::<Option<String>, _>("description").ok().flatten(),
        "routing_strategy": row.try_get::<String, _>("routing_strategy").ok(),
        "fallback_pool_code": row.try_get::<Option<String>, _>("fallback_pool_code").ok().flatten(),
    }))
}

pub(crate) async fn group_snapshot(pg: &PgPool, group_code: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT group_code, group_ratio::text AS group_ratio, description, pool_code,
                self_select, rpm_limit, rph_limit
         FROM price_groups WHERE group_code = $1",
    )
    .bind(group_code)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "group_code": row.try_get::<String, _>("group_code").ok(),
        "group_ratio": row.try_get::<String, _>("group_ratio").ok(),
        "description": row.try_get::<Option<String>, _>("description").ok().flatten(),
        "pool_code": row.try_get::<String, _>("pool_code").ok(),
        "self_select": row.try_get::<bool, _>("self_select").ok(),
        "rpm_limit": row.try_get::<Option<i32>, _>("rpm_limit").ok().flatten(),
        "rph_limit": row.try_get::<Option<i32>, _>("rph_limit").ok().flatten(),
    }))
}

pub(crate) async fn model_snapshot(pg: &PgPool, model_name: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT m.model_name, m.display_name, m.vendor,
                mp.pricing_mode, mp.model_ratio::text AS model_ratio,
                mp.completion_ratio::text AS completion_ratio,
                mp.per_call_price_micro
         FROM models m
         LEFT JOIN model_pricing mp ON mp.model_id = m.id
         WHERE m.model_name = $1",
    )
    .bind(model_name)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "model_name": row.try_get::<String, _>("model_name").ok(),
        "display_name": row.try_get::<Option<String>, _>("display_name").ok().flatten(),
        "vendor": row.try_get::<Option<String>, _>("vendor").ok().flatten(),
        "pricing_mode": row.try_get::<Option<String>, _>("pricing_mode").ok().flatten(),
        "model_ratio": row.try_get::<Option<String>, _>("model_ratio").ok().flatten(),
        "completion_ratio": row.try_get::<Option<String>, _>("completion_ratio").ok().flatten(),
        "per_call_price_micro": row.try_get::<Option<i64>, _>("per_call_price_micro").ok().flatten(),
    }))
}

pub(crate) async fn rule_snapshot(pg: &PgPool, rule_code: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT rule_code, rule_type, scope, params, priority, enabled, valid_from, valid_to
         FROM pricing_rules WHERE rule_code = $1",
    )
    .bind(rule_code)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "rule_code": row.try_get::<String, _>("rule_code").ok(),
        "rule_type": row.try_get::<String, _>("rule_type").ok(),
        "scope": row.try_get::<Value, _>("scope").unwrap_or(Value::Null),
        "params": row.try_get::<Value, _>("params").unwrap_or(Value::Null),
        "priority": row.try_get::<i32, _>("priority").ok(),
        "enabled": row.try_get::<bool, _>("enabled").ok(),
        "valid_from": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("valid_from").ok().flatten(),
        "valid_to": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("valid_to").ok().flatten(),
    }))
}

pub(crate) async fn plan_snapshot(pg: &PgPool, plan_code: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT plan_code, display_name, kind, grant_micro, price_micro, group_code,
                balance_valid_days, period, duration_days
         FROM plans WHERE plan_code = $1",
    )
    .bind(plan_code)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "plan_code": row.try_get::<String, _>("plan_code").ok(),
        "display_name": row.try_get::<String, _>("display_name").ok(),
        "kind": row.try_get::<i16, _>("kind").ok(),
        "grant_micro": row.try_get::<i64, _>("grant_micro").ok(),
        "price_micro": row.try_get::<i64, _>("price_micro").ok(),
        "group_code": row.try_get::<Option<String>, _>("group_code").ok().flatten(),
        "balance_valid_days": row.try_get::<Option<i32>, _>("balance_valid_days").ok().flatten(),
        "period": row.try_get::<Option<i16>, _>("period").ok().flatten(),
        "duration_days": row.try_get::<Option<i32>, _>("duration_days").ok().flatten(),
    }))
}

pub(crate) async fn proxy_snapshot(pg: &PgPool, id: i64) -> Option<Value> {
    let row = sqlx::query(
        "SELECT name, scheme, host, port, status, max_keys, note
         FROM proxies WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "name": row.try_get::<String, _>("name").ok(),
        "scheme": row.try_get::<String, _>("scheme").ok(),
        "host": row.try_get::<String, _>("host").ok(),
        "port": row.try_get::<i32, _>("port").ok(),
        "status": row.try_get::<i16, _>("status").ok(),
        "max_keys": row.try_get::<Option<i32>, _>("max_keys").ok().flatten(),
        "note": row.try_get::<Option<String>, _>("note").ok().flatten(),
    }))
}

pub(crate) async fn proxy_group_snapshot(pg: &PgPool, code: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT g.code, g.name, g.mode, g.description,
                (SELECT COUNT(*)::bigint FROM proxy_group_members m WHERE m.group_code = g.code) AS member_count
         FROM proxy_groups g WHERE g.code = $1",
    )
    .bind(code)
    .fetch_optional(pg)
    .await
    .ok()??;
    Some(json!({
        "code": row.try_get::<String, _>("code").ok(),
        "name": row.try_get::<String, _>("name").ok(),
        "mode": row.try_get::<String, _>("mode").ok(),
        "description": row.try_get::<Option<String>, _>("description").ok().flatten(),
        "member_count": row.try_get::<i64, _>("member_count").ok(),
    }))
}

/// 构造 `ApiKeyPatch` 变更字段集（供管理端与用户自助改 key 共用 diff）。
pub(crate) fn api_key_patch_changes(patch: &okapi_store::admin::ApiKeyPatch) -> Map<String, Value> {
    let mut changed = Map::new();
    if let Some(v) = &patch.name {
        changed.insert("name".into(), json!(v));
    }
    if let Some(v) = patch.status {
        changed.insert("status".into(), json!(v));
    }
    if let Some(v) = patch.quota_micro {
        changed.insert("quota_micro".into(), json!(v));
    }
    if let Some(v) = patch.daily_quota_micro {
        changed.insert("daily_quota_micro".into(), json!(v));
    }
    if let Some(v) = patch.monthly_quota_micro {
        changed.insert("monthly_quota_micro".into(), json!(v));
    }
    if let Some(v) = patch.expires_at {
        changed.insert("expires_at".into(), json!(v));
    }
    if let Some(v) = &patch.group_override {
        changed.insert("group_override".into(), json!(v));
    }
    if let Some(v) = &patch.pool_override {
        changed.insert("pool_override".into(), json!(v));
    }
    if let Some(v) = &patch.model_allowlist {
        changed.insert("model_allowlist".into(), json!(v));
    }
    if let Some(v) = &patch.ip_allowlist {
        changed.insert("ip_allowlist".into(), json!(v));
    }
    if let Some(v) = patch.rpm_limit {
        changed.insert("rpm_limit".into(), json!(v));
    }
    if let Some(v) = patch.tpm_limit {
        changed.insert("tpm_limit".into(), json!(v));
    }
    if let Some(v) = patch.rpd_limit {
        changed.insert("rpd_limit".into(), json!(v));
    }
    if let Some(v) = patch.daily_token_limit {
        changed.insert("daily_token_limit".into(), json!(v));
    }
    if let Some(v) = patch.max_concurrency {
        changed.insert("max_concurrency".into(), json!(v));
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_fields_extracts_only_modified_keys() {
        let before = json!({
            "name": "old-ch",
            "priority": 0,
            "provider": "openai",
            "models": ["gpt-4o"],
        });
        let mut changed = Map::new();
        changed.insert("priority".into(), json!(10));
        changed.insert("name".into(), json!("new-ch"));
        let (b, a) = diff_fields(Some(&before), &changed);
        assert_eq!(b, json!({"priority": 0, "name": "old-ch"}));
        assert_eq!(a, json!({"priority": 10, "name": "new-ch"}));
    }

    #[test]
    fn diff_fields_handles_missing_before_snapshot() {
        let mut changed = Map::new();
        changed.insert("status".into(), json!(2));
        let (b, a) = diff_fields(None, &changed);
        assert_eq!(b, json!({"status": null}));
        assert_eq!(a, json!({"status": 2}));
    }
}
