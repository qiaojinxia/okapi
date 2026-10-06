use super::{AppError, AppState, hash, upstream};
use okapi_providers::{
    Outbound,
    batch::{
        gemini::GeminiBatch,
        vertex::{VertexBatch, gcs::GcsStore},
    },
};
use okapi_store::{ChannelCandidate, credential};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Private frozen account. Do not implement Debug or return this through any HTTP DTO.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    version: u8,
    provider: String,
    api_base: String,
    credential: String,
    proxy_url: Option<String>,
    extra_headers: Vec<(String, String)>,
    gcs: Option<Gcs>,
    input_hash: String,
    metadata: BTreeMap<String, String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Gcs {
    bucket: String,
    #[serde(default = "gcs_base")]
    api_base: String,
}
fn gcs_base() -> String {
    "https://storage.googleapis.com".into()
}
pub(super) enum Remote {
    Gemini(GeminiBatch),
    Vertex(VertexBatch, Box<GcsStore>),
}
pub(super) fn eligible(c: &ChannelCandidate, provider: Option<&str>) -> bool {
    matches!(c.provider.as_str(), "gemini" | "vertex")
        && provider.is_none_or(|p| p == c.provider)
        && c.capabilities.get("batch_images") != Some(&serde_json::Value::Bool(false))
        && c.capabilities.get("images") != Some(&serde_json::Value::Bool(false))
}
impl Binding {
    pub async fn capture(
        state: &AppState,
        c: &ChannelCandidate,
        metadata: &BTreeMap<String, String>,
        input: &[u8],
    ) -> Result<Self, AppError> {
        let api_base = c
            .api_base
            .clone()
            .or_else(|| {
                (c.provider == "gemini")
                    .then(|| "https://generativelanguage.googleapis.com/v1beta".into())
            })
            .ok_or_else(AppError::internal)?;
        let gcs = if c.provider == "vertex" {
            let value = state.setting_cached("image_batch_gcs").await;
            Some(
                serde_json::from_value(
                    value
                        .as_ref()
                        .clone()
                        .ok_or_else(|| AppError::bad_request().with_param("image_batch_gcs"))?,
                )
                .map_err(|_| AppError::bad_request().with_param("image_batch_gcs"))?,
            )
        } else {
            None
        };
        let binding = Self {
            version: 1,
            provider: c.provider.clone(),
            api_base,
            credential: c.credential.clone(),
            proxy_url: c.proxy_url.clone(),
            extra_headers: c.extra_headers.clone(),
            gcs,
            input_hash: hash(input),
            metadata: metadata.clone(),
        };
        binding.validate(state).await?;
        Ok(binding)
    }
    pub fn encode(&self, state: &AppState) -> Result<Vec<u8>, AppError> {
        credential::seal_or_plain(
            state.master_key.as_deref(),
            &serde_json::to_string(self).map_err(|_| AppError::internal())?,
        )
        .map_err(Into::into)
    }
    pub fn decode(state: &AppState, bytes: &[u8], input: &[u8]) -> Result<Self, AppError> {
        let text = credential::open(state.master_key.as_deref(), bytes)?;
        let binding: Self = serde_json::from_str(&text)
            .map_err(|_| AppError::internal().with_param("batch_binding"))?;
        if binding.version != 1 || binding.input_hash != hash(input) {
            return Err(AppError::internal().with_param("batch_binding"));
        }
        Ok(binding)
    }
    async fn validate(&self, state: &AppState) -> Result<(), AppError> {
        crate::console::ssrf::validate_api_base(state, &self.api_base).await?;
        crate::console::ssrf::validate_credential(state, &self.credential).await?;
        if let Some(gcs) = &self.gcs {
            crate::console::ssrf::validate_api_base(state, &gcs.api_base).await?;
        }
        let outbound = Outbound {
            proxy_url: self.proxy_url.clone(),
            extra_headers: self.extra_headers.clone(),
            ..Default::default()
        };
        // Validate immutable protocol configuration before accepting/funding a job.
        // Vertex token refresh remains a worker operation; no cloud request is needed here.
        let invalid =
            |e: okapi_providers::batch::BatchError| AppError::bad_request().with_param(e.code);
        match self.provider.as_str() {
            "gemini" => {
                GeminiBatch::new(&self.api_base, &self.credential, &outbound).map_err(invalid)?;
            }
            "vertex" => {
                VertexBatch::new(&self.api_base, "configuration-validation", &outbound)
                    .map_err(invalid)?;
                let gcs = self.gcs.as_ref().ok_or_else(AppError::internal)?;
                GcsStore::new(
                    &gcs.api_base,
                    "configuration-validation",
                    &outbound,
                    &gcs.bucket,
                    &Uuid::nil().simple().to_string(),
                )
                .map_err(invalid)?;
            }
            _ => return Err(AppError::bad_request().with_param("batch_provider")),
        }
        Ok(())
    }
    pub async fn open(&self, state: &AppState, id: Uuid) -> Result<Remote, AppError> {
        self.validate(state).await?;
        let outbound = Outbound {
            proxy_url: self.proxy_url.clone(),
            extra_headers: self.extra_headers.clone(),
            ..Default::default()
        };
        match self.provider.as_str() {
            "gemini" => Ok(Remote::Gemini(
                GeminiBatch::new(&self.api_base, &self.credential, &outbound)
                    .map_err(|e| upstream(&e))?,
            )),
            "vertex" => {
                let config = self.gcs.as_ref().ok_or_else(AppError::internal)?;
                let token = state
                    .vertex
                    .access_token(&self.credential, &outbound)
                    .await
                    .map_err(|_| AppError::internal().with_param("batch_vertex_token"))?;
                let jobs = VertexBatch::new(&self.api_base, &token, &outbound)
                    .map_err(|e| upstream(&e))?;
                let files = GcsStore::new(
                    &config.api_base,
                    &token,
                    &outbound,
                    &config.bucket,
                    &id.simple().to_string(),
                )
                .map_err(|e| upstream(&e))?;
                Ok(Remote::Vertex(jobs, Box::new(files)))
            }
            _ => Err(AppError::internal().with_param("batch_provider")),
        }
    }
}
