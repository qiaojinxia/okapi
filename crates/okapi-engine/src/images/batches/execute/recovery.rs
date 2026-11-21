use super::{
    AppError, AppState, Batch, Remote, Step, display_name, map_store, remote_state, results, store,
    upstream,
};

pub(super) async fn recover(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
    remote: &Remote,
) -> Result<Step, AppError> {
    let mut scan = store::recovery(&state.pg, lease).await.map_err(map_store)?;
    if scan.conflict {
        return Ok(Step::Pending(300, "batch_recovery_ambiguous"));
    }
    let display = display_name(row)?;
    let input = format!("files/{}", row.id.simple());
    if !scan.complete {
        let page = lookup(remote, row, &display, &input, scan.next_page.as_deref()).await;
        let page = match page {
            Ok(page) => page,
            Err(error) if error.status == Some(400) && scan.next_page.is_some() => {
                // An opaque cursor can expire. Re-scan without forgetting identities
                // already found; restarting a read can never authorize another POST.
                store::restart_recovery(&state.pg, lease)
                    .await
                    .map_err(map_store)?;
                return Ok(Step::Pending(60, "batch_recovery_cursor_expired"));
            }
            Err(error) => return Err(upstream(&error)),
        };
        scan = store::recovery_page(
            &state.pg,
            lease,
            &scan,
            page.next_page.as_deref(),
            &page.names,
            page.conflict,
        )
        .await
        .map_err(map_store)?;
    }
    if scan.conflict {
        return Ok(Step::Pending(300, "batch_recovery_ambiguous"));
    }
    if !scan.complete {
        return Ok(Step::Retry(0));
    }
    let Some(name) = scan.candidate_name.as_deref() else {
        store::restart_recovery(&state.pg, lease)
            .await
            .map_err(map_store)?;
        return Ok(Step::Pending(60, "batch_recovery_not_found"));
    };
    let job = match remote {
        Remote::Gemini(client) => {
            client
                .get_verified(name, &display, &row.upstream_model, &input)
                .await
        }
        Remote::Vertex(client, files) => {
            client
                .get_verified(name, &display, &row.upstream_model, files)
                .await
        }
    };
    match job {
        Ok(job) => {
            store::adopt_recovered(
                &state.pg,
                lease,
                store::Observation {
                    job_name: &job.name,
                    state: remote_state(job.state),
                    output_ref: &results::reference(&job),
                },
            )
            .await
            .map_err(map_store)?;
            Ok(Step::Retry(0))
        }
        Err(error) if matches!(error.code, "batch_lookup_identity" | "batch_job_identity") => {
            store::conflict_recovery(&state.pg, lease)
                .await
                .map_err(map_store)?;
            Ok(Step::Pending(300, "batch_recovery_identity"))
        }
        Err(error) if error.status == Some(404) => {
            Ok(Step::Pending(60, "batch_recovery_not_found"))
        }
        Err(error) => Err(upstream(&error)),
    }
}

async fn lookup(
    remote: &Remote,
    row: &Batch,
    display: &str,
    input: &str,
    cursor: Option<&str>,
) -> Result<okapi_providers::batch::LookupPage, okapi_providers::batch::BatchError> {
    match remote {
        Remote::Gemini(client) => {
            client
                .lookup_page(display, &row.upstream_model, input, cursor)
                .await
        }
        Remote::Vertex(client, files) => {
            client
                .lookup_page(display, &row.upstream_model, files, cursor)
                .await
        }
    }
}
