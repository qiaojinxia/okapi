use super::{AppError, AppState, Batch, HashSet, invalid, limits, read, store, upstream};
use okapi_providers::batch::vertex::gcs::GcsStore;

pub(super) async fn collect(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
    files: &GcsStore,
    directory: &str,
    seen: &mut HashSet<u32>,
    mut remaining: usize,
) -> Result<(), AppError> {
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut objects = HashSet::new();
    for _ in 0..32 {
        let page = files
            .list_results(directory, cursor.as_deref())
            .await
            .map_err(|e| upstream(&e))?;
        for object in page.objects {
            if !objects.insert(object.key.clone()) || objects.len() > 200 {
                return Err(invalid("batch_result_files"));
            }
            let mut reader = files
                .download(&object, limits(remaining)?)
                .await
                .map_err(|e| upstream(&e))?;
            read(state, row, lease, &mut reader, seen).await?;
            remaining = remaining
                .checked_sub(reader.bytes_read())
                .ok_or_else(|| invalid("batch_result_budget"))?;
        }
        let Some(next) = page.next_page else {
            return Ok(());
        };
        if !cursors.insert(next.clone()) {
            return Err(invalid("batch_result_pages"));
        }
        cursor = Some(next);
    }
    Err(invalid("batch_result_pages"))
}
