use super::{
    AppError, AppState, Batch, Remote, Step, funding, map_store, observe, store, upstream,
};
use bytes::Bytes;
use okapi_providers::batch::gemini::{File, FileState, GeminiBatch};
use okapi_store::credential;
use serde_json::json;

pub(super) async fn submit(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
    payload: store::Payload,
    remote: Remote,
) -> Result<Step, AppError> {
    let file = match &remote {
        Remote::Gemini(client) => {
            let file = upload(state, row, lease, &payload, client).await?;
            match file.state {
                FileState::Processing => return Ok(Step::Retry(3)),
                FileState::Failed => {
                    store::abort_before_submission(&state.pg, lease, "batch_input_failed")
                        .await
                        .map_err(map_store)?;
                    return Ok(Step::Retry(0));
                }
                FileState::Active => Some(file),
            }
        }
        Remote::Vertex(_, files) => {
            let object = files
                .upload_input(Bytes::from(payload.input))
                .await
                .map_err(|e| upstream(&e))?;
            store::save_input(
                &state.pg,
                lease,
                &json!({"kind":"gcs","key":object.key,"generation":object.generation}),
                None,
            )
            .await
            .map_err(map_store)?;
            None
        }
    };
    // Recheck after potentially long uploads, immediately before the durable one-shot intent.
    if funding::stop_before_submit(state, row, lease).await? {
        return Ok(Step::Retry(0));
    }
    let submitted = store::mark_submitting(&state.pg, lease)
        .await
        .map_err(map_store)?;
    let display = super::display_name(&submitted)?;
    let result = match &remote {
        Remote::Gemini(client) => {
            client
                .create_file(
                    &row.upstream_model,
                    &display,
                    file.as_ref().ok_or_else(AppError::internal)?,
                )
                .await
        }
        Remote::Vertex(client, files) => client.create(&row.upstream_model, &display, files).await,
    };
    match result {
        Ok(job) => {
            observe(state, &submitted, &job).await?;
        }
        Err(error) if !error.may_have_executed => {
            store::submission_rejected(&state.pg, lease, error.code)
                .await
                .map_err(map_store)?;
        }
        Err(error) => return Err(upstream(&error)),
    }
    Ok(Step::Retry(0))
}
async fn upload(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
    payload: &store::Payload,
    client: &GeminiBatch,
) -> Result<File, AppError> {
    let name = format!("files/{}", row.id.simple());
    match client.file(&name).await {
        Ok(file) => return Ok(file),
        Err(error) if error.status == Some(404) => {}
        Err(error) => return Err(upstream(&error)),
    }
    let reference = json!({"kind":"gemini_file","name":name});
    let session = if let Some(bytes) = &payload.upload_session {
        let secret = credential::open(state.master_key.as_deref(), bytes)?;
        client
            .restore_upload(&secret, &name, payload.input.len())
            .map_err(|e| upstream(&e))?
    } else {
        store::save_input(&state.pg, lease, &reference, None)
            .await
            .map_err(map_store)?;
        let session = client
            .start_upload(
                &name,
                &format!("okapi-{}", row.id.simple()),
                payload.input.len(),
            )
            .await
            .map_err(|e| upstream(&e))?;
        let secret = credential::seal_or_plain(state.master_key.as_deref(), &session.encode())?;
        store::save_input(&state.pg, lease, &reference, Some(&secret))
            .await
            .map_err(map_store)?;
        session
    };
    client
        .finish_upload(&session, Bytes::copy_from_slice(&payload.input))
        .await
        .map_err(|e| upstream(&e))
}
