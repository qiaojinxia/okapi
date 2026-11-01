//! 运维：缓存清理与对账（查看 / 修复）。
use super::super::query::Query;
use super::{audit, guard};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use serde::Deserialize;
use serde_json::{Value, json};

// ---- 缓存清理（#1790-7）----

#[derive(Deserialize)]
pub struct FlushReq {
    /// auth | pricebook。
    pub scope: String,
}

/// 缓存清理。auth 走 Redis 版本键（跨进程立即生效）；routing/pricebook 为进程内
/// （单机 all 形态全局生效；多副本靠 TTL 收敛，NATS 广播 M2 relay 批接入）。
pub async fn cache_flush(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<FlushReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::CACHE_FLUSH).await?;
    match req.scope.as_str() {
        "auth" => state.sched.auth_flush().await,
        "routing" => state.invalidate_routing_caches(),
        "pricebook" => {
            let book = crate::kernel::pricing_loader::load_pricebook(&state.pg)
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "pricebook 重载失败");
                    AppError::internal()
                })?;
            state.pricebook.replace(book);
        }
        _ => return Err(AppError::bad_request().with_param("scope")),
    }
    audit(&state, &actor, "cache.flush", &req.scope, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 对账 ----

#[derive(Deserialize)]
pub struct ReconQuery {
    /// Users per page, not a cap on the complete reconciliation scan.
    #[serde(default = "default_limit")]
    pub limit: i64,
}

pub(super) fn default_limit() -> i64 {
    1000
}

#[derive(Deserialize)]
pub struct RepairReq {
    /// 只修这一个用户。与 `all` 二选一。
    #[serde(default)]
    pub user_id: Option<i64>,
    /// 修当前扫出的全部漂移用户。
    #[serde(default)]
    pub all: bool,
    /// `all` 时的每页用户数；与对账视图同样完整扫描。
    #[serde(default = "default_recon_limit")]
    pub limit: i64,
}

pub(super) const fn default_recon_limit() -> i64 {
    1000
}

/// POST /admin/reconciliation/repair：按账本重建热余额与展示快照。
///
/// 为什么需要：Redis 是唯一热账本且 `reserve.lua` 对缺键按余额 0 处理、不回源 PG，
/// 所以「Redis 活着但数据没了」（没开持久化重启 / 切到空副本 / maxmemory 淘汰 /
/// 手滑 FLUSHDB）会让全站付费请求静默拒服务，且**不会自愈**——对账任务此前只报不修，
/// 全仓也没有第二个入口能把余额写回去，站长只能眼看着差额挂在页面上。
///
/// 权威源是 `billing_events` 求和，操作幂等；在途预扣保持不动（`avail + 在途 == 账本`）。
pub async fn repair_reconciliation(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<RepairReq>,
) -> Result<Json<Value>, AppError> {
    // 它会改余额，权限跟充值/扣减同一道闸——虽然改的方向是"改回账本说的数"
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    let repaired = match (req.user_id, req.all) {
        (Some(user_id), false) => {
            // 先确认漂移是稳定的：结算是异步的，「Redis 已扣、PG 事件还没落」的中间态
            // 看起来和真丢数据一模一样，照着修等于把正在结算的那笔退回去
            match crate::kernel::repair::stable_drift(&state.pg, &state.ledger, user_id).await {
                Ok(None) => {
                    let exists = sqlx::query_scalar!(
                        r#"SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)
                           AS "e!""#,
                        user_id
                    )
                    .fetch_one(&state.pg)
                    .await
                    .map_err(okapi_store::StoreError::from)?;
                    return Err(if exists {
                        // 账目正在变动 → 让管理员过几秒重试，而不是拿一个半截的账本去覆写
                        AppError::new(StatusCode::CONFLICT, codes::RECONCILE_UNSTABLE)
                    } else {
                        AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
                    });
                }
                Err(err) => {
                    tracing::error!(error = %err, user_id, "对账稳定性判定失败");
                    return Err(AppError::internal());
                }
                Ok(Some(_)) => {}
            }
            match crate::kernel::repair::repair_balance(&state.pg, &state.ledger, user_id).await {
                Ok(Some(one)) => vec![one],
                Ok(None) => return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)),
                Err(err) => {
                    tracing::error!(error = %err, user_id, "对账修复失败");
                    return Err(AppError::internal());
                }
            }
        }
        (None, true) => crate::kernel::repair::repair_drifted(&state.pg, &state.ledger, req.limit)
            .await
            .map_err(|err| {
                tracing::error!(error = %err, "批量对账修复失败");
                AppError::internal()
            })?,
        // 既没指定用户又没给 all：批量改余额不该是手滑的默认行为
        _ => return Err(AppError::bad_request().with_param("user_id_or_all")),
    };
    audit(
        &state,
        &actor,
        "billing.reconcile_repair",
        &req.user_id
            .map_or_else(|| "all".to_owned(), |id| id.to_string()),
        json!({ "repaired": repaired.len(), "detail": repaired }),
    )
    .await;
    Ok(Json(
        json!({ "repaired": repaired.len(), "data": repaired }),
    ))
}

pub async fn reconciliation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReconQuery>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::BILLING_READ).await?;
    let drifts = crate::kernel::reconcile::reconcile_balances(&state.pg, &state.ledger, q.limit)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "对账查询失败");
            AppError::internal()
        })?;
    // 带上用户名：漂移行只有 user_id 时，管理员还得去用户页反查一趟
    let ids: Vec<i64> = drifts.iter().map(|d| d.user_id).collect();
    let names: std::collections::HashMap<i64, String> =
        sqlx::query!(r#"SELECT id, username FROM users WHERE id = ANY($1)"#, &ids)
            .fetch_all(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .into_iter()
            .map(|r| (r.id, r.username))
            .collect();
    let data: Vec<Value> = drifts
        .iter()
        .map(|d| {
            json!({
                "user_id": d.user_id,
                "username": names.get(&d.user_id),
                "events_sum_micro": d.events_sum_micro,
                "redis_effective_micro": d.redis_effective_micro,
                "pg_snapshot_micro": d.pg_snapshot_micro,
            })
        })
        .collect();
    Ok(Json(json!({ "drift_count": data.len(), "drifts": data })))
}
