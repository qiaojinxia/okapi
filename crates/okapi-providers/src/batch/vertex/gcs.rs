//! Each client is restricted to one generated batch namespace. Cleanup is version-specific.
use super::super::{
    BatchError, MAX_INPUT_BYTES, jsonl,
    transport::{self, Transport},
};
use crate::{
    aws_sigv4::{payload_hash, uri_encode},
    http::Outbound,
};
use bytes::Bytes;
use reqwest::{Method, Url};
use serde_json::Value;

#[derive(Clone)]
pub struct GcsStore {
    http: Transport,
    base: Url,
    bucket: String,
    prefix: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub key: String,
    pub generation: String,
}
#[derive(Debug)]
pub struct Page {
    pub objects: Vec<Object>,
    pub next_page: Option<String>,
}
#[derive(Debug, Clone, Copy)]
pub enum Versions {
    Live,
    All,
}

impl GcsStore {
    /// `batch_id` is a persisted 32-hex UUID; no caller-supplied bucket-wide cleanup prefix.
    pub fn new(
        api_base: &str,
        token: &str,
        outbound: &Outbound,
        bucket: &str,
        batch_id: &str,
    ) -> Result<Self, BatchError> {
        if !(3..=63).contains(&bucket.len())
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
            || !bucket.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !bucket.ends_with(|c: char| c.is_ascii_alphanumeric())
            || bucket.contains("..")
            || batch_id.len() != 32
            || !batch_id.bytes().all(|b| b.is_ascii_hexdigit())
            || token.is_empty()
        {
            return Err(BatchError::invalid("batch_gcs_namespace"));
        }
        Ok(Self {
            http: Transport::new("authorization", &format!("Bearer {token}"), outbound)?,
            base: transport::base_url(api_base)?,
            bucket: bucket.to_owned(),
            prefix: format!("okapi-batches/{batch_id}/"),
        })
    }
    fn url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        url.set_path(&format!("{}{path}", self.base.path().trim_end_matches('/')));
        url
    }
    fn object_url(&self, key: &str) -> Result<Url, BatchError> {
        self.check_key(key)?;
        Ok(self.url(&format!(
            "/storage/v1/b/{}/o/{}",
            self.bucket,
            uri_encode(key)
        )))
    }
    fn input_key(&self) -> String {
        format!("{}input.jsonl", self.prefix)
    }
    fn output_prefix(&self) -> String {
        format!("{}output/", self.prefix)
    }
    #[must_use]
    pub fn input_uri(&self) -> String {
        format!("gs://{}/{}", self.bucket, self.input_key())
    }
    #[must_use]
    pub fn output_uri(&self) -> String {
        format!("gs://{}/{}", self.bucket, self.output_prefix())
    }
    pub(super) fn check_output_uri(&self, uri: &str) -> Result<(), BatchError> {
        let expected = self.output_uri();
        let suffix = uri
            .strip_prefix(&expected)
            .ok_or_else(|| BatchError::invalid("batch_output_scope"))?;
        if uri.len() > 1100
            || suffix.chars().any(char::is_control)
            || suffix.contains(['?', '#', '%'])
            || suffix.split('/').any(|s| matches!(s, "." | ".."))
        {
            return Err(BatchError::invalid("batch_output_scope"));
        }
        Ok(())
    }
    fn check_key(&self, key: &str) -> Result<(), BatchError> {
        if key.len() > 1024
            || key.chars().any(char::is_control)
            || (key != self.input_key() && !key.starts_with(&self.output_prefix()))
        {
            return Err(BatchError::invalid("batch_object_scope"));
        }
        Ok(())
    }
    fn parse_object(&self, value: &Value) -> Result<Object, BatchError> {
        let key = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| BatchError::invalid("batch_object_name"))?;
        self.check_key(key)?;
        if value.get("bucket").and_then(Value::as_str) != Some(self.bucket.as_str()) {
            return Err(BatchError::invalid("batch_object_bucket"));
        }
        let generation = value
            .get("generation")
            .and_then(Value::as_str)
            .filter(|v| valid_generation(v))
            .ok_or_else(|| BatchError::invalid("batch_object_generation"))?;
        Ok(Object {
            key: key.to_owned(),
            generation: generation.to_owned(),
        })
    }
    /// Immutable input, conditionally created. A lost response can be verified without overwrite.
    pub async fn upload_input(&self, bytes: Bytes) -> Result<Object, BatchError> {
        if bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
            return Err(BatchError::invalid("batch_input_size"));
        }
        let hash = payload_hash(&bytes);
        let size = bytes.len();
        let key = self.input_key();
        let mut url = self.url(&format!("/upload/storage/v1/b/{}/o", self.bucket));
        url.query_pairs_mut()
            .append_pair("uploadType", "media")
            .append_pair("name", &key)
            .append_pair("ifGenerationMatch", "0");
        let result = transport::json(
            self.http
                .request(Method::POST, url)
                .header("content-type", "application/jsonl")
                .body(bytes),
            true,
        )
        .await;
        match result {
            Ok(value) => {
                let object = self.parse_object(&value).map_err(|e| e.uncertain(true))?;
                if object.key != key {
                    return Err(BatchError::invalid("batch_object_identity").uncertain(true));
                }
                Ok(object)
            }
            Err(error) if error.status == Some(412) => {
                let object = self.input().await?;
                let mut url = self.version_url(&object)?;
                url.query_pairs_mut().append_pair("alt", "media");
                let response =
                    transport::send(self.http.request(Method::GET, url), false, false).await?;
                let data = transport::read(response, size, false).await?;
                if data.len() != size || payload_hash(&data) != hash {
                    return Err(BatchError::invalid("batch_input_conflict"));
                }
                Ok(object)
            }
            Err(error) => Err(error),
        }
    }
    /// Recover the generation after an upload acknowledgement was lost.
    pub async fn input(&self) -> Result<Object, BatchError> {
        let value = transport::json(
            self.http
                .request(Method::GET, self.object_url(&self.input_key())?),
            false,
        )
        .await?;
        let object = self.parse_object(&value)?;
        if object.key != self.input_key() {
            return Err(BatchError::invalid("batch_object_identity"));
        }
        Ok(object)
    }
    pub async fn list_output(
        &self,
        page: Option<&str>,
        versions: Versions,
    ) -> Result<Page, BatchError> {
        self.list(&self.output_prefix(), page, versions).await
    }
    /// Includes every input/output generation, but never another task's prefix.
    /// parse_object additionally rejects unknown siblings inside this namespace.
    pub async fn list_cleanup(&self, page: Option<&str>) -> Result<Page, BatchError> {
        self.list(&self.prefix, page, Versions::All).await
    }
    /// Collection is confined to the directory actually reported by this job;
    /// lifecycle cleanup may separately enumerate the whole task namespace.
    pub async fn list_results(
        &self,
        output_uri: &str,
        page: Option<&str>,
    ) -> Result<Page, BatchError> {
        self.check_output_uri(output_uri)?;
        let key = output_uri
            .strip_prefix(&format!("gs://{}/", self.bucket))
            .ok_or_else(|| BatchError::invalid("batch_output_scope"))?;
        let prefix = if key.ends_with('/') {
            key.to_owned()
        } else {
            format!("{key}/")
        };
        self.list(&prefix, page, Versions::Live).await
    }
    async fn list(
        &self,
        prefix: &str,
        page: Option<&str>,
        versions: Versions,
    ) -> Result<Page, BatchError> {
        if page.is_some_and(|p| p.is_empty() || p.len() > 4096 || p.chars().any(char::is_control)) {
            return Err(BatchError::invalid("batch_page_token"));
        }
        let mut url = self.url(&format!("/storage/v1/b/{}/o", self.bucket));
        url.query_pairs_mut()
            .append_pair("prefix", prefix)
            .append_pair("maxResults", "1000")
            .append_pair(
                "versions",
                if matches!(versions, Versions::All) {
                    "true"
                } else {
                    "false"
                },
            );
        if let Some(page) = page {
            url.query_pairs_mut().append_pair("pageToken", page);
        }
        let value = transport::json(self.http.request(Method::GET, url), false).await?;
        let items = match value.get("items") {
            Some(v) => v
                .as_array()
                .filter(|v| v.len() <= 1000)
                .ok_or_else(|| BatchError::invalid("batch_object_list"))?
                .as_slice(),
            None => &[],
        };
        let mut objects = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut live_keys = std::collections::HashSet::new();
        for item in items {
            let object = self.parse_object(item)?;
            if !object.key.starts_with(prefix)
                || !seen.insert((object.key.clone(), object.generation.clone()))
                || (matches!(versions, Versions::Live) && !live_keys.insert(object.key.clone()))
            {
                return Err(BatchError::invalid("batch_object_list"));
            }
            objects.push(object);
        }
        let next_page = value
            .get("nextPageToken")
            .map(|v| {
                v.as_str()
                    .filter(|s| {
                        !s.is_empty()
                            && s.len() <= 4096
                            && !s.chars().any(char::is_control)
                            && Some(*s) != page
                    })
                    .map(str::to_owned)
                    .ok_or_else(|| BatchError::invalid("batch_page_token"))
            })
            .transpose()?;
        Ok(Page { objects, next_page })
    }
    fn version_url(&self, object: &Object) -> Result<Url, BatchError> {
        if !valid_generation(&object.generation) {
            return Err(BatchError::invalid("batch_object_generation"));
        }
        let mut url = self.object_url(&object.key)?;
        url.query_pairs_mut()
            .append_pair("generation", &object.generation);
        Ok(url)
    }
    pub async fn download(
        &self,
        object: &Object,
        limits: jsonl::Limits,
    ) -> Result<jsonl::Reader, BatchError> {
        limits.validate()?;
        let mut url = self.version_url(object)?;
        url.query_pairs_mut().append_pair("alt", "media");
        let response = transport::send(self.http.request(Method::GET, url), false, false).await?;
        jsonl::Reader::new(response, limits)
    }
    pub async fn delete(&self, object: &Object) -> Result<(), BatchError> {
        transport::empty(
            self.http.request(Method::DELETE, self.version_url(object)?),
            true,
            true,
        )
        .await
    }
}
fn valid_generation(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|c| c.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|v| v > 0)
}
