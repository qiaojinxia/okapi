use super::{AppError, AppState, map_store, store};
use std::time::Duration;
use uuid::Uuid;

pub async fn run_statistics(state: &AppState, only: Option<Uuid>) -> Result<bool, AppError> {
    let Some(delivery) = store::statistics::claim(&state.pg, only)
        .await
        .map_err(map_store)?
    else {
        return Ok(false);
    };
    let result = tokio::time::timeout(
        Duration::from_mins(1),
        state.sched.record_batch_statistics(&delivery),
    )
    .await;
    let delivered = matches!(result, Ok(Ok(())));
    store::statistics::acknowledge(&state.pg, &delivery, delivered)
        .await
        .map_err(map_store)?;
    if !delivered {
        return Err(AppError::internal().with_param("batch_statistics_retry"));
    }
    Ok(true)
}
