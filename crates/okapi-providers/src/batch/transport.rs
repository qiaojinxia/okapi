use super::{BatchError, MAX_CONTROL_BYTES};
use crate::http::{Outbound, apply_extra_headers, proxy_url_ok};
use bytes::Bytes;
use reqwest::{Client, Method, RequestBuilder, Response, Url};
use serde_json::Value;
use std::io::Write;
use std::time::Duration;

#[derive(Clone)]
pub(super) struct Transport {
    client: Client,
    header: &'static str,
    credential: String,
    extra: Vec<(String, String)>,
}
impl Transport {
    pub(super) fn new(
        header: &'static str,
        credential: &str,
        outbound: &Outbound,
    ) -> Result<Self, BatchError> {
        if credential.is_empty() || reqwest::header::HeaderValue::from_str(credential).is_err() {
            return Err(BatchError::invalid("batch_credential"));
        }
        let mut builder = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if let Some(proxy) = &outbound.proxy_url {
            if !proxy_url_ok(proxy) {
                return Err(BatchError::invalid("batch_proxy"));
            }
            builder = builder
                .proxy(reqwest::Proxy::all(proxy).map_err(|_| BatchError::invalid("batch_proxy"))?);
        }
        Ok(Self {
            client: builder
                .build()
                .map_err(|_| BatchError::invalid("batch_client"))?,
            header,
            credential: credential.to_owned(),
            extra: outbound
                .extra_headers
                .iter()
                .filter(|(name, _)| {
                    let name = name.to_ascii_lowercase();
                    !name.starts_with("x-goog-upload-") && name != "accept-encoding"
                })
                .cloned()
                .collect(),
        })
    }
    pub(super) fn request(&self, method: Method, url: Url) -> RequestBuilder {
        apply_extra_headers(self.client.request(method, url), &self.extra)
            .header(self.header, &self.credential)
            .header("accept-encoding", "identity")
    }
}

/// Stop serialization at the byte budget, before allocating an oversized request body.
pub(super) fn serialize(value: &impl serde::Serialize, limit: usize) -> Result<Bytes, BatchError> {
    struct Limited {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("batch_input_size"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|e| {
        BatchError::invalid(if e.is_io() {
            "batch_input_size"
        } else {
            "batch_request_json"
        })
    })?;
    Ok(writer.bytes.into())
}

pub(super) fn base_url(raw: &str) -> Result<Url, BatchError> {
    let url = Url::parse(raw).map_err(|_| BatchError::invalid("batch_base_url"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BatchError::invalid("batch_base_url"));
    }
    Ok(url)
}

pub(super) async fn send(
    request: RequestBuilder,
    mutation: bool,
    missing_ok: bool,
) -> Result<Response, BatchError> {
    let response = request
        .send()
        .await
        .map_err(|_| BatchError::invalid("batch_transport").uncertain(mutation))?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) || (missing_ok && status == 404) {
        return Ok(response);
    }
    Err(BatchError {
        code: "batch_http_status",
        status: Some(status),
        retry_after_secs: response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok()),
        may_have_executed: mutation && (!(400..500).contains(&status) || status == 408),
    })
}
pub(super) async fn read(
    mut response: Response,
    limit: usize,
    mutation: bool,
) -> Result<Bytes, BatchError> {
    let too_large = || BatchError::invalid("batch_response_size").uncertain(mutation);
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_large());
    }
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| BatchError::invalid("batch_transport").uncertain(mutation))?
    {
        if chunk.len() > limit.saturating_sub(data.len()) {
            return Err(too_large());
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data.into())
}
pub(super) async fn json(request: RequestBuilder, mutation: bool) -> Result<Value, BatchError> {
    let body = read(
        send(request, mutation, false).await?,
        MAX_CONTROL_BYTES,
        mutation,
    )
    .await?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| BatchError::invalid("batch_response_json").uncertain(mutation))?;
    if !value.is_object() {
        return Err(BatchError::invalid("batch_response_json").uncertain(mutation));
    }
    Ok(value)
}
pub(super) async fn empty(
    request: RequestBuilder,
    mutation: bool,
    missing_ok: bool,
) -> Result<(), BatchError> {
    let response = send(request, mutation, missing_ok).await?;
    if missing_ok && response.status() == 404 {
        return Ok(());
    }
    let bytes = read(response, 64 * 1024, mutation).await?;
    if !bytes.is_empty() {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| BatchError::invalid("batch_response_json").uncertain(mutation))?;
        if !value.is_object() {
            return Err(BatchError::invalid("batch_response_json").uncertain(mutation));
        }
    }
    Ok(())
}
