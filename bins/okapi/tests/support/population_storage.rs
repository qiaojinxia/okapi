//! Execute maintenance SQL directly against the single-generation ClickHouse storage.
use okapi_store::{ChClient, StoreError};

pub(super) async fn execute(ch: &ChClient, sql: &str) -> Result<(), StoreError> {
    ch.execute(sql).await
}
