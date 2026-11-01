use super::{AppError, AppState, Binding, DEADLINE, Remote, map_store, store, upstream};
use std::{collections::HashSet, time::Duration};
use store::cleanup::{self as db, Claim};
use uuid::Uuid;

/// Separate from execution: a terminal cleanup lease cannot submit or settle.
pub async fn run_cleanup(state: &AppState, only: Option<Uuid>) -> Result<bool, AppError> {
    let Some(claim) = db::claim(&state.pg, only).await.map_err(map_store)? else {
        return Ok(false);
    };
    let work = tokio::time::timeout(DEADLINE, execute(state, &claim));
    tokio::pin!(work);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    let result = loop {
        tokio::select! {
            result=&mut work => break result.unwrap_or_else(|_|Err(AppError::internal().with_param("batch_cleanup_timeout"))),
            _=heartbeat.tick()=> if let Err(e)=db::renew(&state.pg,claim.lease).await {break Err(map_store(e));}
        }
    };
    match result {
        Ok(true) => {}
        Ok(false) => db::release(&state.pg, claim.lease, 15, None)
            .await
            .map_err(map_store)?,
        Err(error) => {
            let _ = db::release(&state.pg, claim.lease, 60, Some("batch_cleanup_retry")).await;
            return Err(error);
        }
    }
    Ok(true)
}
async fn execute(state: &AppState, claim: &Claim) -> Result<bool, AppError> {
    let payload = db::payload(&state.pg, claim.lease)
        .await
        .map_err(map_store)?;
    let binding = Binding::decode(state, &payload.binding, &payload.input)?;
    let remote = binding.open(state, claim.batch.id).await?;
    if !claim.progress.job_removed {
        if !remove_job(state, claim, &remote).await? {
            return Ok(false);
        }
        db::job_removed(&state.pg, claim.lease)
            .await
            .map_err(map_store)?;
    }
    match remote {
        Remote::Gemini(client) => {
            // The upload name is generated before any network write, including
            // uploads whose acknowledgement was lost before input_ref was saved.
            client
                .delete_file(&format!("files/{}", claim.batch.id.simple()))
                .await
                .map_err(|e| upstream(&e))?;
            if claim
                .batch
                .output_ref
                .get("kind")
                .and_then(serde_json::Value::as_str)
                == Some("gemini_file")
            {
                let name = claim
                    .batch
                    .output_ref
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(AppError::internal)?;
                client.delete_file(name).await.map_err(|e| upstream(&e))?;
            }
        }
        Remote::Vertex(_, files) => {
            if !remove_objects(&files).await? {
                return Ok(false);
            }
        }
    }
    db::finish(&state.pg, claim.lease)
        .await
        .map_err(map_store)?;
    Ok(true)
}
async fn remove_job(state: &AppState, claim: &Claim, remote: &Remote) -> Result<bool, AppError> {
    let Some(name) = claim.batch.provider_job_name.as_deref() else {
        return Ok(true);
    };
    let job = match remote {
        Remote::Gemini(client) => client.get(name).await,
        Remote::Vertex(client, files) => client.get(name, files).await,
    };
    match job {
        Err(e) if e.status == Some(404) => return Ok(true),
        Err(e) => return Err(upstream(&e)),
        Ok(job) => {
            if !job.state.terminal()
                || claim.batch.remote_state.as_deref()
                    != Some(super::remote_state(job.state).code())
            {
                return Err(AppError::internal().with_param("batch_cleanup_remote_state"));
            }
        }
    }
    match remote {
        Remote::Gemini(client) => {
            client.delete_job(name).await.map_err(|e| upstream(&e))?;
            match client.get(name).await {
                Err(e) if e.status == Some(404) => Ok(true),
                Err(e) => Err(upstream(&e)),
                Ok(_) => Ok(false),
            }
        }
        Remote::Vertex(client, _) => {
            let operation = if let Some(operation) = &claim.progress.operation {
                client.deletion(name, operation).await
            } else {
                client.delete_job(name).await
            }
            .map_err(|e| upstream(&e))?;
            // Even a done/404 operation isn't sufficient: a fresh job GET on
            // the next step must confirm absence before deleting GCS objects.
            let pending = operation
                .as_ref()
                .filter(|o| !o.done)
                .map(|o| o.name.as_str());
            db::operation(&state.pg, claim.lease, pending)
                .await
                .map_err(map_store)?;
            if operation.as_ref().is_some_and(|o| o.failed) {
                return Err(AppError::internal().with_param("batch_cleanup_operation_failed"));
            }
            Ok(false)
        }
    }
}
async fn remove_objects(
    files: &okapi_providers::batch::vertex::gcs::GcsStore,
) -> Result<bool, AppError> {
    let mut cursor = None;
    let mut seen = HashSet::new();
    for _ in 0..32 {
        let page = files
            .list_cleanup(cursor.as_deref())
            .await
            .map_err(|e| upstream(&e))?;
        if !page.objects.is_empty() {
            for object in page.objects.iter().take(32) {
                files.delete(object).await.map_err(|e| upstream(&e))?;
            }
            // Deleting shifts list positions. Restart at the beginning on the
            // next step, so old pagination tokens can never skip a generation.
            return Ok(false);
        }
        let Some(next) = page.next_page else {
            return Ok(true);
        };
        if !seen.insert(next.clone()) {
            return Err(AppError::internal().with_param("batch_cleanup_page_cycle"));
        }
        cursor = Some(next);
    }
    Err(AppError::internal().with_param("batch_cleanup_page_limit"))
}
