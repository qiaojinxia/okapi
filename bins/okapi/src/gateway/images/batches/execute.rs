mod cleanup;
mod funding;
mod recovery;
mod settlement;
mod statistics;
pub use statistics::run_statistics;
mod upload;
pub use cleanup::run_cleanup;

use super::{
    AppError, AppState, Batch,
    binding::{Binding, Remote},
    map_store, results, store, upstream,
};
use okapi_providers::batch::{Job, JobState};
use std::time::Duration;
use tokio::{sync::watch, task::JoinSet};
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(600);
enum Step {
    Complete,
    Retry(u32),
    Pending(u32, &'static str),
}

/// `only` scopes a maintenance/test execution to one persisted job.
pub async fn run_one(state: &AppState, only: Option<Uuid>) -> Result<bool, AppError> {
    let Some(claim) = store::claim(&state.pg, only).await.map_err(map_store)? else {
        return Ok(false);
    };
    let work = tokio::time::timeout(DEADLINE, execute(state, &claim.batch, claim.lease));
    tokio::pin!(work);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    let outcome = loop {
        tokio::select! {
            result = &mut work => break result.unwrap_or_else(|_| Err(AppError::internal().with_param("batch_execution_timeout"))),
            _ = heartbeat.tick() => {
                // Dropping `work` on lease loss prevents any further provider request.
                if let Err(error) = store::renew(&state.pg, claim.lease).await { break Err(map_store(error)); }
            }
        }
    };
    match outcome {
        Ok(Step::Complete) => {}
        Ok(Step::Retry(seconds)) => store::release(&state.pg, claim.lease, seconds, None)
            .await
            .map_err(map_store)?,
        Ok(Step::Pending(seconds, code)) => {
            store::release(&state.pg, claim.lease, seconds, Some(code))
                .await
                .map_err(map_store)?;
        }
        Err(error) => {
            // An expired submitting lease is recovered as uncertain; never authorize another POST.
            let _ = store::release(&state.pg, claim.lease, 30, Some("batch_execution_retry")).await;
            return Err(error);
        }
    }
    Ok(true)
}

async fn execute(state: &AppState, row: &Batch, lease: store::Lease) -> Result<Step, AppError> {
    match row.state {
        store::State::Funding => return funding::fund(state, row, lease).await,
        store::State::Settling => {
            settlement::settle(state, row, lease).await?;
            return Ok(Step::Complete);
        }
        _ => {}
    }
    if row.state == store::State::Preparing
        && funding::stop_before_submit(state, row, lease).await?
    {
        return Ok(Step::Retry(0));
    }
    let payload = store::payload(&state.pg, lease).await.map_err(map_store)?;
    let binding = Binding::decode(state, &payload.binding, &payload.input)?;
    let remote = binding.open(state, row.id).await?;
    match row.state {
        store::State::Preparing => upload::submit(state, row, lease, payload, remote).await,
        store::State::Uncertain => recovery::recover(state, row, lease, &remote).await,
        store::State::Running | store::State::Collecting => {
            let name = row
                .provider_job_name
                .as_deref()
                .ok_or_else(AppError::internal)?;
            let mut job = get(&remote, name).await?;
            if row.cancel_requested && !job.state.terminal() {
                let cancellation = match &remote {
                    Remote::Gemini(client) => client.cancel(name).await,
                    Remote::Vertex(client, _) => client.cancel(name).await,
                };
                // Cancellation can race completion. A fresh terminal result wins even if
                // the cancel RPC itself was rejected or its acknowledgement was lost.
                job = get(&remote, name).await?;
                if !job.state.terminal() {
                    cancellation.map_err(|e| upstream(&e))?;
                }
            }
            let current = observe(state, row, &job).await?;
            if current.state == store::State::Collecting {
                results::collect(state, &current, lease, &remote, job).await?;
                Ok(Step::Retry(0))
            } else {
                Ok(Step::Retry(15))
            }
        }
        _ => Err(AppError::internal().with_param("batch_execution_state")),
    }
}
fn display_name(row: &Batch) -> Result<String, AppError> {
    let intent = row.submit_intent.ok_or_else(AppError::internal)?;
    Ok(format!("okapi-{}-{}", row.id.simple(), intent.simple()))
}
async fn get(remote: &Remote, name: &str) -> Result<Job, AppError> {
    match remote {
        Remote::Gemini(client) => client.get(name).await,
        Remote::Vertex(client, files) => client.get(name, files).await,
    }
    .map_err(|e| upstream(&e))
}
async fn observe(state: &AppState, row: &Batch, job: &Job) -> Result<Batch, AppError> {
    store::observe(
        &state.pg,
        row.id,
        row.submit_intent.ok_or_else(AppError::internal)?,
        store::Observation {
            job_name: &job.name,
            state: remote_state(job.state),
            output_ref: &results::reference(job),
        },
    )
    .await
    .map_err(map_store)
}
fn remote_state(state: JobState) -> store::RemoteState {
    match state {
        JobState::Pending => store::RemoteState::Pending,
        JobState::Running => store::RemoteState::Running,
        JobState::Cancelling => store::RemoteState::Cancelling,
        JobState::Paused => store::RemoteState::Paused,
        JobState::Succeeded => store::RemoteState::Succeeded,
        JobState::PartiallySucceeded => store::RemoteState::PartiallySucceeded,
        JobState::Failed => store::RemoteState::Failed,
        JobState::Cancelled => store::RemoteState::Cancelled,
        JobState::Expired => store::RemoteState::Expired,
    }
}

/// At most two native control/collection steps execute per worker process.
pub async fn run_worker(state: AppState, mut stop: watch::Receiver<bool>) {
    let mut work = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut phase = 0_u8;
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            _=stop.changed()=>break,
            Some(result)=work.join_next(),if !work.is_empty()=>{
                if let Err(error)=result {tracing::error!(%error,"native image batch worker panicked");}
            },
            _=tick.tick()=>{
                while work.len()<2 {
                    let state=state.clone();
                    phase = (phase + 1) % 3;
                    let phase = phase;
                    work.spawn(async move {
                        let result = match phase {
                            0 => run_one(&state,None).await,
                            1 => run_cleanup(&state,None).await,
                            _ => run_statistics(&state,None).await,
                        };
                        if let Err(error)=result {tracing::error!(?error,phase,"native image batch step failed");}
                    });
                }
            },
        }
    }
    if tokio::time::timeout(DEADLINE + Duration::from_secs(15), async {
        while work.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        work.abort_all();
    }
}
