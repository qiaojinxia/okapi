//! 适配规则的存取：版本表 `rule_sets` 与档案表 `compat_profiles` 的读写，以及有效规则装配。
//!
//! 引擎在 `okapi-compat`（无 IO），这里只管几件事：
//! - 读已发布的规则集（渠道层 / 适配档案 / 全局护栏），连同内置厂商层装配成每个渠道的 [`EffectiveRules`]；
//! - 发布新版本：同一事务里把旧的已发布版本归档、写入新版本；
//! - 草稿：每个范围至多一个，带起草时的已发布版本（`base_version`），发布时对不上就拒绝；
//! - 版本历史与回滚（把旧版本的内容作为新版本重新发布）；
//! - 适配档案的增删改，渠道复制时把规则集一并克隆。
//!
//! 已发布的行不可变：改规则 = 新增版本再发布，回滚 = 把旧版本重新发布。草稿是唯一可变的行。
//! 加载失败一律**失败开放**：读到不合法的规则集（手改库、新旧版本不兼容）只记日志并跳过那一层，
//! 不让合法请求因此失败；写入口（控制台）有同一份校验，正常情况下不会产生坏数据。

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use okapi_compat::{EffectiveRules, Layer, LayerInput, Validated, resolve, validate, vendors};
use sqlx::{PgConnection, PgPool};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub const KIND: &str = "compat";

/// 规则集的归属：一个渠道、一个适配档案，或整个网关（全局护栏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Channel(i64),
    /// 适配档案（`compat_profiles.id`）。
    Profile(i64),
    /// 全局护栏：整个网关，最后执行，不可被覆盖。
    Global,
}

impl Scope {
    fn parts(self) -> (&'static str, Option<i64>) {
        match self {
            Self::Channel(id) => ("channel", Some(id)),
            Self::Profile(id) => ("profile", Some(id)),
            Self::Global => ("global", None),
        }
    }
}

/// 一行规则集（任一状态）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RuleSetRow {
    pub id: i64,
    pub version: i32,
    /// `draft` / `published` / `archived`。
    pub status: String,
    pub schema_version: i32,
    pub rules: serde_json::Value,
    pub note: Option<String>,
    pub created_by: Option<i64>,
    /// 草稿：最后一次保存的时间；其它状态：创建时间。
    pub created_at: DateTime<Utc>,
    pub published_by: Option<i64>,
    pub published_at: Option<DateTime<Utc>>,
    /// 草稿起草时的已发布版本（0 = 当时还没有已发布版本）。
    pub base_version: Option<i32>,
    /// `admin` / `mcp` / `system`。
    pub source: String,
    pub rationale: Option<String>,
}

// sqlx 0.9 的查询必须是字面量（不接受 format! 出来的字符串），所以列清单用宏展开成字面量再 concat!
macro_rules! row_columns {
    () => {
        "id, version, status, schema_version, rules, note, created_by, created_at, \
         published_by, published_at, base_version, source, rationale"
    };
}

/// 某个范围当前已发布的规则集。
pub async fn published(pool: &PgPool, scope: Scope) -> Result<Option<RuleSetRow>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    Ok(sqlx::query_as::<_, RuleSetRow>(concat!(
        "SELECT ",
        row_columns!(),
        " FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'published'"
    ))
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_optional(pool)
    .await?)
}

/// 某个范围当前的草稿（至多一个）。
pub async fn draft(pool: &PgPool, scope: Scope) -> Result<Option<RuleSetRow>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    Ok(sqlx::query_as::<_, RuleSetRow>(concat!(
        "SELECT ",
        row_columns!(),
        " FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'draft'"
    ))
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_optional(pool)
    .await?)
}

/// 版本历史，新的在前（含草稿与归档）。
pub async fn versions(
    pool: &PgPool,
    scope: Scope,
    limit: i64,
) -> Result<Vec<RuleSetRow>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    Ok(sqlx::query_as::<_, RuleSetRow>(concat!(
        "SELECT ",
        row_columns!(),
        " FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3
          ORDER BY version DESC
          LIMIT $4"
    ))
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?)
}

/// 指定版本（任一状态）。
pub async fn version(
    pool: &PgPool,
    scope: Scope,
    version: i32,
) -> Result<Option<RuleSetRow>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    Ok(sqlx::query_as::<_, RuleSetRow>(concat!(
        "SELECT ",
        row_columns!(),
        " FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND version = $4"
    ))
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(version)
    .fetch_optional(pool)
    .await?)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

/// 锁住并读出当前已发布版本（没有 = `None`）；与别的发布 / 存草稿串行。
async fn lock_published(conn: &mut PgConnection, scope: Scope) -> Result<Option<i32>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    Ok(sqlx::query_scalar(
        "SELECT version FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'published'
          FOR UPDATE",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_optional(&mut *conn)
    .await?)
}

async fn next_version(conn: &mut PgConnection, scope: Scope) -> Result<i32, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_one(&mut *conn)
    .await?)
}

async fn archive_published(conn: &mut PgConnection, scope: Scope) -> Result<(), StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    sqlx::query(
        "UPDATE rule_sets SET status = 'archived'
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'published'",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// 发布一个新版本（调用方已用 `okapi_compat::validate` 校验过 `rules`）。返回新版本号。
///
/// `base_version`：调用方读到的当前已发布版本（没有 = 0）；与库里不一致说明期间有人改过，
/// 回 `Conflict`，免得两个管理员互相盖掉对方的修改。`None` = 不检查（回滚、系统写入用）。
pub async fn publish(
    conn: &mut PgConnection,
    scope: Scope,
    rules: &serde_json::Value,
    note: Option<&str>,
    actor: Option<i64>,
    base_version: Option<i32>,
) -> Result<i32, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    let current = lock_published(conn, scope).await?;
    if base_version.is_some_and(|base| base != current.unwrap_or(0)) {
        return Err(StoreError::Conflict("rules_version_conflict"));
    }
    let next = next_version(conn, scope).await?;
    archive_published(conn, scope).await?;
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

/// 存草稿的入参。
#[derive(Debug, Clone, Copy)]
pub struct DraftInput<'a> {
    pub rules: &'a serde_json::Value,
    pub note: Option<&'a str>,
    /// AI 提议时写下的依据（给审批的人看）。
    pub rationale: Option<&'a str>,
    pub actor: Option<i64>,
    /// `admin` / `mcp`。
    pub source: &'a str,
    /// 调用方读到的已发布版本（没有 = 0）；对不上就拒绝，免得基于过期的内容起草。
    pub base_version: i32,
}

/// 新建或覆盖这个范围的草稿，返回草稿的版本号。草稿是可变的；已发布的行从不被改。
pub async fn save_draft(
    conn: &mut PgConnection,
    scope: Scope,
    input: DraftInput<'_>,
) -> Result<i32, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    let current = lock_published(conn, scope).await?.unwrap_or(0);
    if input.base_version != current {
        return Err(StoreError::Conflict("rules_version_conflict"));
    }
    let existing: Option<i32> = sqlx::query_scalar(
        "UPDATE rule_sets
            SET rules = $4, note = $5, rationale = $6, source = $7, created_by = $8,
                base_version = $9, schema_version = $10, created_at = now()
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'draft'
          RETURNING version",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(input.rules)
    .bind(input.note)
    .bind(input.rationale)
    .bind(input.source)
    .bind(input.actor)
    .bind(current)
    .bind(i32::try_from(okapi_compat::SCHEMA_VERSION).unwrap_or(1))
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(version) = existing {
        return Ok(version);
    }
    let next = next_version(conn, scope).await?;
    sqlx::query(
        "INSERT INTO rule_sets
            (kind, scope_kind, scope_id, version, status, schema_version, rules, note, rationale,
             source, created_by, base_version)
         VALUES ($1, $2, $3, $4, 'draft', $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(next)
    .bind(i32::try_from(okapi_compat::SCHEMA_VERSION).unwrap_or(1))
    .bind(input.rules)
    .bind(input.note)
    .bind(input.rationale)
    .bind(input.source)
    .bind(input.actor)
    .bind(current)
    .execute(&mut *conn)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            StoreError::Conflict("rules_version_conflict")
        } else {
            StoreError::Sqlx(error)
        }
    })?;
    Ok(next)
}

/// 丢弃这个范围的草稿；原来有草稿返回 `true`。
pub async fn discard_draft(conn: &mut PgConnection, scope: Scope) -> Result<bool, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    let done = sqlx::query(
        "DELETE FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'draft'",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// 发布草稿：同一事务里归档当前已发布版本，草稿改成已发布并取新的版本号。返回新版本号；
/// 没有草稿返回 `None`。草稿起草时的已发布版本与现在对不上（期间有人发布过）回 `Conflict`。
pub async fn publish_draft(
    conn: &mut PgConnection,
    scope: Scope,
    actor: Option<i64>,
) -> Result<Option<i32>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    let current = lock_published(conn, scope).await?.unwrap_or(0);
    let drafted: Option<(i64, Option<i32>)> = sqlx::query_as(
        "SELECT id, base_version FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3 AND status = 'draft'
          FOR UPDATE",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((id, base)) = drafted else {
        return Ok(None);
    };
    if base.unwrap_or(0) != current {
        return Err(StoreError::Conflict("rules_version_conflict"));
    }
    let next = next_version(conn, scope).await?;
    archive_published(conn, scope).await?;
    sqlx::query(
        "UPDATE rule_sets SET status = 'published', version = $2, published_by = $3, published_at = now()
          WHERE id = $1",
    )
    .bind(id)
    .bind(next)
    .bind(actor)
    .execute(&mut *conn)
    .await?;
    Ok(Some(next))
}

/// 把某个旧版本的内容作为新版本重新发布（版本号只增，历史行不变）。返回新版本号；
/// 版本不存在或是草稿返回 `None`。
pub async fn rollback(
    conn: &mut PgConnection,
    scope: Scope,
    to_version: i32,
    actor: Option<i64>,
) -> Result<Option<i32>, StoreError> {
    let (scope_kind, scope_id) = scope.parts();
    let source: Option<(serde_json::Value, i32)> = sqlx::query_as(
        "SELECT rules, schema_version FROM rule_sets
          WHERE kind = $1 AND scope_kind = $2 AND scope_id IS NOT DISTINCT FROM $3
            AND version = $4 AND status <> 'draft'",
    )
    .bind(KIND)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(to_version)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((rules, schema_version)) = source else {
        return Ok(None);
    };
    if u32::try_from(schema_version).map_or(true, |v| v > okapi_compat::SCHEMA_VERSION) {
        return Err(StoreError::Conflict("rules_schema_too_new"));
    }
    let note = format!("回滚到版本 {to_version}");
    publish(conn, scope, &rules, Some(&note), actor, None)
        .await
        .map(Some)
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

// ---------------------------------------------------------------------------------------------
// 适配档案

/// 一个适配档案。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProfileRow {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    /// 叠在哪个内置厂商之上（`builtin:<id>`）。
    pub extends: Option<String>,
    pub hosts: Vec<String>,
    pub created_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

macro_rules! profile_columns {
    () => {
        "id, slug, name, description, extends, hosts, created_by, created_at, updated_at"
    };
}

/// 渠道 `vendor` 字段里档案的前缀。
pub const PROFILE_PREFIX: &str = "custom:";

/// `custom:<slug>` → `<slug>`；不是档案返回 `None`。
#[must_use]
pub fn profile_slug(vendor: &str) -> Option<&str> {
    vendor.strip_prefix(PROFILE_PREFIX)
}

/// 渠道实际叠加的内置厂商（`builtin:<id>`）：直接选了内置厂商就是它；选了档案则是档案 `extends`
/// 的那个。没选 / 档案已不存在 / 档案没叠加厂商时是 `None`。余额探针这类「按厂商取数据」的地方用它。
pub async fn builtin_vendor_of_channel(
    pool: &PgPool,
    channel_id: i64,
) -> Result<Option<String>, StoreError> {
    let vendor: Option<String> = sqlx::query_scalar("SELECT vendor FROM channels WHERE id = $1")
        .bind(channel_id)
        .fetch_optional(pool)
        .await?
        .flatten();
    let Some(vendor) = vendor else {
        return Ok(None);
    };
    match profile_slug(&vendor) {
        Some(slug) => Ok(profile_by_slug(pool, slug)
            .await?
            .and_then(|profile| profile.extends)),
        None => Ok(Some(vendor)),
    }
}

pub async fn list_profiles(pool: &PgPool) -> Result<Vec<ProfileRow>, StoreError> {
    Ok(sqlx::query_as::<_, ProfileRow>(concat!(
        "SELECT ",
        profile_columns!(),
        " FROM compat_profiles ORDER BY slug"
    ))
    .fetch_all(pool)
    .await?)
}

pub async fn profile(pool: &PgPool, id: i64) -> Result<Option<ProfileRow>, StoreError> {
    Ok(sqlx::query_as::<_, ProfileRow>(concat!(
        "SELECT ",
        profile_columns!(),
        " FROM compat_profiles WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

pub async fn profile_by_slug(pool: &PgPool, slug: &str) -> Result<Option<ProfileRow>, StoreError> {
    Ok(sqlx::query_as::<_, ProfileRow>(concat!(
        "SELECT ",
        profile_columns!(),
        " FROM compat_profiles WHERE slug = $1"
    ))
    .bind(slug)
    .fetch_optional(pool)
    .await?)
}

/// 新建档案的入参（`slug` 创建后不可改）。
#[derive(Debug, Clone, Copy)]
pub struct ProfileInput<'a> {
    pub slug: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub extends: Option<&'a str>,
    pub hosts: &'a [String],
}

pub async fn create_profile(
    pool: &PgPool,
    input: ProfileInput<'_>,
    actor: Option<i64>,
) -> Result<ProfileRow, StoreError> {
    sqlx::query_as::<_, ProfileRow>(concat!(
        "INSERT INTO compat_profiles (slug, name, description, extends, hosts, created_by)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING ",
        profile_columns!()
    ))
    .bind(input.slug)
    .bind(input.name)
    .bind(input.description)
    .bind(input.extends)
    .bind(input.hosts)
    .bind(actor)
    .fetch_one(pool)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            StoreError::Conflict("profile_slug_taken")
        } else {
            StoreError::Sqlx(error)
        }
    })
}

/// 改档案的名称 / 说明 / 叠加的厂商 / 主机建议；`None` = 不动。`description` 与 `extends` 用
/// 双层 `Option`：外层 `None` 不动，`Some(None)` 清空。
pub async fn update_profile(
    pool: &PgPool,
    id: i64,
    name: Option<&str>,
    description: Option<Option<&str>>,
    extends: Option<Option<&str>>,
    hosts: Option<&[String]>,
) -> Result<Option<ProfileRow>, StoreError> {
    Ok(sqlx::query_as::<_, ProfileRow>(concat!(
        "UPDATE compat_profiles SET
            name = COALESCE($2, name),
            description = CASE WHEN $3 THEN $4 ELSE description END,
            extends = CASE WHEN $5 THEN $6 ELSE extends END,
            hosts = COALESCE($7, hosts),
            updated_at = now()
         WHERE id = $1 RETURNING ",
        profile_columns!()
    ))
    .bind(id)
    .bind(name)
    .bind(description.is_some())
    .bind(description.flatten())
    .bind(extends.is_some())
    .bind(extends.flatten())
    .bind(hosts)
    .fetch_optional(pool)
    .await?)
}

/// 引用每个档案的（未删除）渠道数，键是档案 slug。
pub async fn profile_usage(pool: &PgPool) -> Result<HashMap<String, i64>, StoreError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT substr(vendor, 8), count(*) FROM channels
          WHERE vendor LIKE 'custom:%' AND deleted_at IS NULL GROUP BY 1",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// 删除档案及其规则集。还有渠道在用就拒绝（`profile_in_use`）；没有这个档案返回 `false`。
pub async fn delete_profile(conn: &mut PgConnection, id: i64) -> Result<bool, StoreError> {
    let slug: Option<String> =
        sqlx::query_scalar("SELECT slug FROM compat_profiles WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(slug) = slug else {
        return Ok(false);
    };
    let in_use: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channels WHERE vendor = $1 AND deleted_at IS NULL",
    )
    .bind(format!("{PROFILE_PREFIX}{slug}"))
    .fetch_one(&mut *conn)
    .await?;
    if in_use > 0 {
        return Err(StoreError::Conflict("profile_in_use"));
    }
    sqlx::query(
        "DELETE FROM rule_sets WHERE kind = $1 AND scope_kind = 'profile' AND scope_id = $2",
    )
    .bind(KIND)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM compat_profiles WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(true)
}

// ---------------------------------------------------------------------------------------------
// 装配

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

/// 档案层的输入（档案本身 + 已发布的那一版；档案还没发布过规则时 `published` 为 `None`）。
#[derive(Debug, Clone, Copy)]
pub struct ProfileLayer<'a> {
    pub slug: &'a str,
    /// 叠在哪个内置厂商之上（`builtin:<id>`）。
    pub extends: Option<&'a str>,
    pub published: Option<PublishedSet<'a>>,
}

/// 已发布规则集的最小输入。
#[derive(Debug, Clone, Copy)]
pub struct PublishedSet<'a> {
    pub version: i32,
    pub schema_version: i32,
    pub rules: &'a serde_json::Value,
}

/// 可装配的几层输入。
#[derive(Debug, Default)]
pub struct Layers<'a> {
    /// `channels.vendor`（带命名空间：`builtin:deepseek` 或 `custom:<slug>`）。
    pub vendor: Option<&'a str>,
    /// `vendor` 是档案（`custom:<slug>`）时，档案的信息。
    pub profile: Option<ProfileLayer<'a>>,
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

/// 内置厂商层（找不到记 warn 并跳过：目录随版本变，旧库里可能还留着已下线的厂商 id）。
fn builtin_layer(id: &str) -> Option<LayerInput> {
    if let Some(vendor) = vendors::get(id) {
        Some(vendor.layer())
    } else {
        tracing::warn!(vendor = id, "选了未知的内置厂商，跳过厂商层");
        None
    }
}

/// 把厂商 / 档案 / 渠道 / 护栏装配成有效规则。纯函数，控制台「有效规则」与网关加载共用。
#[must_use]
pub fn assemble(layers: &Layers<'_>) -> EffectiveRules {
    let mut inputs = Vec::with_capacity(4);
    match (layers.vendor, &layers.profile) {
        // 档案：先是它叠加的内置厂商，再是档案自己的已发布规则集
        (Some(vendor), Some(profile)) if profile_slug(vendor).is_some() => {
            inputs.extend(profile.extends.and_then(builtin_layer));
            if let Some(set) = profile.published {
                let label = format!("{PROFILE_PREFIX}{}@v{}", profile.slug, set.version);
                if let Some(rules) = compile(&label, set.schema_version, set.rules) {
                    inputs.push(LayerInput {
                        layer: Layer::Profile,
                        label,
                        rules,
                    });
                }
            }
        }
        // 选了档案但档案已不存在（手改库）：不套任何厂商层
        (Some(vendor), None) if profile_slug(vendor).is_some() => {
            tracing::warn!(vendor, "渠道选了不存在的适配档案，跳过厂商层");
        }
        (Some(vendor), _) => inputs.extend(builtin_layer(vendor)),
        (None, _) => {}
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

/// 渠道 `api_base` 的主机名（小写）。选择器的 `host` 与能力合并都用它。
#[must_use]
pub fn host_of(api_base: Option<&str>) -> Option<String> {
    reqwest::Url::parse(api_base?)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// 一次装配一批渠道的有效规则（候选加载用）：几条查询，护栏只编译一次。
/// `channels`：(渠道 id, `channels.vendor`)。没有任何一层的渠道共用同一个空集合。
pub async fn load_effective(
    pool: &PgPool,
    channels: &[(i64, Option<String>)],
) -> Result<HashMap<i64, Arc<EffectiveRules>>, StoreError> {
    if channels.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<i64> = channels.iter().map(|(id, _)| *id).collect();
    // 用到的档案（按 slug）：渠道的 vendor 是 custom:<slug> 才查
    let slugs: Vec<String> = channels
        .iter()
        .filter_map(|(_, vendor)| vendor.as_deref().and_then(profile_slug).map(str::to_owned))
        .collect();
    let profiles: HashMap<String, (i64, Option<String>)> = if slugs.is_empty() {
        HashMap::new()
    } else {
        let rows: Vec<(String, i64, Option<String>)> =
            sqlx::query_as("SELECT slug, id, extends FROM compat_profiles WHERE slug = ANY($1)")
                .bind(&slugs)
                .fetch_all(pool)
                .await?;
        rows.into_iter()
            .map(|(slug, id, extends)| (slug, (id, extends)))
            .collect()
    };
    let profile_ids: Vec<i64> = profiles.values().map(|(id, _)| *id).collect();
    let rows: Vec<(String, Option<i64>, i32, i32, serde_json::Value)> = sqlx::query_as(
        "SELECT scope_kind, scope_id, version, schema_version, rules
           FROM rule_sets
          WHERE kind = $1 AND status = 'published'
            AND (scope_kind = 'global'
                 OR (scope_kind = 'channel' AND scope_id = ANY($2))
                 OR (scope_kind = 'profile' AND scope_id = ANY($3)))",
    )
    .bind(KIND)
    .bind(&ids)
    .bind(&profile_ids)
    .fetch_all(pool)
    .await?;
    let mut guard = None;
    let mut per_channel = HashMap::new();
    let mut per_profile = HashMap::new();
    for (scope_kind, scope_id, version, schema_version, rules) in rows {
        match (scope_kind.as_str(), scope_id) {
            ("global", _) => guard = guard_layer(version, schema_version, &rules),
            ("channel", Some(channel_id)) => {
                per_channel.insert(channel_id, (version, schema_version, rules));
            }
            ("profile", Some(profile_id)) => {
                per_profile.insert(profile_id, (version, schema_version, rules));
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
            let profile = vendor
                .as_deref()
                .and_then(profile_slug)
                .and_then(|slug| profiles.get_key_value(slug))
                .map(|(slug, (profile_id, extends))| ProfileLayer {
                    slug,
                    extends: extends.as_deref(),
                    published: per_profile.get(profile_id).map(
                        |(version, schema_version, rules)| PublishedSet {
                            version: *version,
                            schema_version: *schema_version,
                            rules,
                        },
                    ),
                });
            let effective = if vendor.is_none() && channel.is_none() && guard.is_none() {
                Arc::clone(&empty)
            } else {
                Arc::new(assemble(&Layers {
                    vendor: vendor.as_deref(),
                    profile,
                    channel,
                    guard: guard.as_ref(),
                }))
            };
            (*channel_id, effective)
        })
        .collect())
}

/// 把能力阶段规则给出的事实并进渠道自己声明的能力：**渠道声明过的（真或假）一律保留**，
/// 规则只补渠道没说的。渠道的能力不是对象（手改库）时当作没有声明。
#[must_use]
pub fn merge_capabilities(
    channel: serde_json::Value,
    facts: &BTreeMap<String, bool>,
) -> serde_json::Value {
    if facts.is_empty() {
        return channel;
    }
    let mut merged = match channel {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    for (name, value) in facts {
        merged
            .entry(name.clone())
            .or_insert(serde_json::Value::Bool(*value));
    }
    serde_json::Value::Object(merged)
}

/// 候选加载时的能力合并：用渠道的有效规则里能力阶段的事实补全 `capabilities`。
/// 选择器只会用到厂商 / 主机 / 上游模型（校验器限定），其余维度此时还没有请求，填占位。
#[must_use]
pub fn candidate_capabilities(
    effective: &EffectiveRules,
    vendor: Option<&str>,
    api_base: Option<&str>,
    requested_model: &str,
    upstream_model: &str,
    capabilities: serde_json::Value,
) -> serde_json::Value {
    if !effective.has_phase(okapi_compat::Phase::Capability) {
        return capabilities;
    }
    let host = host_of(api_base);
    let ctx = okapi_compat::Context {
        vendor,
        host: host.as_deref(),
        model: upstream_model,
        client_model: requested_model,
        dialect: "",
        operation: "",
        stream: false,
        client: None,
    };
    merge_capabilities(capabilities, &effective.capabilities(&ctx))
}
