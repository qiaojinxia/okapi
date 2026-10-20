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
    /// 用量应答里顺带给出的订阅档位（Codex 的 `plan_type`）。只交给档位观测，不进额度观测：
    /// 有它就不必再为档位单独请求一次限流很严的用量接口。
    #[serde(skip)]
    pub plan: Option<String>,
}

/// 一次观测在多久内算数。额度只靠后台轮询（每 5 分钟一次），有效期必须长过轮询间隔，
/// 否则两次轮询之间阈值不生效、超额账号照常放行。
pub const MAX_AGE_SECS: i64 = 600;

impl Snapshot {
    /// 观测是否还在有效期内。
    #[must_use]
    pub const fn fresh(&self, now: i64) -> bool {
        now.saturating_sub(self.observed_at) <= MAX_AGE_SECS
    }

    /// 最近一次窗口重置的时刻（还没到点的窗口里最早的那个）。观测过期或上游没给重置时刻时为 None。
    #[must_use]
    pub fn next_reset(&self, now: i64) -> Option<i64> {
        if !self.fresh(now) {
            return None;
        }
        self.windows
            .iter()
            .filter_map(|w| w.resets_at)
            .filter(|at| *at > now)
            .min()
    }

    pub fn headroom(&self, now: i64) -> Option<u8> {
        if !self.fresh(now) {
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

/// 用量地址：渠道 API 基址的源 + 固定路径。
pub(crate) fn url(
    context: &super::QuotaContext<'_>,
    path: &str,
) -> Result<reqwest::Url, UpstreamError> {
    let mut url = reqwest::Url::parse(context.api_base)
        .map_err(|_| UpstreamError::Build("quota_api_base".into()))?;
    url.set_query(None);
    url.set_fragment(None);
    url.set_path(path);
    Ok(url)
}

pub(crate) async fn fetch(
    context: &super::QuotaContext<'_>,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<Value, UpstreamError> {
    let url = url(context, path)?;
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
    read(response).await
}

/// 用量接口的应答：非 2xx 带上 `Retry-After` 报错，2xx 读成 JSON。
pub(crate) async fn read(response: reqwest::Response) -> Result<Value, UpstreamError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_reset_is_the_earliest_upcoming_window_of_a_fresh_observation() {
        let snapshot: Snapshot = serde_json::from_value(serde_json::json!({
        "observed_at": 1000, "allowed": null, "windows": [
            {"name": "stale", "used_percent": 90, "resets_at": 900, "window_secs": 18000},
            {"name": "week", "used_percent": 40, "resets_at": 2400, "window_secs": 604_800},
            {"name": "five_hour", "used_percent": 20, "resets_at": 1800, "window_secs": 18000},
            {"name": "unknown", "used_percent": 5, "resets_at": null, "window_secs": null},
        ]}))
        .unwrap();
        assert_eq!(snapshot.next_reset(1000), Some(1800));
        assert_eq!(
            snapshot.next_reset(1000 + MAX_AGE_SECS + 1),
            None,
            "过期观测不算"
        );
    }
}
