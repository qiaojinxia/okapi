//! Private S3-compatible image objects. Credentials never become result URLs or error text.
pub mod fetch;

use crate::aws_sigv4::{self, AwsCredentials, SignParams};
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("{0}")]
pub struct StorageError(pub &'static str);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    pub id: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    #[serde(default = "default_path_style")]
    pub path_style: bool,
    #[serde(default)]
    pub allow_http: bool,
}
const fn default_path_style() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectRef {
    pub store_id: String,
    pub location_hash: String,
    pub key: String,
    pub version_id: Option<String>,
}

#[derive(Clone)]
pub struct S3Store {
    id: String,
    endpoint: Url,
    region: String,
    bucket: String,
    credentials: AwsCredentials,
    path_style: bool,
    client: Client,
    location_hash: String,
}

impl S3Store {
    pub fn new(config: S3Config) -> Result<Self, StorageError> {
        let endpoint =
            Url::parse(&config.endpoint).map_err(|_| StorageError("image_store_endpoint"))?;
        if !matches!(endpoint.scheme(), "https" | "http")
            || (endpoint.scheme() == "http" && !config.allow_http)
            || endpoint.host().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
        {
            return Err(StorageError("image_store_endpoint"));
        }
        if config.id.is_empty()
            || config.id.len() > 64
            || !config
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || config.region.is_empty()
            || config.region.len() > 64
            || !config
                .region
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || !(3..=63).contains(&config.bucket.len())
            || !config
                .bucket
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
            || !config
                .bucket
                .starts_with(|c: char| c.is_ascii_alphanumeric())
            || !config.bucket.ends_with(|c: char| c.is_ascii_alphanumeric())
            || config.bucket.contains("..")
            || config.access_key_id.is_empty()
            || config.secret_access_key.is_empty()
        {
            return Err(StorageError("image_store_config"));
        }
        if !config.path_style && endpoint.domain().is_none() {
            return Err(StorageError("image_store_virtual_host"));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_mins(1))
            .build()
            .map_err(|_| StorageError("image_store_client"))?;
        let location_hash = aws_sigv4::payload_hash(
            format!(
                "{}\n{}\n{}\n{}",
                endpoint, config.region, config.bucket, config.path_style
            )
            .as_bytes(),
        );
        Ok(Self {
            id: config.id,
            endpoint,
            region: config.region,
            bucket: config.bucket,
            credentials: AwsCredentials {
                access_key_id: config.access_key_id,
                secret_access_key: config.secret_access_key,
                session_token: config.session_token,
            },
            path_style: config.path_style,
            client,
            location_hash,
        })
    }
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    #[must_use]
    pub fn object(&self, key: String) -> ObjectRef {
        ObjectRef {
            store_id: self.id.clone(),
            location_hash: self.location_hash.clone(),
            key,
            version_id: None,
        }
    }

    fn url(&self, object: &ObjectRef) -> Result<Url, StorageError> {
        if object.store_id != self.id
            || object.location_hash != self.location_hash
            || object.key.is_empty()
            || object.key.len() > 1024
            || object
                .key
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(StorageError("image_store_location_mismatch"));
        }
        let mut url = self.endpoint.clone();
        if !self.path_style {
            let host = format!(
                "{}.{}",
                self.bucket,
                self.endpoint
                    .host_str()
                    .ok_or(StorageError("image_store_endpoint"))?
            );
            url.set_host(Some(&host))
                .map_err(|_| StorageError("image_store_endpoint"))?;
        }
        let key = object
            .key
            .split('/')
            .map(aws_sigv4::uri_encode)
            .collect::<Vec<_>>()
            .join("/");
        url.set_path(&if self.path_style {
            format!("/{}/{key}", self.bucket)
        } else {
            format!("/{key}")
        });
        if let Some(version) = &object.version_id {
            url.query_pairs_mut().append_pair("versionId", version);
        }
        Ok(url)
    }

    async fn request(
        &self,
        method: Method,
        object: &ObjectRef,
        content: Vec<u8>,
        mime: Option<&str>,
    ) -> Result<reqwest::Response, StorageError> {
        let url = self.url(object)?;
        let hash = aws_sigv4::payload_hash(&content);
        let mut headers = vec![("x-amz-content-sha256", hash.as_str())];
        if method == Method::PUT {
            headers.push(("if-none-match", "*"));
        }
        if let Some(mime) = mime {
            headers.push(("content-type", mime));
        }
        let signed = aws_sigv4::sign(
            &self.credentials,
            &SignParams {
                method: method.as_str(),
                url: &url,
                region: &self.region,
                service: "s3",
                headers: &headers,
                payload_hash: &hash,
                timestamp: chrono::Utc::now(),
            },
        );
        let mut request = self.client.request(method, url).body(content);
        for (k, v) in headers {
            request = request.header(k, v);
        }
        for (k, v) in signed {
            request = request.header(k, v);
        }
        request
            .send()
            .await
            .map_err(|_| StorageError("image_store_transport"))
    }

    /// Immutable key + conditional PUT makes acknowledgement-loss retries safe.
    pub async fn put(
        &self,
        object: &ObjectRef,
        content: Vec<u8>,
        mime: &str,
    ) -> Result<ObjectRef, StorageError> {
        let expected = aws_sigv4::payload_hash(&content);
        let size = content.len();
        let response = self
            .request(Method::PUT, object, content, Some(mime))
            .await?;
        let mut saved = object.clone();
        if response.status() == reqwest::StatusCode::PRECONDITION_FAILED {
            let (bytes, version) = self.get(object, size).await?;
            if bytes.len() != size || aws_sigv4::payload_hash(&bytes) != expected {
                return Err(StorageError("image_store_content_conflict"));
            }
            saved.version_id = version;
        } else {
            if response.status() != reqwest::StatusCode::OK {
                return Err(StorageError("image_store_put_failed"));
            }
            saved.version_id = version_id(&response)?;
        }
        Ok(saved)
    }
    pub async fn get(
        &self,
        object: &ObjectRef,
        limit: usize,
    ) -> Result<(Vec<u8>, Option<String>), StorageError> {
        let response = self.request(Method::GET, object, Vec::new(), None).await?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(StorageError("image_store_get_failed"));
        }
        let version = version_id(&response)?;
        Ok((fetch::read_limited(response, limit).await?, version))
    }
    pub async fn delete(&self, object: &ObjectRef) -> Result<(), StorageError> {
        // A timed-out PUT may have succeeded in a versioned bucket. Resolve that version
        // before deleting; an unversioned DELETE would only create a delete marker.
        let mut object = object.clone();
        if object.version_id.is_none() {
            let head = self
                .request(Method::HEAD, &object, Vec::new(), None)
                .await?;
            if head.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(());
            }
            if head.status() != reqwest::StatusCode::OK {
                return Err(StorageError("image_store_head_failed"));
            }
            object.version_id = version_id(&head)?;
        }
        let response = self
            .request(Method::DELETE, &object, Vec::new(), None)
            .await?;
        if !matches!(response.status().as_u16(), 204 | 404) {
            return Err(StorageError("image_store_delete_failed"));
        }
        Ok(())
    }
}

fn version_id(response: &reqwest::Response) -> Result<Option<String>, StorageError> {
    response
        .headers()
        .get("x-amz-version-id")
        .map(|v| {
            v.to_str()
                .ok()
                .filter(|v| !v.is_empty() && v.len() <= 1024)
                .map(str::to_owned)
                .ok_or(StorageError("image_store_version"))
        })
        .transpose()
}
