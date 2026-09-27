//! Gemini REST Batch + Files protocol. Upload and job creation are separate durable steps.
mod lookup;
mod state;
use super::{
    BatchError, Job, MAX_INLINE_BYTES, MAX_INPUT_BYTES, Request, display_name, jsonl, resource,
    segment,
    transport::{self, Transport},
};
use crate::http::Outbound;
use bytes::Bytes;
use reqwest::{Method, Url};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone)]
pub struct GeminiBatch {
    http: Transport,
    base: Url,
    prefix: String,
}
/// Contains an upload bearer URL. Do not log it; persistence, if needed, must be encrypted.
pub struct UploadSession {
    url: Url,
    length: usize,
    name: String,
}
impl UploadSession {
    /// This is secret material; seal before persistence, and never include it in a public DTO.
    pub fn encode(&self) -> String {
        json!({"version":1,"url":self.url.as_str(),"length":self.length,"name":self.name})
            .to_string()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Processing,
    Active,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub state: FileState,
}

impl GeminiBatch {
    pub fn restore_upload(
        &self,
        secret: &str,
        expected_name: &str,
        expected_length: usize,
    ) -> Result<UploadSession, BatchError> {
        let invalid = || BatchError::invalid("batch_upload_session");
        if secret.len() > 262_144 || expected_length == 0 || expected_length > MAX_INPUT_BYTES {
            return Err(invalid());
        }
        resource(expected_name, "files")?;
        let value: Value = serde_json::from_str(secret).map_err(|_| invalid())?;
        if value.get("version").and_then(Value::as_u64) != Some(1)
            || value.get("name").and_then(Value::as_str) != Some(expected_name)
            || value.get("length").and_then(Value::as_u64) != u64::try_from(expected_length).ok()
        {
            return Err(invalid());
        }
        Ok(UploadSession {
            url: self.upload_url(
                value
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?,
            )?,
            length: expected_length,
            name: expected_name.into(),
        })
    }
    /// `api_base` ends in /v1beta, optionally behind a configured reverse-proxy prefix.
    /// Its SSRF policy and account ownership must already be checked by the gateway.
    pub fn new(api_base: &str, credential: &str, outbound: &Outbound) -> Result<Self, BatchError> {
        let base = transport::base_url(api_base)?;
        let prefix = base
            .path()
            .trim_end_matches('/')
            .strip_suffix("/v1beta")
            .ok_or_else(|| BatchError::invalid("batch_gemini_base"))?
            .to_owned();
        Ok(Self {
            http: Transport::new("x-goog-api-key", credential, outbound)?,
            base,
            prefix,
        })
    }
    fn url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        url.set_path(&format!("{}{path}", self.prefix));
        url
    }
    fn upload_url(&self, raw: &str) -> Result<Url, BatchError> {
        let url = Url::parse(raw).map_err(|_| BatchError::invalid("batch_upload_url"))?;
        if url.origin() != self.base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.path() != format!("{}/upload/v1beta/files", self.prefix)
        {
            return Err(BatchError::invalid("batch_upload_url"));
        }
        Ok(url)
    }
    /// The caller chooses and persists a unique file name before starting the upload.
    pub async fn start_upload(
        &self,
        name: &str,
        display: &str,
        length: usize,
    ) -> Result<UploadSession, BatchError> {
        resource(name, "files")?;
        display_name(display)?;
        let id = &name[6..];
        if id.len() > 40
            || id.starts_with('-')
            || id.ends_with('-')
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || length == 0
            || length > MAX_INPUT_BYTES
        {
            return Err(BatchError::invalid("batch_upload_input"));
        }
        let request = self
            .http
            .request(Method::POST, self.url("/upload/v1beta/files"))
            .header("x-goog-upload-protocol", "resumable")
            .header("x-goog-upload-command", "start")
            .header("x-goog-upload-header-content-length", length)
            .header("x-goog-upload-header-content-type", "application/jsonl")
            .json(&json!({"file":{"name":name,"displayName":display}}));
        let response = transport::send(request, true, false).await?;
        let raw = response
            .headers()
            .get("x-goog-upload-url")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| BatchError::invalid("batch_upload_url").uncertain(true))?;
        let url = self.upload_url(raw).map_err(|e| e.uncertain(true))?;
        let _ = transport::read(response, 64 * 1024, true).await?;
        Ok(UploadSession {
            url,
            length,
            name: name.to_owned(),
        })
    }
    pub async fn finish_upload(
        &self,
        session: &UploadSession,
        bytes: Bytes,
    ) -> Result<File, BatchError> {
        self.upload_url(session.url.as_str())?;
        if bytes.len() != session.length {
            return Err(BatchError::invalid("batch_upload_length"));
        }
        let value = transport::json(
            self.http
                .request(Method::POST, session.url.clone())
                .header("x-goog-upload-offset", "0")
                .header("x-goog-upload-command", "upload, finalize")
                .header("content-type", "application/jsonl")
                .body(bytes),
            true,
        )
        .await?;
        parse_file(
            value
                .get("file")
                .ok_or_else(|| BatchError::invalid("batch_file_response").uncertain(true))?,
            &session.name,
        )
        .map_err(|e| e.uncertain(true))
    }
    pub async fn file(&self, name: &str) -> Result<File, BatchError> {
        resource(name, "files")?;
        let value = transport::json(
            self.http
                .request(Method::GET, self.url(&format!("/v1beta/{name}"))),
            false,
        )
        .await?;
        parse_file(&value, name)
    }
    pub async fn create_file(
        &self,
        model: &str,
        display: &str,
        file: &File,
    ) -> Result<Job, BatchError> {
        resource(&file.name, "files")?;
        if file.state != FileState::Active {
            return Err(BatchError::invalid("batch_file_not_active"));
        }
        self.create(model, display, &json!({"fileName":file.name}))
            .await
    }
    pub async fn create_inline(
        &self,
        model: &str,
        display: &str,
        requests: &[Request],
    ) -> Result<Job, BatchError> {
        #[derive(Serialize)]
        struct Metadata<'a> {
            key: &'a str,
        }
        #[derive(Serialize)]
        struct Inline<'a> {
            request: &'a Value,
            metadata: Metadata<'a>,
        }
        #[derive(Serialize)]
        struct Requests<'a> {
            requests: Vec<Inline<'a>>,
        }
        #[derive(Serialize)]
        struct Input<'a> {
            requests: Requests<'a>,
        }
        jsonl::validate(requests)?;
        let requests = requests
            .iter()
            .map(|r| Inline {
                request: &r.request,
                metadata: Metadata { key: &r.key },
            })
            .collect();
        self.create(
            model,
            display,
            &Input {
                requests: Requests { requests },
            },
        )
        .await
    }
    async fn create(
        &self,
        model: &str,
        display: &str,
        input: &impl Serialize,
    ) -> Result<Job, BatchError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Batch<'a, T: Serialize> {
            display_name: &'a str,
            input_config: &'a T,
        }
        #[derive(Serialize)]
        struct Envelope<'a, T: Serialize> {
            batch: Batch<'a, T>,
        }
        display_name(display)?;
        let model = model.strip_prefix("models/").unwrap_or(model);
        if !segment(model) {
            return Err(BatchError::invalid("batch_model"));
        }
        let body = transport::serialize(
            &Envelope {
                batch: Batch {
                    display_name: display,
                    input_config: input,
                },
            },
            MAX_INLINE_BYTES - 1,
        )
        .map_err(|_| BatchError::invalid("batch_inline_size"))?;
        let value = transport::json(
            self.http
                .request(
                    Method::POST,
                    self.url(&format!("/v1beta/models/{model}:batchGenerateContent")),
                )
                .header("content-type", "application/json")
                .body(body),
            true,
        )
        .await?;
        state::parse(&value, None).map_err(|e| e.uncertain(true))
    }
    pub async fn get(&self, name: &str) -> Result<Job, BatchError> {
        resource(name, "batches")?;
        let value = transport::json(
            self.http
                .request(Method::GET, self.url(&format!("/v1beta/{name}"))),
            false,
        )
        .await?;
        state::parse(&value, Some(name))
    }
    /// Acknowledgement only. Continue polling; cancellation can race with successful completion.
    pub async fn cancel(&self, name: &str) -> Result<(), BatchError> {
        resource(name, "batches")?;
        transport::empty(
            self.http
                .request(Method::POST, self.url(&format!("/v1beta/{name}:cancel"))),
            true,
            false,
        )
        .await
    }
    /// Deleting the operation does not cancel the job or delete its input/output files.
    pub async fn delete_job(&self, name: &str) -> Result<(), BatchError> {
        resource(name, "batches")?;
        transport::empty(
            self.http
                .request(Method::DELETE, self.url(&format!("/v1beta/{name}"))),
            true,
            true,
        )
        .await
    }
    pub async fn delete_file(&self, name: &str) -> Result<(), BatchError> {
        resource(name, "files")?;
        transport::empty(
            self.http
                .request(Method::DELETE, self.url(&format!("/v1beta/{name}"))),
            true,
            true,
        )
        .await
    }
    pub async fn download(
        &self,
        name: &str,
        limits: jsonl::Limits,
    ) -> Result<jsonl::Reader, BatchError> {
        resource(name, "files")?;
        limits.validate()?;
        let mut url = self.url(&format!("/download/v1beta/{name}:download"));
        url.query_pairs_mut().append_pair("alt", "media");
        let response = transport::send(self.http.request(Method::GET, url), false, false).await?;
        jsonl::Reader::new(response, limits)
    }
}
fn parse_file(value: &Value, expected: &str) -> Result<File, BatchError> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| *n == expected)
        .ok_or_else(|| BatchError::invalid("batch_file_identity"))?;
    let state = match value.get("state").and_then(Value::as_str) {
        Some("ACTIVE") => FileState::Active,
        Some("PROCESSING") => FileState::Processing,
        Some("FAILED") => FileState::Failed,
        _ => return Err(BatchError::invalid("batch_file_state")),
    };
    Ok(File {
        name: name.to_owned(),
        state,
    })
}
