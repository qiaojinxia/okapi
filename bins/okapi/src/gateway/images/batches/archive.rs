use super::{
    AppError, AppState, Response, StatusCode, hash, map_store, not_found, parse_id, public,
    public_id, store,
};
use async_zip::{Compression, ZipEntryBuilder, tokio::write::ZipFileWriter};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, Method},
};
use serde_json::{Value, json};
use std::{io, time::Duration};
use store::archive::{self as db, Archive, Entry, Lease};
use tokio::{
    io::{AsyncReadExt, DuplexStream},
    sync::OwnedSemaphorePermit,
    task::JoinHandle,
};
use uuid::Uuid;

struct Transfer {
    reader: DuplexStream,
    task: Option<JoinHandle<Result<(), AppError>>>,
    pg: sqlx::PgPool,
    lease: Lease,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Transfer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let pg = self.pg.clone();
        let lease = self.lease;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = db::release(&pg, lease).await;
            });
        }
        // Process death or a failed release is bounded by the persisted deadline.
    }
}
fn incomplete() -> io::Error {
    io::Error::other("batch_archive_incomplete")
}
fn filename(entry: &Entry) -> Result<Option<String>, AppError> {
    if entry.state != "succeeded" {
        return Ok(None);
    }
    let extension = match entry.content_type.as_deref() {
        Some("image/png") => "png",
        Some("image/jpeg") => "jpg",
        Some("image/webp") => "webp",
        _ => return Err(AppError::internal().with_param("batch_archive_mime")),
    };
    Ok(Some(format!("images/{:04}.{extension}", entry.slot)))
}
fn manifest(archive: &Archive) -> Result<Vec<u8>, AppError> {
    let outputs:Vec<Value>=archive.entries.iter().map(|entry| {
        Ok(json!({"slot":entry.slot,"index":entry.image_index,"custom_id":entry.custom_id,"status":entry.state,"file":filename(entry)?,"mime_type":entry.content_type,"error_code":entry.error_code}))
    }).collect::<Result<_,AppError>>()?;
    serde_json::to_vec_pretty(&json!({"batch":public(&archive.batch),"outputs":outputs}))
        .map_err(|_| AppError::internal())
}
async fn write(
    pg: sqlx::PgPool,
    archive: Archive,
    lease: Lease,
    writer: DuplexStream,
    manifest: Vec<u8>,
) -> Result<(), AppError> {
    let mut zip = ZipFileWriter::with_tokio(writer).force_no_zip64();
    zip.write_entry_whole(
        ZipEntryBuilder::new("manifest.json".into(), Compression::Stored),
        &manifest,
    )
    .await
    .map_err(|_| AppError::internal())?;
    for entry in &archive.entries {
        let Some(name) = filename(entry)? else {
            continue;
        };
        let data = db::content(&pg, lease, entry.slot)
            .await
            .map_err(map_store)?
            .ok_or_else(not_found)?;
        if Some(data.len()) != entry.bytes.and_then(|v| usize::try_from(v).ok())
            || entry.content_hash.as_deref() != Some(hash(&data).as_str())
        {
            return Err(AppError::internal().with_param("batch_archive_integrity"));
        }
        zip.write_entry_whole(
            ZipEntryBuilder::new(name.into(), Compression::Stored),
            &data,
        )
        .await
        .map_err(|_| AppError::internal())?;
    }
    zip.close().await.map_err(|_| AppError::internal())?;
    Ok(())
}
pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let rid = Uuid::new_v4();
    let result = async {
        let key = crate::gateway::auth::authenticate_data_plane(&state, &headers).await?;
        let id = parse_id(&id)?;
        let permit = if method == Method::HEAD {
            None
        } else {
            Some(
                state
                    .image_download_gate
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| {
                        AppError::new(
                            StatusCode::TOO_MANY_REQUESTS,
                            okapi_api::codes::RATE_LIMITED,
                        )
                        .with_param("image_download_capacity")
                    })?,
            )
        };
        let archive = db::open(&state.pg, id, key.user_id, key.key_id, permit.is_some())
            .await
            .map_err(map_store)?
            .ok_or_else(not_found)?;
        let body = if let Some(permit) = permit {
            let lease = archive.lease.ok_or_else(AppError::internal)?;
            let (writer, reader) = tokio::io::duplex(64 * 1024);
            // Construct the drop guard before any fallible preparation after lease acquisition.
            let mut transfer = Transfer {
                reader,
                task: None,
                pg: state.pg.clone(),
                lease,
                _permit: permit,
            };
            let manifest = manifest(&archive)?;
            let pg = state.pg.clone();
            transfer.task = Some(tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_secs(db::LEASE_SECONDS - 10),
                    write(pg, archive, lease, writer, manifest),
                )
                .await
                .unwrap_or_else(|_| Err(AppError::internal().with_param("batch_archive_timeout")))
            }));
            let stream = futures::stream::try_unfold(transfer, |mut transfer| async move {
                let mut bytes = vec![0; 64 * 1024];
                let n = transfer.reader.read(&mut bytes).await?;
                if n == 0 {
                    transfer
                        .task
                        .take()
                        .ok_or_else(incomplete)?
                        .await
                        .map_err(|_| incomplete())?
                        .map_err(|_| incomplete())?;
                    Ok::<_, io::Error>(None)
                } else {
                    bytes.truncate(n);
                    Ok(Some((Bytes::from(bytes), transfer)))
                }
            });
            Body::from_stream(stream)
        } else {
            Body::empty()
        };
        Response::builder()
            .header("content-type", "application/zip")
            .header(
                "content-disposition",
                format!("attachment; filename=\"{}.zip\"", public_id(id)),
            )
            .header("cache-control", "private, no-store")
            .header("x-content-type-options", "nosniff")
            .body(body)
            .map(|r| crate::gateway::error::with_request_id(r, rid))
            .map_err(|_| AppError::internal())
    }
    .await;
    result.unwrap_or_else(|e: AppError| e.into_response_with(Some(rid)))
}
