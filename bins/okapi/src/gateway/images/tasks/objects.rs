//! Durable S3 offload. PG remains readable until conditional upload is confirmed.
mod db;
use super::{AppError, AppState};
use okapi_providers::{
    aws_sigv4::payload_hash,
    image_store::{ObjectRef, S3Config, S3Store, StorageError, fetch::FetchPolicy},
};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub active: Option<String>,
    #[serde(default)]
    pub stores: Vec<S3Config>,
    #[serde(default)]
    pub copy_urls: bool,
    #[serde(default)]
    pub fetch: FetchPolicy,
}

#[derive(Default)]
pub struct Storage {
    stores: HashMap<String, S3Store>,
    active: Option<String>,
    pub copy_urls: bool,
    pub fetch: FetchPolicy,
}
impl Storage {
    pub fn from_env() -> Result<Self, StorageError> {
        match std::env::var("OKAPI_IMAGE_STORAGE") {
            Ok(raw) if !raw.trim().is_empty() => Self::new(
                serde_json::from_str(&raw).map_err(|_| StorageError("image_storage_config"))?,
            ),
            Ok(_) | Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(_) => Err(StorageError("image_storage_config")),
        }
    }
    pub fn new(config: StorageConfig) -> Result<Self, StorageError> {
        config.fetch.validate()?;
        if config.stores.len() > 16 {
            return Err(StorageError("image_storage_config"));
        }
        let mut stores = HashMap::new();
        for entry in config.stores {
            let store = S3Store::new(entry)?;
            if stores.insert(store.id().to_owned(), store).is_some() {
                return Err(StorageError("image_storage_duplicate_id"));
            }
        }
        if config
            .active
            .as_ref()
            .is_some_and(|id| !stores.contains_key(id))
        {
            return Err(StorageError("image_storage_active_id"));
        }
        Ok(Self {
            stores,
            copy_urls: config.copy_urls || config.active.is_some(),
            active: config.active,
            fetch: config.fetch,
        })
    }
    fn find(&self, reference: &ObjectRef) -> Result<&S3Store, StorageError> {
        self.stores
            .get(&reference.store_id)
            .ok_or(StorageError("image_store_unavailable"))
    }
    pub async fn download(
        &self,
        artifact: okapi_store::image_tasks::StoredArtifact,
    ) -> Result<(Vec<u8>, String), StorageError> {
        if let Some(bytes) = artifact.content {
            return Ok((bytes, artifact.content_type));
        }
        let reference: ObjectRef = serde_json::from_value(
            artifact
                .reference
                .ok_or(StorageError("image_object_missing"))?,
        )
        .map_err(|_| StorageError("image_object_reference"))?;
        let expected = artifact
            .content_sha256
            .ok_or(StorageError("image_object_hash"))?;
        let size = usize::try_from(
            artifact
                .content_bytes
                .ok_or(StorageError("image_object_size"))?,
        )
        .ok()
        .filter(|n| *n <= 64 * 1024 * 1024)
        .ok_or(StorageError("image_object_size"))?;
        let (bytes, _) = self.find(&reference)?.get(&reference, size).await?;
        if bytes.len() != size || payload_hash(&bytes) != expected {
            return Err(StorageError("image_object_integrity"));
        }
        Ok((bytes, artifact.content_type))
    }
}

pub(super) fn error(error: StorageError) -> AppError {
    AppError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        okapi_api::codes::UPSTREAM_ERROR,
    )
    .with_param(error.0)
}

pub(super) async fn run_one(state: &AppState) -> Result<bool, AppError> {
    let active = state
        .image_storage
        .active
        .as_ref()
        .and_then(|id| state.image_storage.stores.get(id));
    let Some(mut work) = db::claim(&state.pg, active).await? else {
        return Ok(false);
    };
    let result = async {
        let store = state.image_storage.find(&work.reference)?;
        if work.deleting {
            store.delete(&work.reference).await?;
            Ok(None)
        } else {
            let bytes = work
                .content
                .take()
                .ok_or(StorageError("image_object_source_missing"))?;
            if payload_hash(&bytes) != work.hash {
                return Err(StorageError("image_object_integrity"));
            }
            store
                .put(&work.reference, bytes, &work.mime)
                .await
                .map(Some)
        }
    }
    .await;
    match result {
        Ok(reference) => db::finish(&state.pg, &work, reference).await?,
        Err(error) => {
            db::retry(&state.pg, &work, error.0).await?;
            tracing::warn!(object_id=%work.id,error=error.0,"image object operation deferred");
        }
    }
    Ok(true)
}
