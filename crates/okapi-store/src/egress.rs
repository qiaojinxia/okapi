//! 出口代理（IMPLEMENTATION §11.41）。
//!
//! 代理是一等资源；渠道的出口绑定四态：继承全局默认 / 直连 / 单个代理 / 代理组。
//! 「有效出口」的口径只有一处：视图 `channel_egress` + 函数 `egress_pick`（迁移 0037）。
//! 候选查询、控制面解析、路由诊断都经它们，Rust 侧不重写继承规则。
//!
//! 固定分配（pinned 组）的结果持久化在 `channel_keys.egress_proxy_id`：任何改变绑定、组员或
//! 全局默认的写操作，在同一事务里跑一次 [`reconcile`]——全量对账，而不是按变更去推算哪些 key
//! 受影响；哪条写路径漏了，也会在下一次写时自愈。对账只在「分到的代理已不是组员」时改分配，
//! 从不因为代理熔断或停用而换 IP（默认等它恢复）。

use crate::error::StoreError;
use crate::listing::{Page, Slice, count_unless_all, len_as_total};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use std::collections::HashMap;

/// 全局默认出口所在的 settings 键（值为 [`Binding`] 的 JSON；不允许 `inherit`）。
pub const DEFAULT_SETTING: &str = "egress_default";

/// 固定分配串行化：分配要先读「每个代理已分了几把」再写，而 `max_keys` 跨组共享，故全局一把锁。
const ASSIGN_LOCK: i64 = 0x0E6E_5500;

/// 被动熔断：连续 3 次连接阶段失败进冷却，30s 起按轮翻倍、封顶 10 分钟。
/// 比 key 的 5xx 冷却（60s 起、封顶 2h）短：代理故障多是网络抖动或进程重启，恢复快；
/// 而固定分配的 key 在代理冷却期间是停摆的，冷却拖长就是账号停摆拖长。
const FAILURE_THRESHOLD: i64 = 3;
const COOLDOWN_BASE_SECS: i64 = 30;
const COOLDOWN_MAX_SECS: i64 = 600;
/// 冷却结束后这么久之内再失败，按轮次升级退避；更久之后的失败从 1 重新计数。
const HALF_OPEN_WINDOW_SECS: i64 = 600;

/// 出口绑定。JSON 形态：`{"mode":"inherit"}` / `{"mode":"direct"}` /
/// `{"mode":"proxy","proxy_id":3}` / `{"mode":"group","group_code":"hk"}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Binding {
    /// 跟随全局默认（只对渠道有意义；全局默认本身不能是 inherit）。
    Inherit,
    Direct,
    Proxy {
        proxy_id: i64,
    },
    Group {
        group_code: String,
    },
}

impl Binding {
    /// 落到 `channels.egress_*` 三列的形态（与表上的 CHECK 约束一一对应）。
    fn columns(&self) -> (Option<&'static str>, Option<i64>, Option<&str>) {
        match self {
            Self::Inherit => (None, None, None),
            Self::Direct => (Some("direct"), None, None),
            Self::Proxy { proxy_id } => (Some("proxy"), Some(*proxy_id), None),
            Self::Group { group_code } => (Some("group"), None, Some(group_code.as_str())),
        }
    }

    /// 由三列还原；形状不对（理论上被 CHECK 挡住）按继承处理。
    #[must_use]
    pub fn from_columns(mode: Option<&str>, proxy_id: Option<i64>, group: Option<String>) -> Self {
        match (mode, proxy_id, group) {
            (Some("direct"), _, _) => Self::Direct,
            (Some("proxy"), Some(proxy_id), _) => Self::Proxy { proxy_id },
            (Some("group"), _, Some(group_code)) => Self::Group { group_code },
            _ => Self::Inherit,
        }
    }
}

/// 控制面出口解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Direct,
    Proxy {
        id: i64,
        url: String,
    },
    /// 绑了出口却没有可用代理（停用 / 未分配 / 组里没人）：调用方必须失败，不得直连。
    Unavailable,
}

impl Resolved {
    /// 不可用即错：控制面调用方据此拒绝发请求。
    pub fn proxy_url(self) -> Result<Option<String>, StoreError> {
        match self {
            Self::Direct => Ok(None),
            Self::Proxy { url, .. } => Ok(Some(url)),
            Self::Unavailable => Err(StoreError::Conflict("egress_unavailable")),
        }
    }
}

/// 写入代理时由 URL 解析出的非密字段（console 用 providers 的同一解析器得出）。
#[derive(Debug, Clone, Copy)]
pub struct Endpoint<'a> {
    pub scheme: &'a str,
    pub host: &'a str,
    pub port: i32,
    pub username: Option<&'a str>,
}

/// 新建代理。不实现 Debug：`url` 含认证信息。
pub struct NewProxy<'a> {
    pub name: &'a str,
    pub url: &'a str,
    pub endpoint: Endpoint<'a>,
    pub max_keys: Option<i32>,
    pub max_concurrency: Option<i32>,
    pub note: Option<&'a str>,
    pub status: i16,
    pub owner_id: Option<i64>,
}

/// 代理补丁：外层 None = 不动；`url` 给了就整条换（含认证），并清掉熔断与测试结果——
/// 那些事实属于旧地址。
#[derive(Default)]
// 三态补丁字段（不动 / 置空 / 置值），豁免理由同 console::double_option
#[allow(clippy::option_option)]
pub struct ProxyPatch<'a> {
    pub name: Option<&'a str>,
    pub url: Option<(&'a str, Endpoint<'a>)>,
    pub max_keys: Option<Option<i32>>,
    pub max_concurrency: Option<Option<i32>>,
    pub note: Option<Option<&'a str>>,
    pub status: Option<i16>,
}

/// 代理列表行（不含 URL 密文）。
#[derive(Debug, Clone, Serialize)]
pub struct ProxyRow {
    pub id: i64,
    pub name: String,
    pub owner_id: Option<i64>,
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub username: Option<String>,
    pub status: i16,
    pub max_keys: Option<i32>,
    pub max_concurrency: Option<i32>,
    pub failed_count: i32,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub exit_ip: Option<String>,
    pub exit_country: Option<String>,
    pub latency_ms: Option<i32>,
    pub checked_at: Option<DateTime<Utc>>,
    /// 出口 IP 最近一次变化（测试 / 后台探测发现）：变化前的 IP 与发现时刻。
    pub previous_exit_ip: Option<String>,
    pub exit_ip_changed_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    /// 直接绑定它的渠道数（未删除）。
    pub channel_count: i64,
    /// 固定分配到它的 key 数（未删除渠道）。
    pub assigned_keys: i64,
    /// 它所在的代理组。
    pub groups: Vec<String>,
    /// 是否为全局默认出口。
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 代理组成员（写入形态）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMember {
    pub proxy_id: i64,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_weight")]
    pub weight: i32,
}

fn default_weight() -> i32 {
    1
}

/// 代理组成员（读取形态，带代理状态与组内分配数）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberRow {
    pub proxy_id: i64,
    pub name: String,
    pub status: i16,
    pub cooling: bool,
    pub priority: i32,
    pub weight: i32,
    pub max_keys: Option<i32>,
    /// 本组里分到该代理的 key 数（只对 pinned 组有意义）。
    pub assigned_keys: i64,
}

/// 代理组列表行。
#[derive(Debug, Clone, Serialize)]
pub struct GroupRow {
    pub code: String,
    pub name: String,
    pub mode: String,
    pub owner_id: Option<i64>,
    pub description: Option<String>,
    pub members: Vec<GroupMemberRow>,
    /// 直接绑定该组的渠道数（未删除；不含经全局默认继承的）。
    pub channel_count: i64,
    /// 有效出口是该组、却没分到代理的 key 数（容量满或组里没有可分配成员）。
    pub unassigned_keys: i64,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 新建 / 覆盖代理组。
pub struct GroupInput<'a> {
    pub code: &'a str,
    pub name: &'a str,
    pub mode: &'a str,
    pub description: Option<&'a str>,
    /// 只在新建时落库；覆盖既有组不改属主。
    pub owner_id: Option<i64>,
    pub members: &'a [GroupMember],
}

/// 固定分配明细（组详情 / 手动调整用）。
#[derive(Debug, Clone, Serialize)]
pub struct Assignment {
    pub key_id: i64,
    pub channel_id: i64,
    pub channel_name: String,
    pub proxy_id: Option<i64>,
}

/// 一次对账的结果，回给前端提示（如「3 把 key 因容量已满未分到代理」）。
/// 对账是全量的，三个数都是**全站**口径：这次操作顺带补上 / 释放的其它组的 key 也算在内。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileReport {
    /// 本次新分配的 key 数。
    pub assigned: u64,
    /// 本次释放的分配（绑定不再是 pinned 组 / 代理已不是组员 / 渠道已删）。
    pub released: u64,
    /// 对账后全站仍在排队、分不到代理的 key 数（成员容量全满或组里没有启用的成员）。
    pub unassigned: u64,
}

/// 一次测试的结果。
#[derive(Debug, Clone, Copy)]
pub struct ProbeRecord<'a> {
    pub ok: bool,
    pub exit_ip: Option<&'a str>,
    pub exit_country: Option<&'a str>,
    pub latency_ms: Option<i32>,
    pub error: Option<&'a str>,
}

/// 测试发现的出口 IP 变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExitChange {
    pub previous: String,
    pub current: String,
}

/// 后台探测的对象（启用中的代理）。不实现 Debug：带 URL 密文。
pub struct ProbeTarget {
    pub id: i64,
    pub name: String,
    pub url_ciphertext: Vec<u8>,
    /// 有渠道在用：直接绑定、固定分配、经代理组（含全局默认）可达，或本身是全局默认。
    pub in_use: bool,
    pub channel_count: i64,
    pub assigned_keys: i64,
}

// ---- 代理 ----

pub async fn create_proxy(
    pool: &PgPool,
    input: &NewProxy<'_>,
    master_key: Option<&str>,
) -> Result<i64, StoreError> {
    let sealed = crate::credential::seal_or_plain(master_key, input.url)?;
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO proxies (name, owner_id, url_ciphertext, scheme, host, port, username,
                             status, max_keys, max_concurrency, note)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING id
        "#,
        input.name,
        input.owner_id,
        sealed,
        input.endpoint.scheme,
        input.endpoint.host,
        input.endpoint.port,
        input.endpoint.username,
        input.status,
        input.max_keys,
        input.max_concurrency,
        input.note,
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 批量导入：一个事务里逐条建，返回新 id（与入参同序）。查重由调用方先用 [`endpoint_keys`] 做。
pub async fn create_proxies(
    pool: &PgPool,
    inputs: &[NewProxy<'_>],
    master_key: Option<&str>,
) -> Result<Vec<i64>, StoreError> {
    let mut tx = pool.begin().await?;
    let mut ids = Vec::with_capacity(inputs.len());
    for input in inputs {
        let sealed = crate::credential::seal_or_plain(master_key, input.url)?;
        let id = sqlx::query_scalar!(
            r#"
            INSERT INTO proxies (name, owner_id, url_ciphertext, scheme, host, port, username,
                                 status, max_keys, max_concurrency, note)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id
            "#,
            input.name,
            input.owner_id,
            sealed,
            input.endpoint.scheme,
            input.endpoint.host,
            input.endpoint.port,
            input.endpoint.username,
            input.status,
            input.max_keys,
            input.max_concurrency,
            input.note,
        )
        .fetch_one(&mut *tx)
        .await?;
        ids.push(id);
    }
    tx.commit().await?;
    Ok(ids)
}

/// 已有代理的 `(scheme, host, port, username)`（导入查重用）。`owner` = own 范围只比自己的。
pub async fn endpoint_keys(
    pool: &PgPool,
    owner: Option<i64>,
) -> Result<std::collections::HashSet<(String, String, i32, Option<String>)>, StoreError> {
    let rows = sqlx::query!(
        r#"SELECT scheme, host, port, username FROM proxies
           WHERE ($1::bigint IS NULL OR owner_id = $1)"#,
        owner
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.scheme, r.host, r.port, r.username))
        .collect())
}

/// 改代理并对账（容量放宽 / 重新启用后，排队等分配的 key 当场分到）。None = 不存在。
/// 改 URL 或重新启用会清熔断（新地址 / 运维确认过）。
pub async fn update_proxy(
    pool: &PgPool,
    id: i64,
    patch: &ProxyPatch<'_>,
    master_key: Option<&str>,
) -> Result<Option<ReconcileReport>, StoreError> {
    let sealed = patch
        .url
        .map(|(url, _)| crate::credential::seal_or_plain(master_key, url))
        .transpose()?;
    let endpoint = patch.url.map(|(_, endpoint)| endpoint);
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let result = sqlx::query!(
        r#"
        UPDATE proxies SET
            name           = COALESCE($2, name),
            url_ciphertext = COALESCE($3, url_ciphertext),
            scheme         = COALESCE($4, scheme),
            host           = COALESCE($5, host),
            port           = COALESCE($6, port),
            username       = CASE WHEN $3::bytea IS NULL THEN username ELSE $7 END,
            max_keys       = CASE WHEN $8 THEN $9 ELSE max_keys END,
            note           = CASE WHEN $10 THEN $11 ELSE note END,
            status         = COALESCE($12, status),
            max_concurrency = CASE WHEN $13 THEN $14 ELSE max_concurrency END,
            failed_count   = CASE WHEN $3::bytea IS NOT NULL OR ($12 = 1 AND status <> 1)
                                  THEN 0 ELSE failed_count END,
            cooldown_until = CASE WHEN $3::bytea IS NOT NULL OR ($12 = 1 AND status <> 1)
                                  THEN NULL ELSE cooldown_until END,
            last_error     = CASE WHEN $3::bytea IS NOT NULL THEN NULL ELSE last_error END,
            exit_ip        = CASE WHEN $3::bytea IS NOT NULL THEN NULL ELSE exit_ip END,
            exit_country   = CASE WHEN $3::bytea IS NOT NULL THEN NULL ELSE exit_country END,
            latency_ms     = CASE WHEN $3::bytea IS NOT NULL THEN NULL ELSE latency_ms END,
            checked_at     = CASE WHEN $3::bytea IS NOT NULL THEN NULL ELSE checked_at END,
            previous_exit_ip   = CASE WHEN $3::bytea IS NOT NULL THEN NULL ELSE previous_exit_ip END,
            exit_ip_changed_at = CASE WHEN $3::bytea IS NOT NULL THEN NULL ELSE exit_ip_changed_at END,
            updated_at     = now()
        WHERE id = $1
        "#,
        id,
        patch.name,
        sealed,
        endpoint.map(|e| e.scheme),
        endpoint.map(|e| e.host),
        endpoint.map(|e| e.port),
        endpoint.and_then(|e| e.username),
        patch.max_keys.is_some(),
        patch.max_keys.flatten(),
        patch.note.is_some(),
        patch.note.flatten(),
        patch.status,
        patch.max_concurrency.is_some(),
        patch.max_concurrency.flatten(),
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(Some(report))
}

/// 删代理。被渠道直接绑定或是全局默认 → `Conflict`（没有可自动替代的出口，静默改成直连
/// 等于把真实 IP 暴露给上游）。固定分配到它的 key 在同一事务里改分到组内其他成员。
pub async fn delete_proxy(pool: &PgPool, id: i64) -> Result<bool, StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let refs = sqlx::query!(
        r#"
        SELECT
            (SELECT COUNT(*) FROM channels
              WHERE egress_proxy_id = $1 AND deleted_at IS NULL) AS "channels!",
            EXISTS (SELECT 1 FROM settings
                     WHERE key = 'egress_default' AND value ->> 'mode' = 'proxy'
                       AND value ->> 'proxy_id' = $1::bigint::text) AS "is_default!"
        "#,
        id
    )
    .fetch_one(&mut *tx)
    .await?;
    if refs.channels > 0 {
        return Err(StoreError::Conflict("proxy_in_use"));
    }
    if refs.is_default {
        return Err(StoreError::Conflict("proxy_is_default"));
    }
    // 软删渠道的残留绑定不算占用，但 FK 会挡删除：一并清成「继承」
    sqlx::query!(
        r#"UPDATE channels SET egress_mode = NULL, egress_proxy_id = NULL
           WHERE egress_proxy_id = $1 AND deleted_at IS NOT NULL"#,
        id
    )
    .execute(&mut *tx)
    .await?;
    let deleted = sqlx::query!(r#"DELETE FROM proxies WHERE id = $1"#, id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(deleted)
}

/// 代理属主（外层 None = 代理不存在）。
pub async fn proxy_owner(pool: &PgPool, id: i64) -> Result<Option<Option<i64>>, StoreError> {
    let row = sqlx::query!(r#"SELECT owner_id FROM proxies WHERE id = $1"#, id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.owner_id))
}

/// 代理的完整 URL（含认证，测试用）。None = 不存在。
pub async fn proxy_url(
    pool: &PgPool,
    id: i64,
    master_key: Option<&str>,
) -> Result<Option<String>, StoreError> {
    let row = sqlx::query_scalar!(r#"SELECT url_ciphertext FROM proxies WHERE id = $1"#, id)
        .fetch_optional(pool)
        .await?;
    row.map(|stored| crate::credential::open(master_key, &stored))
        .transpose()
}

/// 代理列表；`owner` = own 范围只看自己的；`query` 匹配名称 / 主机。
pub async fn list_proxies(
    pool: &PgPool,
    owner: Option<i64>,
    query: Option<&str>,
    slice: Slice,
) -> Result<Page<ProxyRow>, StoreError> {
    let pattern = crate::listing::like_pattern(query);
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
            SELECT p.id, p.name, p.owner_id, p.scheme, p.host, p.port, p.username, p.status,
                   p.max_keys, p.max_concurrency, p.failed_count, p.cooldown_until, p.last_error,
                   p.exit_ip, p.exit_country, p.latency_ms, p.checked_at, p.previous_exit_ip,
                   p.exit_ip_changed_at, p.note, p.created_at, p.updated_at,
                   (SELECT COUNT(*) FROM channels c
                     WHERE c.egress_proxy_id = p.id AND c.deleted_at IS NULL) AS "channel_count!",
                   (SELECT COUNT(*) FROM channel_keys k JOIN channels c ON c.id = k.channel_id
                     WHERE k.egress_proxy_id = p.id AND c.deleted_at IS NULL) AS "assigned_keys!",
                   COALESCE((SELECT array_agg(m.group_code ORDER BY m.group_code)
                               FROM proxy_group_members m WHERE m.proxy_id = p.id),
                            ARRAY[]::varchar[]) AS "groups!",
                   EXISTS (SELECT 1 FROM settings
                            WHERE key = 'egress_default' AND value ->> 'mode' = 'proxy'
                              AND value ->> 'proxy_id' = p.id::text) AS "is_default!"
            FROM proxies p
            WHERE ($1::bigint IS NULL OR p.owner_id = $1)
              AND ($2::text IS NULL OR p.name ILIKE $2 OR p.host ILIKE $2)
            ORDER BY p.id
            LIMIT $3 OFFSET $4
            "#,
            owner,
            pattern.as_deref(),
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        count_unless_all(
            slice,
            sqlx::query_scalar!(
                r#"SELECT COUNT(*) AS "c!" FROM proxies p
                   WHERE ($1::bigint IS NULL OR p.owner_id = $1)
                     AND ($2::text IS NULL OR p.name ILIKE $2 OR p.host ILIKE $2)"#,
                owner,
                pattern.as_deref()
            )
            .fetch_one(pool)
        ),
    )?;
    let total = total.unwrap_or_else(|| len_as_total(rows.len()));
    let data = rows
        .into_iter()
        .map(|r| ProxyRow {
            id: r.id,
            name: r.name,
            owner_id: r.owner_id,
            scheme: r.scheme,
            host: r.host,
            port: r.port,
            username: r.username,
            status: r.status,
            max_keys: r.max_keys,
            max_concurrency: r.max_concurrency,
            failed_count: r.failed_count,
            cooldown_until: r.cooldown_until,
            last_error: r.last_error,
            exit_ip: r.exit_ip,
            exit_country: r.exit_country,
            latency_ms: r.latency_ms,
            checked_at: r.checked_at,
            previous_exit_ip: r.previous_exit_ip,
            exit_ip_changed_at: r.exit_ip_changed_at,
            note: r.note,
            channel_count: r.channel_count,
            assigned_keys: r.assigned_keys,
            groups: r.groups,
            is_default: r.is_default,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();
    Ok(Page { data, total })
}

/// 记一次测试（控制台手动 / 后台探测）。出口 IP 与上次不同就记下变化并返回它。
///
/// `manual` 成功即视为人工确认恢复：清熔断。后台探测只更新事实，不碰熔断——探测地址
/// 不是上游，「能到 Cloudflare」证明不了「能到上游」，拿它提前放行会让熔断来回翻。
/// 失败只记录，不触发熔断。探测没解析出 IP（自定义探测地址）时保留上次已知的出口 IP。
pub async fn record_probe(
    pool: &PgPool,
    id: i64,
    probe: ProbeRecord<'_>,
    manual: bool,
) -> Result<Option<ExitChange>, StoreError> {
    let row = sqlx::query!(
        r#"
        WITH prev AS (SELECT id, exit_ip AS old_ip FROM proxies WHERE id = $1 FOR UPDATE)
        UPDATE proxies p SET
            exit_ip        = CASE WHEN $2 THEN COALESCE($3, p.exit_ip) ELSE p.exit_ip END,
            exit_country   = CASE WHEN $2 AND $3 IS NOT NULL THEN $4 ELSE p.exit_country END,
            latency_ms     = CASE WHEN $2 THEN $5 ELSE p.latency_ms END,
            -- 后台探测成功不抹掉熔断中的那条失败原因（手动测试成功会一并清熔断，可以清）
            last_error     = CASE
                                 WHEN $2 AND ($7 OR p.cooldown_until IS NULL OR p.cooldown_until <= now())
                                     THEN NULL
                                 WHEN $2 THEN p.last_error
                                 ELSE $6
                             END,
            previous_exit_ip = CASE WHEN $2 AND $3 IS NOT NULL AND prev.old_ip IS NOT NULL
                                         AND prev.old_ip <> $3
                                    THEN prev.old_ip ELSE p.previous_exit_ip END,
            exit_ip_changed_at = CASE WHEN $2 AND $3 IS NOT NULL AND prev.old_ip IS NOT NULL
                                           AND prev.old_ip <> $3
                                      THEN now() ELSE p.exit_ip_changed_at END,
            failed_count   = CASE WHEN $2 AND $7 THEN 0 ELSE p.failed_count END,
            cooldown_until = CASE WHEN $2 AND $7 THEN NULL ELSE p.cooldown_until END,
            checked_at     = now(),
            updated_at     = now()
        FROM prev
        WHERE p.id = prev.id
        RETURNING prev.old_ip AS "old_ip?", p.exit_ip AS "new_ip?"
        "#,
        id,
        probe.ok,
        probe.exit_ip,
        probe.exit_country,
        probe.latency_ms,
        probe.error.map(|e| e.chars().take(255).collect::<String>()),
        manual,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| match (r.old_ip, r.new_ip) {
        (Some(previous), Some(current)) if probe.ok && previous != current => {
            Some(ExitChange { previous, current })
        }
        _ => None,
    }))
}

/// 后台探测的对象：全部启用中的代理，带「是否有渠道在用」。
pub async fn probe_targets(pool: &PgPool) -> Result<Vec<ProbeTarget>, StoreError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.name, p.url_ciphertext,
               (SELECT COUNT(*) FROM channels c
                 WHERE c.egress_proxy_id = p.id AND c.deleted_at IS NULL) AS "channel_count!",
               (SELECT COUNT(*) FROM channel_keys k JOIN channels c ON c.id = k.channel_id
                 WHERE k.egress_proxy_id = p.id AND c.deleted_at IS NULL) AS "assigned_keys!",
               (EXISTS (SELECT 1 FROM channel_egress e JOIN channels c ON c.id = e.channel_id
                         WHERE c.deleted_at IS NULL
                           AND ((e.mode = 'proxy' AND e.proxy_id = p.id)
                             OR (e.mode = 'group' AND EXISTS (
                                     SELECT 1 FROM proxy_group_members m
                                     WHERE m.group_code = e.group_code AND m.proxy_id = p.id)))))
                   AS "in_use!"
        FROM proxies p
        WHERE p.status = 1
        ORDER BY p.id
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ProbeTarget {
            id: r.id,
            name: r.name,
            url_ciphertext: r.url_ciphertext,
            in_use: r.in_use,
            channel_count: r.channel_count,
            assigned_keys: r.assigned_keys,
        })
        .collect())
}

// ---- 被动熔断 ----

/// 登记一次经该代理的连接阶段失败（TCP / TLS / 代理隧道 / 连接超时）。
/// 只有出口健康的代理计数：冷却中迟到的失败不续冷却，一次故障不会被在途请求放大成长冷却。
pub async fn mark_failure(pool: &PgPool, proxy_id: i64, error: &str) -> Result<(), StoreError> {
    let error: String = error.chars().take(255).collect();
    sqlx::query!(
        r#"
        WITH next AS (
            SELECT id, CASE
                WHEN failed_count >= $3::bigint AND (cooldown_until IS NULL
                     OR cooldown_until < now() - make_interval(secs => $6::bigint::double precision))
                THEN 1 ELSE failed_count + 1 END AS failed
            FROM proxies
            WHERE id = $1 AND (cooldown_until IS NULL OR cooldown_until <= now())
            FOR UPDATE)
        UPDATE proxies p SET failed_count = next.failed, last_error = $2,
            cooldown_until = CASE WHEN next.failed >= $3::bigint
                THEN now() + make_interval(secs => least($5::bigint::double precision,
                     $4::bigint::double precision
                     * power(2, least(20, greatest(0, next.failed - $3::bigint)))))
                ELSE p.cooldown_until END,
            updated_at = now()
        FROM next WHERE p.id = next.id
        "#,
        proxy_id,
        error,
        FAILURE_THRESHOLD,
        COOLDOWN_BASE_SECS,
        COOLDOWN_MAX_SECS,
        HALF_OPEN_WINDOW_SECS,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 经该代理成功拿到上游响应：清零连续失败计数（含半开），只在确有计数时写。
pub async fn clear_failures(pool: &PgPool, proxy_id: i64) -> Result<bool, StoreError> {
    let cleared = sqlx::query!(
        r#"UPDATE proxies SET failed_count = 0, updated_at = now()
           WHERE id = $1 AND failed_count > 0
             AND (cooldown_until IS NULL OR cooldown_until <= now())"#,
        proxy_id
    )
    .execute(pool)
    .await?;
    Ok(cleared.rows_affected() > 0)
}

// ---- 代理组 ----

/// 新建或覆盖代理组（成员整组替换），同事务对账固定分配。
pub async fn upsert_group(
    pool: &PgPool,
    input: &GroupInput<'_>,
) -> Result<ReconcileReport, StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    sqlx::query!(
        r#"
        INSERT INTO proxy_groups (code, name, mode, owner_id, description)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (code) DO UPDATE SET
            name = EXCLUDED.name, mode = EXCLUDED.mode,
            description = EXCLUDED.description, updated_at = now()
        "#,
        input.code,
        input.name,
        input.mode,
        input.owner_id,
        input.description,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"DELETE FROM proxy_group_members WHERE group_code = $1"#,
        input.code
    )
    .execute(&mut *tx)
    .await?;
    let ids: Vec<i64> = input.members.iter().map(|m| m.proxy_id).collect();
    let priorities: Vec<i32> = input.members.iter().map(|m| m.priority).collect();
    let weights: Vec<i32> = input.members.iter().map(|m| m.weight).collect();
    sqlx::query!(
        r#"
        INSERT INTO proxy_group_members (group_code, proxy_id, priority, weight)
        SELECT $1, m.proxy_id, m.priority, m.weight
        FROM unnest($2::bigint[], $3::int[], $4::int[]) AS m(proxy_id, priority, weight)
        "#,
        input.code,
        &ids,
        &priorities,
        &weights,
    )
    .execute(&mut *tx)
    .await?;
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// 往组里追加成员（已在组里的跳过，priority 0 / weight 1），同事务对账。
pub async fn add_group_members(
    pool: &PgPool,
    code: &str,
    proxy_ids: &[i64],
) -> Result<ReconcileReport, StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    sqlx::query!(
        r#"
        INSERT INTO proxy_group_members (group_code, proxy_id)
        SELECT $1, id FROM unnest($2::bigint[]) AS m(id)
        ON CONFLICT (group_code, proxy_id) DO NOTHING
        "#,
        code,
        proxy_ids
    )
    .execute(&mut *tx)
    .await?;
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// 删代理组。被渠道直接绑定或是全局默认 → `Conflict`。
pub async fn delete_group(pool: &PgPool, code: &str) -> Result<bool, StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let refs = sqlx::query!(
        r#"
        SELECT
            (SELECT COUNT(*) FROM channels
              WHERE egress_group_code = $1 AND deleted_at IS NULL) AS "channels!",
            EXISTS (SELECT 1 FROM settings
                     WHERE key = 'egress_default' AND value ->> 'mode' = 'group'
                       AND value ->> 'group_code' = $1) AS "is_default!"
        "#,
        code
    )
    .fetch_one(&mut *tx)
    .await?;
    if refs.channels > 0 {
        return Err(StoreError::Conflict("proxy_group_in_use"));
    }
    if refs.is_default {
        return Err(StoreError::Conflict("proxy_group_is_default"));
    }
    sqlx::query!(
        r#"UPDATE channels SET egress_mode = NULL, egress_group_code = NULL
           WHERE egress_group_code = $1 AND deleted_at IS NOT NULL"#,
        code
    )
    .execute(&mut *tx)
    .await?;
    let deleted = sqlx::query!(r#"DELETE FROM proxy_groups WHERE code = $1"#, code)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(deleted)
}

/// 代理组属主（外层 None = 不存在）。
pub async fn group_owner(pool: &PgPool, code: &str) -> Result<Option<Option<i64>>, StoreError> {
    let row = sqlx::query!(r#"SELECT owner_id FROM proxy_groups WHERE code = $1"#, code)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.owner_id))
}

/// 代理组列表（成员内联；组的数量是配置级规模，不分页也无妨，但仍按 `slice` 切）。
pub async fn list_groups(
    pool: &PgPool,
    owner: Option<i64>,
    slice: Slice,
) -> Result<Page<GroupRow>, StoreError> {
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
            SELECT g.code, g.name, g.mode, g.owner_id, g.description, g.created_at, g.updated_at,
                   COALESCE((
                       SELECT jsonb_agg(jsonb_build_object(
                                  'proxy_id', p.id, 'name', p.name, 'status', p.status,
                                  'cooling', COALESCE(p.cooldown_until > now(), false),
                                  'priority', m.priority, 'weight', m.weight,
                                  'max_keys', p.max_keys,
                                  'assigned_keys', (
                                      SELECT COUNT(*) FROM channel_keys k
                                      JOIN channel_egress e ON e.channel_id = k.channel_id
                                      JOIN channels c ON c.id = k.channel_id
                                      WHERE k.egress_proxy_id = p.id AND e.mode = 'group'
                                        AND e.group_code = g.code AND c.deleted_at IS NULL))
                              ORDER BY m.priority DESC, p.id)
                       FROM proxy_group_members m JOIN proxies p ON p.id = m.proxy_id
                       WHERE m.group_code = g.code),
                       '[]'::jsonb) AS "members!",
                   (SELECT COUNT(*) FROM channels c
                     WHERE c.egress_group_code = g.code AND c.deleted_at IS NULL) AS "channel_count!",
                   (SELECT COUNT(*) FROM channel_keys k
                      JOIN channel_egress e ON e.channel_id = k.channel_id
                      JOIN channels c ON c.id = k.channel_id
                     WHERE g.mode = 'pinned' AND e.mode = 'group' AND e.group_code = g.code
                       AND c.deleted_at IS NULL AND k.egress_proxy_id IS NULL) AS "unassigned_keys!",
                   EXISTS (SELECT 1 FROM settings
                            WHERE key = 'egress_default' AND value ->> 'mode' = 'group'
                              AND value ->> 'group_code' = g.code) AS "is_default!"
            FROM proxy_groups g
            WHERE ($1::bigint IS NULL OR g.owner_id = $1)
            ORDER BY g.code
            LIMIT $2 OFFSET $3
            "#,
            owner,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        count_unless_all(
            slice,
            sqlx::query_scalar!(
                r#"SELECT COUNT(*) AS "c!" FROM proxy_groups g
                   WHERE ($1::bigint IS NULL OR g.owner_id = $1)"#,
                owner
            )
            .fetch_one(pool)
        ),
    )?;
    let total = total.unwrap_or_else(|| len_as_total(rows.len()));
    let data = rows
        .into_iter()
        .map(|r| GroupRow {
            code: r.code,
            name: r.name,
            mode: r.mode,
            owner_id: r.owner_id,
            description: r.description,
            members: serde_json::from_value(r.members).unwrap_or_default(),
            channel_count: r.channel_count,
            unassigned_keys: r.unassigned_keys,
            is_default: r.is_default,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();
    Ok(Page { data, total })
}

/// 有效出口为该组的全部 key 及其分配（含经全局默认继承的），供组详情展示与手动调整。
pub async fn group_assignments(pool: &PgPool, code: &str) -> Result<Vec<Assignment>, StoreError> {
    let rows = sqlx::query_as!(
        Assignment,
        r#"
        SELECT k.id AS key_id, c.id AS channel_id, c.name AS channel_name,
               k.egress_proxy_id AS proxy_id
        FROM channel_keys k
        JOIN channels c ON c.id = k.channel_id
        JOIN channel_egress e ON e.channel_id = c.id
        WHERE c.deleted_at IS NULL AND e.mode = 'group' AND e.group_code = $1
        ORDER BY c.id, k.id
        "#,
        code
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---- 绑定 ----

/// 设置渠道出口绑定并对账。None = 渠道不存在。
/// 绑定目标的存在性与可见性由调用方先校验（FK 只兜底存在性）。
pub async fn set_channel_binding(
    pool: &PgPool,
    channel_id: i64,
    binding: &Binding,
) -> Result<Option<ReconcileReport>, StoreError> {
    let (affected, report) = set_channels_binding(pool, &[channel_id], binding).await?;
    Ok((affected > 0).then_some(report))
}

/// 批量设置渠道出口绑定并对账（一个事务）。返回命中的渠道数。
pub async fn set_channels_binding(
    pool: &PgPool,
    channel_ids: &[i64],
    binding: &Binding,
) -> Result<(u64, ReconcileReport), StoreError> {
    let (mode, proxy_id, group_code) = binding.columns();
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let affected = sqlx::query!(
        r#"
        UPDATE channels
        SET egress_mode = $2, egress_proxy_id = $3, egress_group_code = $4, updated_at = now()
        WHERE id = ANY($1) AND deleted_at IS NULL
        "#,
        channel_ids,
        mode,
        proxy_id,
        group_code,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok((affected, report))
}

/// 在建渠道的事务里落出口绑定（固定分配可带上换码前已选定的代理，见 [`pick_for_new_key`]）。
pub async fn bind_new_channel(
    conn: &mut PgConnection,
    channel_id: i64,
    key_id: i64,
    binding: &Binding,
    preassigned: Option<i64>,
) -> Result<ReconcileReport, StoreError> {
    let (mode, proxy_id, group_code) = binding.columns();
    lock_assignments(conn).await?;
    sqlx::query!(
        r#"UPDATE channels SET egress_mode = $2, egress_proxy_id = $3, egress_group_code = $4
           WHERE id = $1"#,
        channel_id,
        mode,
        proxy_id,
        group_code,
    )
    .execute(&mut *conn)
    .await?;
    if let Some(proxy) = preassigned {
        sqlx::query!(
            r#"UPDATE channel_keys SET egress_proxy_id = $2 WHERE id = $1"#,
            key_id,
            proxy
        )
        .execute(&mut *conn)
        .await?;
    }
    reconcile_locked(conn).await
}

/// 全局默认出口（未配置 / 形状不对 = 直连）。
pub async fn default_binding(pool: &PgPool) -> Result<Binding, StoreError> {
    let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'egress_default'"#)
        .fetch_optional(pool)
        .await?;
    Ok(value
        .and_then(|v| serde_json::from_value::<Binding>(v).ok())
        .filter(|b| *b != Binding::Inherit)
        .unwrap_or(Binding::Direct))
}

/// 设置全局默认出口并对账（继承默认的渠道随之换出口）。`Inherit` 不是合法的默认值。
pub async fn set_default_binding(
    pool: &PgPool,
    binding: &Binding,
    updated_by: i64,
) -> Result<ReconcileReport, StoreError> {
    if *binding == Binding::Inherit {
        return Err(StoreError::InvalidData("egress_default_inherit"));
    }
    let value =
        serde_json::to_value(binding).map_err(|_| StoreError::InvalidData("egress_default"))?;
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    // settings 没有 FK：目标是否存在在这里、在锁内判，避免与并发删除交错出悬空默认值
    let exists = match binding {
        Binding::Inherit | Binding::Direct => true,
        Binding::Proxy { proxy_id } => {
            sqlx::query_scalar!(
                r#"SELECT EXISTS (SELECT 1 FROM proxies WHERE id = $1) AS "e!""#,
                proxy_id
            )
            .fetch_one(&mut *tx)
            .await?
        }
        Binding::Group { group_code } => {
            sqlx::query_scalar!(
                r#"SELECT EXISTS (SELECT 1 FROM proxy_groups WHERE code = $1) AS "e!""#,
                group_code
            )
            .fetch_one(&mut *tx)
            .await?
        }
    };
    if !exists {
        return Err(StoreError::Conflict("egress_target_missing"));
    }
    sqlx::query!(
        r#"
        INSERT INTO settings (key, value, updated_by) VALUES ('egress_default', $1, $2)
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
            updated_by = EXCLUDED.updated_by, updated_at = now()
        "#,
        value,
        updated_by
    )
    .execute(&mut *tx)
    .await?;
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// 手动把一把 key 改分到组内另一个代理。key 的有效出口必须是 pinned 组且目标是组员；
/// 目标若已满（不计这把 key 自己）→ `Conflict("proxy_full")`。
pub async fn assign_key(pool: &PgPool, key_id: i64, proxy_id: i64) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let row = sqlx::query!(
        r#"
        SELECT e.mode AS "mode!", g.mode AS "group_mode?",
               EXISTS (SELECT 1 FROM proxy_group_members m
                        WHERE m.group_code = e.group_code AND m.proxy_id = $2) AS "member!",
               (SELECT p.max_keys FROM proxies p WHERE p.id = $2) AS max_keys,
               (SELECT COUNT(*) FROM channel_keys o JOIN channels oc ON oc.id = o.channel_id
                 WHERE o.egress_proxy_id = $2 AND o.id <> $1 AND oc.deleted_at IS NULL) AS "load!"
        FROM channel_keys k
        JOIN channels c ON c.id = k.channel_id AND c.deleted_at IS NULL
        JOIN channel_egress e ON e.channel_id = c.id
        LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
        WHERE k.id = $1
        "#,
        key_id,
        proxy_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(StoreError::Conflict("key_not_found"))?;
    if row.mode != "group" || row.group_mode.as_deref() != Some("pinned") {
        return Err(StoreError::Conflict("egress_not_pinned"));
    }
    if !row.member {
        return Err(StoreError::Conflict("proxy_not_in_group"));
    }
    if row.max_keys.is_some_and(|cap| row.load >= i64::from(cap)) {
        return Err(StoreError::Conflict("proxy_full"));
    }
    sqlx::query!(
        r#"UPDATE channel_keys SET egress_proxy_id = $2, updated_at = now() WHERE id = $1"#,
        key_id,
        proxy_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---- 固定分配对账 ----

async fn lock_assignments(conn: &mut PgConnection) -> Result<(), StoreError> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", ASSIGN_LOCK)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// 全量对账（自带锁；须在事务内调用，锁随事务释放）。
pub async fn reconcile(conn: &mut PgConnection) -> Result<ReconcileReport, StoreError> {
    lock_assignments(conn).await?;
    reconcile_locked(conn).await
}

/// 组内成员候选（分配用）。
struct Slot {
    proxy_id: i64,
    usable: bool,
    healthy: bool,
    priority: i32,
    capacity: Option<i64>,
}

async fn reconcile_locked(conn: &mut PgConnection) -> Result<ReconcileReport, StoreError> {
    // 1. 释放失效分配：渠道已删 / 有效出口不再是 pinned 组 / 分到的代理已不是组员
    let released = sqlx::query!(
        r#"
        UPDATE channel_keys k SET egress_proxy_id = NULL
        FROM channels c
        JOIN channel_egress e ON e.channel_id = c.id
        LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
        WHERE k.channel_id = c.id AND k.egress_proxy_id IS NOT NULL
          AND (c.deleted_at IS NOT NULL
               OR g.mode IS DISTINCT FROM 'pinned'
               OR NOT EXISTS (SELECT 1 FROM proxy_group_members m
                               WHERE m.group_code = g.code AND m.proxy_id = k.egress_proxy_id))
        "#
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();

    // 2. 待分配的 key（有效出口是 pinned 组、尚无分配），按 id 先来先分
    let pending: Vec<(i64, String)> = sqlx::query!(
        r#"
        SELECT k.id, e.group_code AS "group_code!"
        FROM channel_keys k
        JOIN channels c ON c.id = k.channel_id
        JOIN channel_egress e ON e.channel_id = c.id
        JOIN proxy_groups g ON g.code = e.group_code
        WHERE c.deleted_at IS NULL AND e.mode = 'group' AND g.mode = 'pinned'
          AND k.egress_proxy_id IS NULL
        ORDER BY k.id
        "#
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.group_code))
    .collect();
    if pending.is_empty() {
        return Ok(ReconcileReport {
            released,
            ..ReconcileReport::default()
        });
    }

    // 3. 逐把分配，一次写回
    let (slots, mut load) = allocation_state(conn, &pending).await?;
    let mut keys = Vec::new();
    let mut proxies = Vec::new();
    let mut unassigned = 0u64;
    for (key_id, group_code) in &pending {
        match choose(slots.get(group_code).map(Vec::as_slice), &load) {
            Some(proxy_id) => {
                *load.entry(proxy_id).or_default() += 1;
                keys.push(*key_id);
                proxies.push(proxy_id);
            }
            None => unassigned += 1,
        }
    }
    if !keys.is_empty() {
        sqlx::query!(
            r#"
            UPDATE channel_keys k SET egress_proxy_id = v.proxy_id
            FROM unnest($1::bigint[], $2::bigint[]) AS v(key_id, proxy_id)
            WHERE k.id = v.key_id
            "#,
            &keys,
            &proxies
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(ReconcileReport {
        assigned: u64::try_from(keys.len()).unwrap_or(u64::MAX),
        released,
        unassigned,
    })
}

/// 涉及组的成员表与每个代理的全局已分配数（`max_keys` 跨组共享）。
async fn allocation_state(
    conn: &mut PgConnection,
    pending: &[(i64, String)],
) -> Result<(HashMap<String, Vec<Slot>>, HashMap<i64, i64>), StoreError> {
    let mut groups: Vec<String> = pending.iter().map(|(_, g)| g.clone()).collect();
    groups.sort_unstable();
    groups.dedup();
    let members = sqlx::query!(
        r#"
        SELECT m.group_code, p.id, p.status, m.priority, p.max_keys,
               COALESCE(p.cooldown_until > now(), false) AS "cooling!"
        FROM proxy_group_members m JOIN proxies p ON p.id = m.proxy_id
        WHERE m.group_code = ANY($1)
        ORDER BY m.group_code, p.id
        "#,
        &groups
    )
    .fetch_all(&mut *conn)
    .await?;
    let loads = sqlx::query!(
        r#"
        SELECT k.egress_proxy_id AS "proxy_id!", COUNT(*) AS "n!"
        FROM channel_keys k JOIN channels c ON c.id = k.channel_id
        WHERE k.egress_proxy_id IS NOT NULL AND c.deleted_at IS NULL
        GROUP BY k.egress_proxy_id
        "#
    )
    .fetch_all(&mut *conn)
    .await?;
    let load: HashMap<i64, i64> = loads.into_iter().map(|r| (r.proxy_id, r.n)).collect();
    let mut slots: HashMap<String, Vec<Slot>> = HashMap::new();
    for m in members {
        slots.entry(m.group_code).or_default().push(Slot {
            proxy_id: m.id,
            usable: m.status == 1,
            healthy: m.status == 1 && !m.cooling,
            priority: m.priority,
            capacity: m.max_keys.map(i64::from),
        });
    }
    Ok((slots, load))
}

/// 启用且未满的成员里：健康优先 → 已分配最少 → priority 高 → id 小。
/// 熔断中的成员仍可分（它会恢复，固定分配本就是等恢复的语义）；停用的不分。
fn choose(candidates: Option<&[Slot]>, load: &HashMap<i64, i64>) -> Option<i64> {
    let used = |s: &Slot| load.get(&s.proxy_id).copied().unwrap_or(0);
    candidates?
        .iter()
        .filter(|s| s.usable && s.capacity.is_none_or(|cap| used(s) < cap))
        .min_by_key(|s| {
            (
                !s.healthy,
                used(s),
                std::cmp::Reverse(s.priority),
                s.proxy_id,
            )
        })
        .map(|s| s.proxy_id)
}

/// OAuth 换码前给「即将新建的 key」选定出口：换码请求就得从它日后的 IP 出去。
/// 只读（不占位）：建渠道时以 `bind_new_channel(.., preassigned)` 落库，并发下最多
/// 让某个代理超出 `max_keys` 一把——对账从不驱逐已有分配，宁可超一点也不在换码后改 IP。
pub async fn pick_for_new_key(
    pool: &PgPool,
    binding: &Binding,
    master_key: Option<&str>,
) -> Result<(Resolved, Option<i64>), StoreError> {
    let effective = match binding {
        Binding::Inherit => default_binding(pool).await?,
        other => other.clone(),
    };
    match effective {
        Binding::Inherit | Binding::Direct => Ok((Resolved::Direct, None)),
        Binding::Proxy { proxy_id } => {
            let row = sqlx::query!(
                r#"SELECT url_ciphertext FROM proxies WHERE id = $1 AND status = 1"#,
                proxy_id
            )
            .fetch_optional(pool)
            .await?;
            Ok(match row {
                Some(r) => (
                    Resolved::Proxy {
                        id: proxy_id,
                        url: crate::credential::open(master_key, &r.url_ciphertext)?,
                    },
                    None,
                ),
                None => (Resolved::Unavailable, None),
            })
        }
        Binding::Group { group_code } => {
            let row = sqlx::query!(
                r#"
                SELECT g.mode, p.id, p.url_ciphertext
                FROM proxy_groups g
                JOIN proxy_group_members m ON m.group_code = g.code
                JOIN proxies p ON p.id = m.proxy_id AND p.status = 1
                WHERE g.code = $1
                  AND (g.mode = 'rotate' OR p.max_keys IS NULL OR p.max_keys > (
                       SELECT COUNT(*) FROM channel_keys k JOIN channels c ON c.id = k.channel_id
                       WHERE k.egress_proxy_id = p.id AND c.deleted_at IS NULL))
                ORDER BY COALESCE(p.cooldown_until > now(), false),
                         CASE WHEN g.mode = 'pinned' THEN (
                             SELECT COUNT(*) FROM channel_keys k JOIN channels c ON c.id = k.channel_id
                             WHERE k.egress_proxy_id = p.id AND c.deleted_at IS NULL) ELSE 0 END,
                         m.priority DESC,
                         CASE WHEN g.mode = 'rotate'
                              THEN -ln(1 - random()) / GREATEST(m.weight, 1) ELSE 0 END,
                         p.id
                LIMIT 1
                "#,
                group_code
            )
            .fetch_optional(pool)
            .await?;
            Ok(match row {
                Some(r) => {
                    let url = crate::credential::open(master_key, &r.url_ciphertext)?;
                    let preassigned = (r.mode == "pinned").then_some(r.id);
                    (Resolved::Proxy { id: r.id, url }, preassigned)
                }
                None => (Resolved::Unavailable, None),
            })
        }
    }
}

// ---- 控制面解析 ----

/// 一把 key 此刻的出口（控制面：刷新 token、配额轮询、测活、余额、视频回源）。
/// 固定出口熔断中照样返回它（宁可失败也不换 IP）；轮换组健康成员优先。None = key 不存在。
pub async fn resolve_for_key(
    pool: &PgPool,
    key_id: i64,
    master_key: Option<&str>,
) -> Result<Option<Resolved>, StoreError> {
    let row = sqlx::query!(
        r#"
        SELECT ep.mode AS "mode!", ep.proxy_id, ep.url_ciphertext
        FROM channel_keys k
        CROSS JOIN LATERAL egress_pick(k.channel_id, k.id, false) ep
        WHERE k.id = $1
        "#,
        key_id
    )
    .fetch_optional(pool)
    .await?;
    row.map(|r| resolved(master_key, &r.mode, r.proxy_id, r.url_ciphertext.as_deref()))
        .transpose()
}

/// 渠道级操作（拉模型、余额、测活）按渠道的第一把 key 解析出口。None = 渠道或 key 不存在。
pub async fn resolve_for_channel(
    pool: &PgPool,
    channel_id: i64,
    master_key: Option<&str>,
) -> Result<Option<Resolved>, StoreError> {
    let key = sqlx::query_scalar!(
        r#"SELECT id FROM channel_keys WHERE channel_id = $1 ORDER BY id LIMIT 1"#,
        channel_id
    )
    .fetch_optional(pool)
    .await?;
    match key {
        Some(key) => resolve_for_key(pool, key, master_key).await,
        None => Ok(None),
    }
}

/// `egress_pick` 一行 → 解析结果。
pub(crate) fn resolved(
    master_key: Option<&str>,
    mode: &str,
    proxy_id: Option<i64>,
    url_ciphertext: Option<&[u8]>,
) -> Result<Resolved, StoreError> {
    match (proxy_id, url_ciphertext) {
        (Some(id), Some(stored)) => Ok(Resolved::Proxy {
            id,
            url: crate::credential::open(master_key, stored)?,
        }),
        _ if mode == "direct" => Ok(Resolved::Direct),
        _ => Ok(Resolved::Unavailable),
    }
}

/// `okapi seal-credentials` 的代理部分：把迁移来的明文代理 URL 封成信封。幂等。
pub async fn seal_existing(
    pool: &PgPool,
    master_key_hex: &str,
) -> Result<crate::credential::SealStats, StoreError> {
    let rows = sqlx::query!(r#"SELECT id, url_ciphertext FROM proxies ORDER BY id"#)
        .fetch_all(pool)
        .await?;
    let mut stats = crate::credential::SealStats::default();
    for row in rows {
        if crate::credential::is_sealed(&row.url_ciphertext) {
            stats.already_sealed += 1;
            continue;
        }
        let Ok(plain) = std::str::from_utf8(&row.url_ciphertext) else {
            stats.unreadable.push(row.id);
            continue;
        };
        let sealed = crate::credential::seal(master_key_hex, plain)?;
        stats.sealed += sqlx::query!(
            r#"UPDATE proxies SET url_ciphertext = $2, updated_at = now()
               WHERE id = $1 AND url_ciphertext = $3"#,
            row.id,
            sealed,
            row.url_ciphertext
        )
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binding_json_shape_round_trips_and_maps_to_columns() {
        for (value, columns) in [
            (json!({"mode":"inherit"}), (None, None, None)),
            (json!({"mode":"direct"}), (Some("direct"), None, None)),
            (
                json!({"mode":"proxy","proxy_id":7}),
                (Some("proxy"), Some(7), None),
            ),
            (
                json!({"mode":"group","group_code":"hk"}),
                (Some("group"), None, Some("hk")),
            ),
        ] {
            let binding: Binding = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&binding).unwrap(), value);
            assert_eq!(binding.columns(), columns);
            let (mode, proxy, group) = columns;
            assert_eq!(
                Binding::from_columns(mode, proxy, group.map(str::to_owned)),
                binding
            );
        }
        // 缺目标的形状不能被当成合法绑定
        assert!(serde_json::from_value::<Binding>(json!({"mode":"proxy"})).is_err());
        assert!(serde_json::from_value::<Binding>(json!({"mode":"vpn"})).is_err());
    }

    #[test]
    fn allocation_prefers_healthy_then_least_loaded_and_respects_capacity() {
        let slot = |proxy_id, usable, healthy, priority, capacity| Slot {
            proxy_id,
            usable,
            healthy,
            priority,
            capacity,
        };
        let slots = [
            slot(1, true, true, 0, Some(1)),
            slot(2, true, true, 0, None),
            slot(3, true, false, 9, None),
            slot(4, false, false, 9, None),
        ];
        let mut load = HashMap::new();
        // 都空：健康里 priority 相同取 id 小的
        assert_eq!(choose(Some(&slots), &load), Some(1));
        // 1 满了（容量 1）→ 2；不选熔断中的 3、停用的 4
        load.insert(1, 1);
        assert_eq!(choose(Some(&slots), &load), Some(2));
        // 健康成员都满了才落到熔断中的（等它恢复），停用的永远不分
        let busy = [
            slot(1, true, true, 0, Some(1)),
            slot(3, true, false, 0, None),
            slot(4, false, false, 0, None),
        ];
        assert_eq!(choose(Some(&busy), &load), Some(3));
        let full = [
            slot(1, true, true, 0, Some(1)),
            slot(4, false, true, 0, None),
        ];
        assert_eq!(choose(Some(&full), &load), None);
        assert_eq!(choose(None, &load), None);
        // 负载均衡：已分配少的优先于 priority
        let mut balanced = HashMap::new();
        balanced.insert(5, 3);
        let pair = [slot(5, true, true, 9, None), slot(6, true, true, 0, None)];
        assert_eq!(choose(Some(&pair), &balanced), Some(6));
    }

    #[test]
    fn unavailable_egress_is_an_error_never_direct() {
        assert_eq!(Resolved::Direct.proxy_url().unwrap(), None);
        assert_eq!(
            Resolved::Proxy {
                id: 1,
                url: "http://p:1".into()
            }
            .proxy_url()
            .unwrap()
            .as_deref(),
            Some("http://p:1")
        );
        assert!(Resolved::Unavailable.proxy_url().is_err());
        assert_eq!(
            resolved(None, "direct", None, None).unwrap(),
            Resolved::Direct
        );
        assert_eq!(
            resolved(None, "group", None, None).unwrap(),
            Resolved::Unavailable
        );
        assert_eq!(
            resolved(None, "proxy", None, None).unwrap(),
            Resolved::Unavailable
        );
    }
}
