//! 用户门户 API（M2 第一批）：余额 / 用量 / key 分账。
//!
//! 合作商轻量模式（IMPLEMENTATION §6.1 的 key 即子账户）：给每位员工发独立 key，
//! `/api/me/usage` 默认 `scope=key`（员工只看自己这把 key 的用量），
//! `scope=user` 为钱包主体汇总视图。完整 Team 层（独立登录/成员限额）在 M4。
//! 统计查询走 ClickHouse MV；未启用 CH 时 fail-closed 返回 501 stats_disabled。
//!
//! 本文件放 `me`、公告与日志的共用参数；用量 / 流水 / 订单在 billing，兑换在 redeem，
//! 分组自选与返利在 groups，门户 key 在 keys（日志、价目、key 趋势是此前就有的子模块）。

mod key_trends;
mod pricing;
mod usage_logs;
pub use pricing::{public_groups, public_models, public_pricing, public_statistics};
pub use usage_logs::{list as logs, series as logs_series, stat as logs_stat};
mod billing;
mod groups;
mod keys;
mod redeem;

pub use self::billing::{LedgerQuery, UsageQuery, ledger, orders, usage};
pub(super) use self::groups::ensure_selectable;
pub use self::groups::{aff, groups};
pub use self::keys::{PatchKeyReq, delete_key, keys, patch_key};
pub(super) use self::keys::{normalize_allowlist, normalize_ip_allowlist, validate_key_limits};
pub use self::redeem::{RedeemReq, redeem};

use crate::gateway::state::AppState;
use crate::kernel::auth::authenticate;
use crate::kernel::error::AppError;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};

fn ch_or_disabled(state: &AppState) -> Result<&ChClient, AppError> {
    state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))
}

fn ch_i64(row: &Value, key: &str) -> i64 {
    row.get(key).map_or(0, |v| {
        v.as_str()
            .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
            .unwrap_or(0)
    })
}

/// GET /api/me：身份、余额与**生效权限点**（热余额为准，快照列对账用）。
///
/// 权限点给前端用来决定"哪些入口该出现"，而不是让用户点进去再吃 403——
/// 一个只读运维角色看到十个改配置的按钮，每个都点不动，是很糟的体验。
/// 语义与后端 `AuthedKey::has_permission` 一致：`["*"]` 表示全权。
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let balance = state.ledger.balance(key.user_id).await?;
    let has_web_session = super::auth_web::require_session(&state, &headers)
        .await
        .is_ok_and(|user_id| user_id == key.actor_user_id());
    let key_info: Option<(String, String, String)> =
        sqlx::query_as("SELECT k.name, k.key_prefix, u.username FROM api_keys k JOIN users u ON u.id = COALESCE(k.member_user_id,k.user_id) WHERE k.id = $1 AND k.user_id = $2")
            .bind(key.key_id)
            .bind(key.user_id)
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    // 订阅池（§11.28）：热值一次 HMGET；窗口外 / 越界为负对用户都显示 0
    let (sub, sub_until) = state.ledger.sub_balance(key.user_id).await?;
    let sub_active = sub_until > chrono::Utc::now().timestamp();
    // 余额有效期（#1790-6）是本站独有的机制：钱会在某一天被清零，用户必须能在
    // 首页看到那一天——不在鉴权缓存里（低频字段），点查 PG 一次。
    let balance_expires_at = sqlx::query_scalar!(
        r#"SELECT balance_expires_at FROM users WHERE id = $1"#,
        key.user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .flatten();
    // super_admin 与"未绑定自定义角色的 admin"都是全权（对齐 new-api 迁移习惯）
    let permissions: Vec<String> = if key.role >= 100 {
        vec!["*".to_owned()]
    } else if key.role >= 10 {
        key.permissions
            .clone()
            .unwrap_or_else(|| vec!["*".to_owned()])
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "user_id": key.actor_user_id(),
        "wallet_user_id": key.user_id,
        "username": key_info.as_ref().map(|info| &info.2),
        "key_id": key.key_id,
        "key_name": key_info.as_ref().map(|info| &info.0),
        "key_prefix": key_info.as_ref().map(|info| &info.1),
        "has_web_session": has_web_session,
        "group": key.group_code,
        "balance_micro": balance.as_micros(),
        "balance_expires_at": balance_expires_at.map(|t| t.to_rfc3339()),
        // 订阅池剩余（窗口内才有；无订阅 / 窗口外 = 0）与池可用截止 unix 秒（0 = 无）
        "subscription_remaining_micro": if sub_active { sub.as_micros().max(0) } else { 0 },
        "subscription_until_unix": if sub_active { sub_until } else { 0 },
        "role": key.role,
        "permissions": permissions,
    })))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// 游标：上一页最后一行的 id；按 (created_at, id) 倒序取它之后的记录。
    #[serde(default)]
    pub before: Option<i64>,
    /// `key`（缺省）| `user`：与 /api/me/usage 同一语义——合作商员工缺省只见自己那把 key。
    #[serde(default)]
    pub scope: Option<String>,
    /// 精确模型名过滤。
    #[serde(default)]
    pub model: Option<String>,
    /// 只看失败（status = 40，不包含已退款记录）。
    #[serde(default)]
    pub errors_only: Option<bool>,
    pub api_key_id: Option<i64>,
    pub request_id: Option<uuid::Uuid>,
    /// 含首尾的日历日期，与门户看板下钻携带的统计时区配套。
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub timezone: Option<String>,
}

/// 门户日志列表与汇总的缺省回看天数（含今天）。
const DEFAULT_LOG_DAYS: u32 = 30;

struct LogWindow {
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
    timezone: String,
}

impl LogsQuery {
    /// 没给日期时缺省近 [`DEFAULT_LOG_DAYS`] 天：账单表按 created_at 分区，无时间界的列表 / 汇总
    /// 要扫用户全部历史分区，成本随终身账单行数线性涨（保留期缺省永久）。按 request_id 点查不加界。
    async fn window(&self, pg: &sqlx::PgPool) -> Result<Option<LogWindow>, AppError> {
        if self.start_date.is_none() && self.end_date.is_none() && self.request_id.is_some() {
            return Ok(None);
        }
        let timezone = self.timezone.as_deref().unwrap_or("UTC");
        if timezone.len() > 128 {
            return Err(AppError::bad_request().with_param("timezone"));
        }
        // PG 校验 IANA 时区并提供当地今天；不依赖 ClickHouse，也不使用浏览器时钟。
        let today = sqlx::query_scalar::<_, chrono::NaiveDate>(
            "SELECT (CURRENT_TIMESTAMP AT TIME ZONE name)::date FROM pg_timezone_names WHERE name = $1",
        )
        .bind(timezone)
        .fetch_optional(pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::bad_request().with_param("timezone"))?;
        let (start, end) = super::usage_details::CalendarWindow::bounds(
            today,
            DEFAULT_LOG_DAYS,
            self.start_date.as_deref(),
            self.end_date.as_deref(),
        )?;
        Ok(Some(LogWindow {
            start,
            end,
            timezone: timezone.to_owned(),
        }))
    }
}

fn default_limit() -> i64 {
    50
}

/// GET /api/notice：站点公告（无鉴权，登录页也要显示）。
///
/// 存 `settings.site_notice`（吸收判据②：能用现有表表达就不新增表），经 60s 进程缓存
/// 读取——发布后一分钟内全站可见，足够。对外只透出四个白名单字段并做类型收口：
/// settings 的写入口是泛型 key/value，不能假设值形状；`level` 收敛到三档枚举，
/// 正文截断到 4000 字——公告是横幅，不是文章。
pub async fn notice(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let raw = state.setting_cached("site_notice").await;
    let Some(v) = raw.as_ref() else {
        return Ok(Json(json!({ "notice": Value::Null })));
    };
    let enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let body = v
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !enabled || body.is_empty() {
        return Ok(Json(json!({ "notice": Value::Null })));
    }
    let level = match v.get("level").and_then(Value::as_str) {
        Some("warning") => "warning",
        Some("critical") => "critical",
        _ => "info",
    };
    let clipped: String = body.chars().take(4000).collect();
    Ok(Json(json!({
        "notice": {
            "title": v.get("title").and_then(Value::as_str).unwrap_or_default().trim(),
            "body": clipped,
            "level": level,
            // 前端用它做"已读"锚点：重新发布（updated_at 变）会再次弹出
            "updated_at": v.get("updated_at").and_then(Value::as_str).unwrap_or_default(),
        }
    })))
}
