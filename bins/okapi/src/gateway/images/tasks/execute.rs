use super::{AppError, AppState, Input, Lease, hash, store};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use okapi_api::codes;
use okapi_domain::Money;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::{sync::watch, task::JoinSet};
use uuid::Uuid;

const EXECUTION_TIMEOUT: Duration = Duration::from_secs(420);

/// Runs one persisted request; also advances recovery/settlement when no generation is needed.
pub async fn run_one(state: &AppState) -> Result<bool, AppError> {
    if recover_one(state).await? || settle_one(state, None).await? {
        return Ok(true);
    }
    if super::objects::run_one(state).await? {
        return Ok(true);
    }
    let Some(claimed) = store::claim(&state.pg).await? else {
        return Ok(false);
    };
    let lease = Lease {
        id: claimed.task.id,
        token: claimed.task.lease_id.ok_or_else(AppError::internal)?,
    };
    let result = tokio::time::timeout(EXECUTION_TIMEOUT, execute(state, &claimed, lease))
        .await
        .unwrap_or_else(|_| {
            Err(
                AppError::new(StatusCode::GATEWAY_TIMEOUT, codes::UPSTREAM_ERROR)
                    .with_param("image_task_timeout"),
            )
        });
    if let Err(error) = result {
        let cancelled = store::get_owned(
            &state.pg,
            lease.id,
            claimed.task.user_id,
            claimed.task.api_key_id,
        )
        .await?
        .is_some_and(|task| task.cancel_requested && task.status == "preparing");
        store::fail(
            &state.pg,
            lease.id,
            lease.token,
            i32::from(error.status.as_u16()),
            &json!({"code":error.code,"param":error.param}),
            cancelled,
        )
        .await?;
    }
    settle_one(state, Some(lease.id)).await?;
    Ok(true)
}

async fn execute(state: &AppState, claimed: &store::Claimed, lease: Lease) -> Result<(), AppError> {
    if hash(&claimed.payload) != claimed.request_hash {
        return Err(AppError::internal().with_param("image_task_payload_corrupt"));
    }
    let input = Input::decode(&claimed.payload, claimed.task.kind == "edit")?;
    input.require_nonstream()?;
    let key_hash: Option<String> = sqlx::query_scalar(
        "SELECT key_hash FROM api_keys WHERE id=$1 AND user_id=$2 AND deleted_at IS NULL",
    )
    .bind(claimed.task.api_key_id)
    .bind(claimed.task.user_id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let key_hash = key_hash.ok_or_else(|| AppError::unauthorized(codes::KEY_DISABLED))?;
    let key = okapi_store::auth::find_key_by_hash(&state.pg, &key_hash)
        .await?
        .filter(|key| key.is_usable(chrono::Utc::now()))
        .ok_or_else(|| AppError::unauthorized(codes::KEY_DISABLED))?;
    let ip = claimed.client_ip.as_ref().and_then(|ip| ip.parse().ok());
    if !key.allows_ip(ip) {
        return Err(AppError::new(StatusCode::FORBIDDEN, codes::IP_NOT_ALLOWED));
    }
    crate::gateway::refresh_pricebook_if_newer(state)
        .await
        .map_err(|_| AppError::internal())?;
    state.check_settle_backlog()?;
    let mut headers = HeaderMap::new();
    if let Ok(agent) = HeaderValue::from_str(&claimed.client_type) {
        headers.insert("user-agent", agent);
    }
    if let Some(ip) = &claimed.client_ip
        && let Ok(ip) = HeaderValue::from_str(ip)
    {
        headers.insert(crate::gateway::clients::PEER_IP_HEADER, ip);
    }
    super::super::handle(
        state,
        &key,
        &headers,
        input,
        lease.token,
        Instant::now(),
        if claimed.task.kind == "edit" {
            "/v1/images/edits"
        } else {
            "/v1/images/generations"
        },
        Some(lease),
    )
    .await?;
    Ok(())
}

/// Reserve may have completed just before a crash, but dispatch is durably marked *before* POST.
/// Only the former state may be requeued; the latter is uncertain and must never be replayed.
async fn recover_one(state: &AppState) -> Result<bool, AppError> {
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let expired:Option<store::Task>=sqlx::query_as("SELECT id,user_id,api_key_id,kind,model_name,status,lease_id,reservation_id,lease_until,attempts,cancel_requested,result,error,http_status,billing_pending,created_at,completed_at,expires_at FROM image_tasks WHERE status IN ('preparing','processing') AND lease_until<=now() ORDER BY lease_until FOR UPDATE SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await.map_err(okapi_store::StoreError::from)?;
    let Some(task) = expired else {
        return Ok(false);
    };
    if task.status == "preparing" && !task.cancel_requested && task.attempts < 3 {
        // Refund before requeue; retrying this transition cannot deduct a second reservation.
        state
            .ledger
            .refund(
                task.user_id,
                task.api_key_id,
                task.reservation_id.ok_or_else(AppError::internal)?,
            )
            .await?;
        sqlx::query("UPDATE image_tasks SET status='queued',lease_id=NULL,lease_until=NULL,updated_at=now() WHERE id=$1")
            .bind(task.id).execute(&mut *tx).await.map_err(okapi_store::StoreError::from)?;
    } else {
        let status = if task.status == "preparing" && task.cancel_requested {
            "cancelled"
        } else {
            "failed"
        };
        let param = if task.status == "processing" {
            "image_task_result_unknown"
        } else {
            "image_task_interrupted"
        };
        sqlx::query("UPDATE image_tasks SET status=$2,error=$3,http_status=502,payload=NULL,client_ip=NULL,storage_budget=$4,completed_at=now(),updated_at=now(),billing_pending=true,lease_id=NULL,lease_until=NULL WHERE id=$1")
            .bind(task.id).bind(status).bind(json!({"code":"upstream_error","param":param}))
            .bind(store::TASK_OVERHEAD_BYTES)
            .execute(&mut *tx).await.map_err(okapi_store::StoreError::from)?;
    }
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    Ok(true)
}

/// The PG result/ledger transaction is authoritative. Redis commits/refunds are idempotent,
/// so an ambiguous acknowledgement or crash before clearing this flag can be retried.
async fn settle_one(state: &AppState, id: Option<Uuid>) -> Result<bool, AppError> {
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let task:Option<store::Task>=sqlx::query_as("SELECT id,user_id,api_key_id,kind,model_name,status,lease_id,reservation_id,lease_until,attempts,cancel_requested,result,error,http_status,billing_pending,created_at,completed_at,expires_at FROM image_tasks WHERE billing_pending AND ($1::uuid IS NULL OR id=$1) ORDER BY updated_at FOR UPDATE SKIP LOCKED LIMIT 1")
        .bind(id).fetch_optional(&mut *tx).await.map_err(okapi_store::StoreError::from)?;
    let Some(task) = task else {
        return Ok(false);
    };
    if task.status == "completed" {
        okapi_store::history::read_lock(&mut tx).await?;
        let amount:i64=sqlx::query_scalar("SELECT amount_micro FROM billing_financial_records WHERE request_id=$1 AND status=20 ORDER BY created_at DESC LIMIT 1")
            .bind(task.reservation_id.ok_or_else(AppError::internal)?).fetch_one(&mut *tx).await.map_err(okapi_store::StoreError::from)?;
        state
            .ledger
            .commit(
                task.user_id,
                task.api_key_id,
                task.reservation_id.ok_or_else(AppError::internal)?,
                Money::from_micros(amount),
            )
            .await?;
    } else if matches!(task.status.as_str(), "failed" | "cancelled") {
        state
            .ledger
            .refund(
                task.user_id,
                task.api_key_id,
                task.reservation_id.ok_or_else(AppError::internal)?,
            )
            .await?;
    } else {
        return Err(AppError::internal().with_param("image_task_invalid_settlement"));
    }
    sqlx::query("UPDATE image_tasks SET billing_pending=false,updated_at=now() WHERE id=$1")
        .bind(task.id)
        .execute(&mut *tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    Ok(true)
}

/// Bounded worker concurrency. The stop signal stops admission; live work gets its normal deadline.
pub async fn run_worker(state: AppState, mut stop: watch::Receiver<bool>) {
    let mut work = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut cleanup = tokio::time::interval(Duration::from_secs(60));
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            _=stop.changed()=>break,
            Some(result)=work.join_next(),if !work.is_empty()=>{
                if let Err(error)=result {tracing::error!(%error,"image worker task panicked");}
            }
            _=tick.tick()=>{
                while work.len()<4 {
                    let state=state.clone();
                    work.spawn(async move {
                        if let Err(error)=run_one(&state).await {tracing::error!(?error,"image task execution failed");}
                    });
                }
            }
            _=cleanup.tick()=>{
                if let Err(error)=store::cleanup(&state.pg).await {tracing::error!(%error,"image task cleanup failed");}
            }
        }
    }
    if tokio::time::timeout(EXECUTION_TIMEOUT + Duration::from_secs(15), async {
        while work.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        work.abort_all();
    }
}
