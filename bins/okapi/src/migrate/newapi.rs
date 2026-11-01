//! new-api 三表导出（users / tokens / channels）→ Okapi。
use super::{
    KeySource, MigrateStats, ensure_group_and_pool, expired_time, i, provider_of, read_jsonl,
    require_export, s, sync_imported_key,
};
use okapi_domain::Money;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::Path;

/// 执行迁移。`ledger` 为 None 时（dry-run 或 Redis 不可用）余额只统计不入账。
// 线性三表导入脚本，拆分割裂迁移时序
#[allow(clippy::too_many_lines)]
pub async fn run_newapi(
    pg: &PgPool,
    ledger: Option<&okapi_ledger::BalanceLedger>,
    dir: &Path,
    dry_run: bool,
    master_key: Option<&str>,
) -> anyhow::Result<MigrateStats> {
    require_export(dir, &["users.jsonl", "tokens.jsonl", "channels.jsonl"])?;
    let mut stats = MigrateStats::default();

    // ---- users ----
    // 源 id → 新 id（tokens 关联用）
    let mut user_map: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for row in read_jsonl(dir, "users.jsonl")? {
        let row = row?;
        let Some(username) = s(&row, "username") else {
            stats.skipped.push(format!("users: 缺 username（{row}）"));
            continue;
        };
        let Some(src_id) = i(&row, "id") else {
            stats.skipped.push(format!("users:{username} 缺 id"));
            continue;
        };
        let quota = i(&row, "quota").unwrap_or(0);
        // 500000 quota = $1 = 1_000_000 micro → ×2
        let balance_micro = quota.saturating_mul(2);
        let role: i16 = match i(&row, "role").unwrap_or(1) {
            r if r >= 100 => 100,
            r if r >= 10 => 10,
            _ => 1,
        };
        let row_status: i16 = if i(&row, "status").unwrap_or(1) == 1 {
            1
        } else {
            2
        };
        stats.users += 1;
        if balance_micro > 0 {
            stats.users_credited += 1;
        }
        if dry_run {
            user_map.insert(src_id, -1);
            continue;
        }
        let email = s(&row, "email");
        let user_id = sqlx::query_scalar!(
            r#"
            INSERT INTO users (username, email, role, status)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (username) DO UPDATE SET
                email = COALESCE(EXCLUDED.email, users.email),
                role = EXCLUDED.role,
                status = EXCLUDED.status,
                updated_at = now()
            RETURNING id
            "#,
            username,
            email,
            role,
            row_status
        )
        .fetch_one(pg)
        .await?;
        user_map.insert(src_id, user_id);

        // 用户分组 → user_groups（同名价格分组按需建，指向同名池）
        if let Some(group) = s(&row, "group")
            .map(str::trim)
            .filter(|g| !g.is_empty() && *g != "default")
        {
            ensure_group_and_pool(pg, group).await?;
            sqlx::query!(
                r#"INSERT INTO user_groups (user_id, group_code, priority) VALUES ($1, $2, 10)
                   ON CONFLICT (user_id, group_code) DO NOTHING"#,
                user_id,
                group
            )
            .execute(pg)
            .await?;
        }

        // 每用户/来源一条事件；幂等检查与双侧写入共用用户锁。
        if balance_micro > 0 {
            okapi_ledger::operations::import_credit(
                pg,
                ledger,
                user_id,
                Money::from_micros(balance_micro),
                "system:migrate",
                serde_json::json!({"tags": ["newapi_import"], "src_quota": quota}),
            )
            .await?;
        }
    }

    // ---- tokens → api_keys ----
    for row in read_jsonl(dir, "tokens.jsonl")? {
        let row = row?;
        let Some(key) = s(&row, "key") else {
            stats.skipped.push("tokens: 缺 key".to_owned());
            continue;
        };
        let Some(new_user) = i(&row, "user_id").and_then(|src| user_map.get(&src).copied()) else {
            stats
                .skipped
                .push(format!("tokens: user_id 无映射（{row}）"));
            continue;
        };
        stats.keys += 1;
        if dry_run {
            continue;
        }
        // new-api 存明文 key（可能不带 sk- 前缀）；Okapi 只存 SHA-256
        let full = if key.starts_with("sk-") {
            key.to_owned()
        } else {
            format!("sk-{key}")
        };
        let key_hash = hex::encode(Sha256::digest(full.as_bytes()));
        let prefix: String = full.chars().take(16).collect();
        let name = s(&row, "name").unwrap_or("newapi-import");
        let row_status: i16 = if i(&row, "status").unwrap_or(1) == 1 {
            1
        } else {
            2
        };
        // 令牌分组：new-api 令牌可钉住某分组（价随组走）；auto / 空 = 跟随用户分组
        let token_group = s(&row, "group")
            .map(str::trim)
            .filter(|g| !g.is_empty() && *g != "auto" && *g != "default");
        if let Some(g) = token_group {
            ensure_group_and_pool(pg, g).await?;
        }
        // 过期时间：new-api 的 `expired_time` 是 Unix 秒，-1（或 0）= 永不过期。此前整列被忽略，
        // 带过期时间的令牌迁过来就成了永久有效；写不进来的值记注记，不静默当永久
        let expires_at = expired_time(&row);
        if expires_at.is_none()
            && row.get("expired_time").is_some_and(|v| !v.is_null())
            && i(&row, "expired_time").is_none()
        {
            stats.skipped.push(format!(
                "tokens:{name} expired_time 无法解析，按永久有效导入"
            ));
        }
        sqlx::query!(
            r#"
            INSERT INTO api_keys (user_id, key_hash, key_prefix, name, status, group_override, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (key_hash) DO UPDATE SET status = EXCLUDED.status,
                                                 group_override = EXCLUDED.group_override,
                                                 expires_at = EXCLUDED.expires_at
            "#,
            new_user,
            key_hash,
            prefix,
            name,
            row_status,
            token_group,
            expires_at
        )
        .execute(pg)
        .await?;
    }

    // ---- channels ----
    for row in read_jsonl(dir, "channels.jsonl")? {
        let row = row?;
        let Some(name) = s(&row, "name") else {
            stats.skipped.push("channels: 缺 name".to_owned());
            continue;
        };
        let Some(credential) = s(&row, "key") else {
            stats.skipped.push(format!("channels:{name} 缺 key"));
            continue;
        };
        let newapi_type = i(&row, "type").unwrap_or(1);
        let (provider, exact) = provider_of(newapi_type);
        if !exact {
            stats.skipped.push(format!(
                "channels:{name} type={newapi_type} 按 openai_compat 导入"
            ));
        }
        let models: Vec<String> = s(&row, "models")
            .map(|m| {
                m.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        // 渠道分组 CSV → 所属池；空 = 只进 default 池
        let channel_groups: Vec<String> = s(&row, "group")
            .map(|g| {
                g.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| vec![okapi_store::channels::DEFAULT_POOL.to_owned()]);
        stats.channels += 1;
        if dry_run {
            continue;
        }
        let base_url = s(&row, "base_url").unwrap_or("");
        let row_status: i16 = if i(&row, "status").unwrap_or(1) == 1 {
            1
        } else {
            2
        };
        let priority = i32::try_from(i(&row, "priority").unwrap_or(0)).unwrap_or(0);
        let weight = i32::try_from(i(&row, "weight").unwrap_or(1))
            .unwrap_or(1)
            .max(1);
        // 幂等：name 唯一 upsert（new-api 渠道名可重复——追加 #type 消歧由导出侧保证）
        let channel_id = sqlx::query_scalar!(
            r#"
            SELECT id FROM channels WHERE name = $1 AND deleted_at IS NULL
            "#,
            name
        )
        .fetch_optional(pg)
        .await?;
        if let Some(channel_id) = channel_id {
            sqlx::query!(
                r#"UPDATE channels SET provider = $2, api_base = NULLIF($3, ''),
                   models = $4, priority = $5, status = $6, updated_at = now()
                   WHERE id = $1"#,
                channel_id,
                provider,
                base_url,
                serde_json::json!(models),
                priority,
                row_status
            )
            .execute(pg)
            .await?;
            // 凭证与权重也跟着源系统走：此前只改渠道行，源系统轮换 key 后重跑迁移，这里仍是旧凭证
            let source = KeySource {
                name,
                credential,
                weight,
                master_key,
            };
            sync_imported_key(pg, channel_id, &source, &mut stats.skipped).await?;
        } else {
            let model_refs: Vec<&str> = models.iter().map(String::as_str).collect();
            let (channel_id, key_id) = okapi_store::provision::create_channel(
                pg,
                name,
                provider,
                base_url,
                credential,
                &model_refs,
                false,
                master_key,
            )
            .await?;
            sqlx::query!(
                r#"UPDATE channels SET priority = $2, status = $3 WHERE id = $1"#,
                channel_id,
                priority,
                row_status
            )
            .execute(pg)
            .await?;
            sqlx::query!(
                r#"UPDATE channel_keys SET weight = $2 WHERE id = $1"#,
                key_id,
                weight
            )
            .execute(pg)
            .await?;
        }
        // 池成员关系：按 new-api 的分组 CSV 覆盖式写入（每个分组名一个同名池）
        let target = sqlx::query_scalar!(
            r#"SELECT id FROM channels WHERE name = $1 AND deleted_at IS NULL"#,
            name
        )
        .fetch_one(pg)
        .await?;
        // 模型映射随行导入（JSON 字符串列；非对象忽略）——对 azure 是必需项：映射值就是部署名。
        // new-api 把 azure 的 api-version 放在 `other` 列，落到 settings.api_version（§11.29）。
        let mapping = s(&row, "model_mapping")
            .and_then(|m| serde_json::from_str::<Value>(m).ok())
            .filter(Value::is_object);
        let api_version = (provider == "azure")
            .then(|| s(&row, "other"))
            .flatten()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if mapping.is_some() || api_version.is_some() {
            sqlx::query!(
                r#"UPDATE channels
                   SET model_mapping = COALESCE($2, model_mapping),
                       settings = CASE WHEN $3::text IS NULL THEN settings
                                       ELSE settings || jsonb_build_object('api_version', $3::text) END
                   WHERE id = $1"#,
                target,
                mapping,
                api_version
            )
            .execute(pg)
            .await?;
        }
        for g in &channel_groups {
            ensure_group_and_pool(pg, g).await?;
        }
        okapi_store::admin::set_channel_pool_codes(pg, target, &channel_groups).await?;
    }

    Ok(stats)
}
