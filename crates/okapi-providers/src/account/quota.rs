//! Provider-independent quota windows and bounded read-only HTTP queries.
use crate::UpstreamError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Window {
    pub name: String,
    pub used_percent: u8,
    pub resets_at: Option<i64>,
    pub window_secs: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub observed_at: i64,
    pub allowed: Option<bool>,
    pub windows: Vec<Window>,
    /// The configurable percentage threshold applies only to this window.
    /// Other reported windows still enforce the upstream's hard exhaustion.
    #[serde(default)]
    pub threshold_window: Option<String>,
}

impl Snapshot {
    pub fn headroom(&self, now: i64) -> Option<u8> {
        if now.saturating_sub(self.observed_at) > 120 {
            return None;
        }
        // A passed reset is not proof of zero usage; get a new observation.
        let live = |w: &&Window| w.used_percent <= 100 && w.resets_at.is_none_or(|at| at > now);
        if self
            .windows
            .iter()
            .filter(live)
            .any(|w| w.used_percent == 100)
        {
            return Some(0);
        }
        let remaining = self
            .windows
            .iter()
            .filter(live)
            .filter(|w| {
                self.threshold_window
                    .as_ref()
                    .is_none_or(|name| name == &w.name)
            })
            .map(|w| 100 - w.used_percent)
            .min();
        if self.allowed == Some(false)
            && self
                .windows
                .iter()
                .all(|w| w.resets_at.is_none_or(|at| at > now))
        {
            Some(0)
        } else {
            remaining
        }
    }
}

pub(crate) fn percentage(value: &Value) -> Option<u8> {
    let number = value.as_number()?.to_string();
    let (whole, fraction) = number.split_once('.').unwrap_or((&number, ""));
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rounded = whole
        .parse::<u64>()
        .ok()?
        .saturating_add(u64::from(fraction.bytes().any(|b| b != b'0')))
        .min(100);
    rounded.try_into().ok()
}

pub(crate) async fn fetch(
    context: &super::QuotaContext<'_>,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<Value, UpstreamError> {
    let mut url = reqwest::Url::parse(context.api_base)
        .map_err(|_| UpstreamError::Build("quota_api_base".into()))?;
    url.set_query(None);
    url.set_fragment(None);
    url.set_path(path);
    let mut request = context
        .http
        .probe_client(context.outbound.proxy_url.as_deref())?
        .get(url)
        .bearer_auth(context.access_token)
        .timeout(std::time::Duration::from_secs(10));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            UpstreamError::Timeout
        } else {
            UpstreamError::Connect("quota_probe".into())
        }
    })?;
    if !response.status().is_success() {
        return Err(UpstreamError::Status {
            status: response.status().as_u16(),
            body: bytes::Bytes::new(),
            retry_after_secs: response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok()),
        });
    }
    let body = crate::openai::response_bytes(response, Some(262_144)).await?;
    serde_json::from_slice(&body).map_err(|_| UpstreamError::Build("quota_bad_response".into()))
}
