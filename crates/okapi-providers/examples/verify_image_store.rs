//! Explicit live S3 smoke check; never silently skips when configuration is absent.
//! Use a dedicated empty test bucket. See docs/images-contract.md.
use okapi_providers::image_store::{ObjectRef, S3Config, S3Store, StorageError};
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw = std::env::var("OKAPI_S3_VERIFY_CONFIG")
        .map_err(|_| StorageError("set_OKAPI_S3_VERIFY_CONFIG_for_a_test_bucket"))?;
    let config: S3Config =
        serde_json::from_str(&raw).map_err(|_| StorageError("invalid_OKAPI_S3_VERIFY_CONFIG"))?;
    let versioned = match std::env::var("OKAPI_S3_VERIFY_VERSIONED").as_deref() {
        Ok("true") => true,
        Ok("false") => false,
        _ => return Err(StorageError("set_OKAPI_S3_VERIFY_VERSIONED_true_or_false").into()),
    };
    let store = S3Store::new(config)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    // Reserved characters exercise S3's single-encoded canonical path.
    let reference = store.object(format!(
        "okapi-verification/{nonce}-{}/图像 +%?#$.png",
        std::process::id()
    ));
    let result = verify(&store, &reference, versioned).await;
    // HEAD discovers an accepted PUT's version even when verification failed afterwards.
    let cleanup = store.delete(&reference).await;
    result?;
    cleanup?;
    println!(
        "S3_VERIFY passed: signed put/get, bounded read, conditional retry, content conflict, version handling, delete and missing-object cleanup; versioned={versioned}"
    );
    Ok(())
}

async fn verify(
    store: &S3Store,
    reference: &ObjectRef,
    versioned: bool,
) -> Result<(), StorageError> {
    let bytes = b"\x89PNG\r\n\x1a\nsynthetic-storage-verification".to_vec();
    let saved = store.put(reference, bytes.clone(), "image/png").await?;
    if saved.version_id.is_some() != versioned {
        return Err(StorageError("unexpected_bucket_versioning"));
    }
    let (received, version) = store.get(&saved, bytes.len()).await?;
    if received != bytes || version != saved.version_id {
        return Err(StorageError("download_mismatch"));
    }
    if store.get(&saved, bytes.len() - 1).await.is_ok() {
        return Err(StorageError("download_limit_not_enforced"));
    }
    let retried = store.put(reference, bytes.clone(), "image/png").await?;
    if retried.version_id != saved.version_id {
        return Err(StorageError("retry_created_another_version"));
    }
    let conflict = store
        .put(reference, vec![b'x'; bytes.len()], "image/png")
        .await;
    if !matches!(conflict, Err(StorageError("image_store_content_conflict"))) {
        return Err(StorageError("conflicting_content_not_rejected"));
    }
    if store.get(&saved, bytes.len()).await?.0 != bytes {
        return Err(StorageError("conflict_overwrote_original"));
    }
    store.delete(&saved).await?;
    if store.get(&saved, bytes.len()).await.is_ok() {
        return Err(StorageError("saved_version_not_deleted"));
    }
    // Simulate losing the PUT response: cleanup knows only the original object key.
    let uncertain = store.put(reference, bytes.clone(), "image/png").await?;
    store.delete(reference).await?;
    if store.get(&uncertain, bytes.len()).await.is_ok() {
        return Err(StorageError("uncertain_version_not_deleted"));
    }
    store.delete(reference).await?;
    Ok(())
}
