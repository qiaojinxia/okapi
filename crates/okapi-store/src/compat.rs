//! 适配规则的存取：版本表 `rule_sets` 的读写与有效规则装配。
//!
//! 引擎在 `okapi-compat`（无 IO），这里只管三件事：
//! - 读已发布的规则集（渠道层 / 全局护栏），连同内置厂商层装配成每个渠道的 [`EffectiveRules`]；
//! - 发布新版本：同一事务里把旧的已发布版本归档、写入新版本；
//! - 渠道复制时把规则集一并克隆。
//!
//! 已发布的行不可变：改规则 = 新增版本再发布，回滚 = 把旧版本重新发布（P1 只有前一半的接口）。
//! 加载失败一律**失败开放**：读到不合法的规则集（手改库、新旧版本不兼容）只记日志并跳过那一层，
//! 不让合法请求因此失败；写入口（控制台）有同一份校验，正常情况下不会产生坏数据。

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use okapi_compat::{EffectiveRules, Layer, LayerInput, Validated, resolve, validate, vendors};
use sqlx::{PgConnection, PgPool};
use std::collections::HashMap;
use std::sync::Arc;

pub const KIND: &str = "compat";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Channel(i64),
    /// 全局护栏：整个网关，最后执行，不可被覆盖。
    Global,
}

impl Scope {
    fn parts(self) -> (&'static str, Option<i64>) {
        match self {
            Self::Channel(id) => ("channel", Some(id)),
            Self::Global => ("global", None),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RuleSetRow {
    pub id: i64,
    pub version: i32,
    pub schema_version: i32,
    pub rules: serde_json::Value,
    pub note: Option<String>,
    pub created_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

/// 某个范围当前已发布的规则集。
pub async fn published(pool: &PgPool, scope: Scope) -> Result<Option<RuleSetRow>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    Ok(sqlx::query_as::<_, RuleSetRow>(
        "SELECT id, version, schema_version, rules, note, created_by, created_at, published_at
           FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'published'",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_optional(pool)
    .await?)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

/// 发布一个新版本（调用方已用 `okapi_compat::validate` 校验过 `rules`）。返回新版本号。
///
/// `base_version`：调用方读到的当前已发布版本（没有 = 0）；与库里不一致说明期间有人改过，
/// 回 `Conflict`，免得两个管理员互相盖掉对方的修改。
pub async fn publish(
    conn: &mut PgConnection,
    scope: Scope,
    rules: &serde_json::Value,
    note: Option<&str>,
    actor: Option<i64>,
    base_version: Option<i32>,
) -> Result<i32, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    let current: Option<i32> = sqlx::query_scalar(
        "SELECT version FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'published'
          FOR UPDATE",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_optional(&mut *conn)
    .await?;
    if base_version.is_some_and(|base| base != current.unwrap_or(0)) {
        return Err(StoreError::Conflict("rules_version_conflict"));
    }
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE rule_sets SET status = 'archived'
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'published'",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO rule_sets
            (kind, scope_kind, scope_id, version, status, schema_version, rules, note,
             created_by, published_by, published_at)
         VALUES ($1, $2, $3, $4, 'published', $5, $6, $7, $8, $8, now())",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(next)
    .bind(i32::try_from(okapi_compat::SCHEMA_VERSION).unwrap_or(1))
    .bind(rules)
    .bind(note)
    .bind(actor)
    .execute(&mut *conn)
    .await
    .map_err(|error| {
        // 两个管理员同时给一个还没有规则集的范围发布：唯一索引拦下后到的那个
        if is_unique_violation(&error) {
            StoreError::Conflict("rules_version_conflict")
        } else {
            StoreError::Sqlx(error)
        }
    })?;
    Ok(next)
}

/// 渠道复制时克隆已发布的规则集（新渠道从版本 1 起）。源没有规则集则什么也不做。
pub async fn copy_channel_rules(
    conn: &mut PgConnection,
    from_channel: i64,
    to_channel: i64,
    actor: Option<i64>,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO rule_sets
            (kind, scope_kind, scope_id, version, status, schema_version, rules, note,
             created_by, published_by, published_at)
         SELECT kind, scope_kind, $2, 1, 'published', schema_version, rules,
                '复制自渠道 #' || $1::text, $3, $3, now()
           FROM rule_sets
          WHERE kind = $4 AND scope_kind = 'channel' AND scope_id = $1 AND status = 'published'",
    )
    .bind(from_channel)
    .bind(to_channel)
    .bind(actor)
    .bind(KIND)
    .execute(conn)
    .await?;
    Ok(())
}

/// 校验一个已存的规则集；不合法返回 `None` 并记日志（失败开放）。
/// schema 版本比本二进制认识的新（写入了新版本后又回滚了镜像）同样跳过，免得按旧语义误读新规则。
fn compile(label: &str, schema_version: i32, rules: &serde_json::Value) -> Option<Arc<Validated>> {
    if u32::try_from(schema_version).map_or(true, |v| v > okapi_compat::SCHEMA_VERSION) {
        tracing::error!(
            set = label,
            schema_version,
            "适配规则集的 schema 版本比本程序新，已跳过这一层"
        );
        return None;
    }
    match validate(rules) {
        Ok(valid) => Some(Arc::new(valid)),
        Err(issues) => {
            tracing::error!(
                set = label,
                issues = %issues.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
                "已发布的适配规则集校验不过，已跳过这一层"
            );
            None
        }
    }
}

/// 渠道层的输入（已发布的那一版）。
#[derive(Debug, Clone, Copy)]
pub struct ChannelLayer<'a> {
    pub channel_id: i64,
    pub version: i32,
    pub schema_version: i32,
    pub rules: &'a serde_json::Value,
}

/// 可装配的几层输入。
#[derive(Debug, Default)]
pub struct Layers<'a> {
    /// `channels.vendor`（带命名空间）。
    pub vendor: Option<&'a str>,
    pub channel: Option<ChannelLayer<'a>>,
    pub guard: Option<&'a LayerInput>,
}

/// 全局护栏的解析输入；不合法返回 `None`。
#[must_use]
pub fn guard_layer(
    version: i32,
    schema_version: i32,
    rules: &serde_json::Value,
) -> Option<LayerInput> {
    let label = format!("guard@v{version}");
    compile(&label, schema_version, rules).map(|rules| LayerInput {
        layer: Layer::Guard,
        label,
        rules,
    })
}

/// 把厂商 / 渠道 / 护栏装配成有效规则。纯函数，控制台「有效规则」与网关加载共用。
#[must_use]
pub fn assemble(layers: &Layers<'_>) -> EffectiveRules {
    let mut inputs = Vec::with_capacity(3);
    if let Some(id) = layers.vendor {
        if let Some(vendor) = vendors::get(id) {
            inputs.push(vendor.layer());
        } else {
            tracing::warn!(vendor = id, "渠道选了未知的厂商，跳过厂商层");
        }
    }
    if let Some(channel) = layers.channel {
        let label = format!("channel#{}@v{}", channel.channel_id, channel.version);
        if let Some(rules) = compile(&label, channel.schema_version, channel.rules) {
            inputs.push(LayerInput {
                layer: Layer::Channel,
                label,
                rules,
            });
        }
    }
    inputs.extend(layers.guard.cloned());
    resolve(inputs)
}

/// 一次装配一批渠道的有效规则（候选加载用）：一条查询，护栏只编译一次。
/// `channels`：(渠道 id, `channels.vendor`)。没有任何一层的渠道共用同一个空集合。
pub async fn load_effective(
    pool: &PgPool,
    channels: &[(i64, Option<String>)],
) -> Result<HashMap<i64, Arc<EffectiveRules>>, StoreError> {
    if channels.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<i64> = channels.iter().map(|(id, _)| *id).collect();
    let rows: Vec<(String, Option<i64>, i32, i32, serde_json::Value)> = sqlx::query_as(
        "SELECT scope_kind, scope_id, version, schema_version, rules
           FROM rule_sets
          WHERE kind = $1 AND status = 'published'
            AND (scope_kind = 'global' OR (scope_kind = 'channel' AND scope_id = ANY($2)))",
    )
    .bind(KIND)
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let mut guard = None;
    let mut per_channel = HashMap::new();
    for (scope_kind, scope_id, version, schema_version, rules) in rows {
        match (scope_kind.as_str(), scope_id) {
            ("global", _) => guard = guard_layer(version, schema_version, &rules),
            ("channel", Some(channel_id)) => {
                per_channel.insert(channel_id, (version, schema_version, rules));
            }
            _ => {}
        }
    }
    let empty = Arc::new(EffectiveRules::default());
    Ok(channels
        .iter()
        .map(|(channel_id, vendor)| {
            let channel = per_channel
                .get(channel_id)
                .map(|(version, schema_version, rules)| ChannelLayer {
                    channel_id: *channel_id,
                    version: *version,
                    schema_version: *schema_version,
                    rules,
                });
            let effective = if vendor.is_none() && channel.is_none() && guard.is_none() {
                Arc::clone(&empty)
            } else {
                Arc::new(assemble(&Layers {
                    vendor: vendor.as_deref(),
                    channel,
                    guard: guard.as_ref(),
                }))
            };
            (*channel_id, effective)
        })
        .collect())
}
