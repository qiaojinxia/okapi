use super::{
    Error, State,
    work::{self, Lease},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};

/// Never serialize this through the public API: cursors belong to a private account.
#[derive(sqlx::FromRow)]
pub struct Recovery {
    pub next_page: Option<String>,
    pub candidate_name: Option<String>,
    pub pages: i32,
    pub complete: bool,
    pub conflict: bool,
    cursor_hashes: Value,
}
impl Default for Recovery {
    fn default() -> Self {
        Self {
            next_page: None,
            candidate_name: None,
            pages: 0,
            complete: false,
            conflict: false,
            cursor_hashes: json!([]),
        }
    }
}
async fn load(tx: &mut Transaction<'_, Postgres>, lease: Lease) -> Result<Recovery, Error> {
    Ok(sqlx::query_as("SELECT next_page,candidate_name,pages,complete,conflict,cursor_hashes FROM image_batch_recovery WHERE batch_id=$1")
        .bind(lease.id).fetch_optional(&mut **tx).await?.unwrap_or_default())
}
async fn lock(pg: &PgPool, lease: Lease) -> Result<Transaction<'static, Postgres>, Error> {
    let (tx, row) = work::locked(pg, lease).await?;
    if row.state != State::Uncertain
        || row.submit_intent.is_none()
        || row.provider_job_name.is_some()
    {
        return Err(Error::Transition);
    }
    Ok(tx)
}
pub async fn recovery(pg: &PgPool, lease: Lease) -> Result<Recovery, Error> {
    let mut tx = lock(pg, lease).await?;
    let scan = load(&mut tx, lease).await?;
    tx.commit().await?;
    Ok(scan)
}
/// Checkpoint every page under the same lease/phase fence. One candidate is only
/// a hint until EOF; two distinct identities remain a durable conflict.
pub async fn recovery_page(
    pg: &PgPool,
    lease: Lease,
    previous: &Recovery,
    next: Option<&str>,
    names: &[String],
    conflict: bool,
) -> Result<Recovery, Error> {
    if next.is_some_and(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control))
        || names.len() > 100
        || names
            .iter()
            .any(|n| n.is_empty() || n.len() > 1024 || n.chars().any(char::is_control))
    {
        return Err(Error::Invalid("batch_recovery_page"));
    }
    let mut tx = lock(pg, lease).await?;
    let mut scan = load(&mut tx, lease).await?;
    if scan.complete
        || scan.conflict
        || scan.pages != previous.pages
        || scan.next_page != previous.next_page
    {
        return Err(Error::Transition);
    }
    if scan.pages >= 1024 {
        return Err(Error::Invalid("batch_recovery_limit"));
    }
    if let Some(token) = next {
        let hash = json!(hex::encode(Sha256::digest(token.as_bytes())));
        let hashes = scan
            .cursor_hashes
            .as_array_mut()
            .ok_or(Error::Invalid("batch_recovery_cursor"))?;
        if hashes.contains(&hash) {
            return Err(Error::Invalid("batch_recovery_cursor_cycle"));
        }
        hashes.push(hash);
    }
    scan.conflict = conflict;
    for name in names {
        match &scan.candidate_name {
            Some(old) if old != name => scan.conflict = true,
            None => scan.candidate_name = Some(name.clone()),
            _ => {}
        }
    }
    scan.pages += 1;
    scan.next_page = next.map(str::to_owned);
    scan.complete = next.is_none();
    sqlx::query("INSERT INTO image_batch_recovery(batch_id,next_page,candidate_name,pages,cursor_hashes,complete,conflict) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(batch_id) DO UPDATE SET next_page=EXCLUDED.next_page,candidate_name=EXCLUDED.candidate_name,pages=EXCLUDED.pages,cursor_hashes=EXCLUDED.cursor_hashes,complete=EXCLUDED.complete,conflict=EXCLUDED.conflict,updated_at=now()")
        .bind(lease.id).bind(&scan.next_page).bind(&scan.candidate_name).bind(scan.pages).bind(&scan.cursor_hashes).bind(scan.complete).bind(scan.conflict).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(scan)
}
/// Expired cursors can restart a read-only scan; preserve every prior candidate
/// and any ambiguity so a later scan cannot silently erase conflicting evidence.
pub async fn restart_recovery(pg: &PgPool, lease: Lease) -> Result<(), Error> {
    let mut tx = lock(pg, lease).await?;
    sqlx::query("UPDATE image_batch_recovery SET next_page=NULL,pages=0,cursor_hashes='[]',complete=false,updated_at=now() WHERE batch_id=$1 AND NOT conflict")
        .bind(lease.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn conflict_recovery(pg: &PgPool, lease: Lease) -> Result<(), Error> {
    let mut tx = lock(pg, lease).await?;
    sqlx::query("INSERT INTO image_batch_recovery(batch_id,conflict) VALUES($1,true) ON CONFLICT(batch_id) DO UPDATE SET conflict=true,updated_at=now()")
        .bind(lease.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Unlike the original POST acknowledgement, lookup evidence is only valid for
/// the current executor and its completed, unambiguous checkpoint.
pub async fn adopt_recovered(
    pg: &PgPool,
    lease: Lease,
    observation: super::Observation<'_>,
) -> Result<super::Batch, Error> {
    let (mut tx, row) = work::locked(pg, lease).await?;
    let scan = load(&mut tx, lease).await?;
    if row.state != State::Uncertain
        || row.submit_intent.is_none()
        || row.provider_job_name.is_some()
        || !scan.complete
        || scan.conflict
        || scan.candidate_name.as_deref() != Some(observation.job_name)
    {
        return Err(Error::Transition);
    }
    work::apply_observation(tx, row, observation).await
}
