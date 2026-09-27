//! Vertex batchPredictionJobs with task-scoped GCS input/output.
mod cleanup;
pub mod gcs;
mod lookup;
use super::{
    BatchError, Job, JobState, Output, display_name, segment,
    transport::{self, Transport},
};
use crate::http::Outbound;
pub use cleanup::DeleteOperation;
use reqwest::{Method, Url};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct VertexBatch {
    http: Transport,
    base: Url,
    api_path: String,
    parent: String,
}
impl VertexBatch {
    /// Obtain/refresh the token with VertexUpstream::access_token before each worker operation.
    /// `api_base` contains /v1/projects/{project}/locations/{location}.
    pub fn new(
        api_base: &str,
        access_token: &str,
        outbound: &Outbound,
    ) -> Result<Self, BatchError> {
        let base = transport::base_url(api_base)?;
        let path = base.path().trim_end_matches('/');
        let (api_path, tail) = path
            .rsplit_once("/projects/")
            .ok_or_else(|| BatchError::invalid("batch_vertex_base"))?;
        let (project, location) = tail
            .split_once("/locations/")
            .ok_or_else(|| BatchError::invalid("batch_vertex_base"))?;
        if !(api_path.ends_with("/v1") || api_path.ends_with("/v1beta1"))
            || !segment(project)
            || !segment(location)
        {
            return Err(BatchError::invalid("batch_vertex_base"));
        }
        let api_path = api_path.to_owned();
        let parent = format!("projects/{project}/locations/{location}");
        if access_token.is_empty() {
            return Err(BatchError::invalid("batch_credential"));
        }
        Ok(Self {
            http: Transport::new("authorization", &format!("Bearer {access_token}"), outbound)?,
            base,
            api_path,
            parent,
        })
    }
    fn url(&self, resource: &str, suffix: &str) -> Url {
        let mut url = self.base.clone();
        url.set_path(&format!("{}/{resource}{suffix}", self.api_path));
        url
    }
    fn job_name(&self, name: &str) -> Result<(), BatchError> {
        let prefix = format!("{}/batchPredictionJobs/", self.parent);
        if !name.strip_prefix(&prefix).is_some_and(segment) {
            return Err(BatchError::invalid("batch_job_identity"));
        }
        Ok(())
    }
    pub async fn create(
        &self,
        model: &str,
        display: &str,
        files: &gcs::GcsStore,
    ) -> Result<Job, BatchError> {
        display_name(display)?;
        let model = model
            .strip_prefix("publishers/google/models/")
            .unwrap_or(model);
        if !segment(model) {
            return Err(BatchError::invalid("batch_model"));
        }
        let body = json!({"displayName":display,"model":format!("publishers/google/models/{model}"),
            "inputConfig":{"instancesFormat":"jsonl","gcsSource":{"uris":[files.input_uri()]}},
            "outputConfig":{"predictionsFormat":"jsonl","gcsDestination":{"outputUriPrefix":files.output_uri()}},
            "instanceConfig":{"keyField":"key"}});
        let value = transport::json(
            self.http
                .request(
                    Method::POST,
                    self.url(&format!("{}/batchPredictionJobs", self.parent), ""),
                )
                .json(&body),
            true,
        )
        .await?;
        self.parse(&value, None, files)
            .map_err(|e| e.uncertain(true))
    }
    pub async fn get(&self, name: &str, files: &gcs::GcsStore) -> Result<Job, BatchError> {
        self.job_name(name)?;
        let value =
            transport::json(self.http.request(Method::GET, self.url(name, "")), false).await?;
        self.parse(&value, Some(name), files)
    }
    pub async fn cancel(&self, name: &str) -> Result<(), BatchError> {
        self.job_name(name)?;
        transport::empty(
            self.http
                .request(Method::POST, self.url(name, ":cancel"))
                .json(&json!({})),
            true,
            false,
        )
        .await
    }
    fn parse(
        &self,
        value: &Value,
        expected: Option<&str>,
        files: &gcs::GcsStore,
    ) -> Result<Job, BatchError> {
        let invalid = || BatchError::invalid("batch_job_response");
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        self.job_name(name)?;
        if expected.is_some_and(|v| v != name) {
            return Err(BatchError::invalid("batch_job_identity"));
        }
        let state = JobState::parse(
            value
                .get("state")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?,
        )?;
        let error_code = value
            .get("error")
            .map(|v| {
                v.get("code")
                    .and_then(Value::as_i64)
                    .filter(|n| *n >= 0)
                    .ok_or_else(invalid)
            })
            .transpose()?
            .filter(|n| *n != 0);
        if error_code.is_some()
            && (!state.terminal()
                || matches!(state, JobState::Succeeded | JobState::PartiallySucceeded))
        {
            return Err(invalid());
        }
        let mut output = files.output_uri();
        for path in [
            "/outputConfig/gcsDestination/outputUriPrefix",
            "/outputInfo/gcsOutputDirectory",
        ] {
            if let Some(value) = value.pointer(path) {
                let uri = value.as_str().ok_or_else(invalid)?;
                files.check_output_uri(uri)?;
                uri.clone_into(&mut output);
            }
        }
        Ok(Job {
            name: name.to_owned(),
            state,
            output: state.terminal().then_some(Output::GcsPrefix(output)),
            error_code,
        })
    }
}
