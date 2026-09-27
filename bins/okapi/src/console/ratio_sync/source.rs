use super::{AppState, MAX_BODY_BYTES, PricingTable, parse_source};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};

const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGES: usize = 512;

pub(super) async fn fetch(
    state: &AppState,
    url: &str,
    timeout: Duration,
) -> Result<PricingTable, &'static str> {
    tokio::time::timeout(timeout, collect(state, url, timeout))
        .await
        .map_err(|_| "timeout")?
}

async fn collect(
    state: &AppState,
    raw: &str,
    timeout: Duration,
) -> Result<PricingTable, &'static str> {
    super::super::ssrf::validate_api_base(state, raw)
        .await
        .map_err(|_| "source_url_rejected")?;
    let mut url = reqwest::Url::parse(raw).map_err(|_| "source_url_rejected")?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "client_build")?;
    let mut progress = Progress::default();
    let mut total_bytes = 0;
    let mut table = PricingTable::new();
    for _ in 0..MAX_PAGES {
        let value = read(&client, &url, &mut total_bytes).await?;
        let next = progress.advance(&value)?;
        table.extend(parse_source(&value).ok_or("unrecognized_shape")?);
        let Some(offset) = next else {
            return Ok(table);
        };
        // Never follow a source-supplied next URL. Keep the original origin/path/filter,
        // and only replace our validated numeric offset, without duplicate parameters.
        let query: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key != "offset")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(query)
            .append_pair("offset", &offset.to_string());
    }
    Err("pagination_limit")
}

async fn read(
    client: &reqwest::Client,
    url: &reqwest::Url,
    total_bytes: &mut usize,
) -> Result<Value, &'static str> {
    let mut response = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else {
                "request"
            }
        })?;
    if !response.status().is_success() {
        return Err("upstream_status");
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY_BYTES as u64)
    {
        return Err("body_too_large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| if e.is_timeout() { "timeout" } else { "body" })?
    {
        if chunk.len() > MAX_BODY_BYTES.saturating_sub(bytes.len()) {
            return Err("body_too_large");
        }
        if chunk.len() > MAX_SOURCE_BYTES.saturating_sub(*total_bytes) {
            return Err("source_too_large");
        }
        *total_bytes += chunk.len();
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "not_json")
}

#[derive(Default)]
struct Progress {
    offset: i64,
    total: Option<i64>,
    models: HashSet<String>,
}

impl Progress {
    fn advance(&mut self, value: &Value) -> Result<Option<i64>, &'static str> {
        if !value["models"].is_array() {
            return if self.total.is_some() {
                Err("pagination_invalid")
            } else {
                Ok(None)
            };
        }
        let has_page = ["total", "limit", "offset", "has_more", "next_offset"]
            .iter()
            .any(|key| value.get(*key).is_some());
        if !has_page && self.total.is_none() {
            return Ok(None);
        }
        let invalid = "pagination_invalid";
        let models = value["models"].as_array().ok_or(invalid)?;
        let total = value["total"].as_i64().filter(|v| *v >= 0).ok_or(invalid)?;
        let limit = value["limit"].as_i64().filter(|v| *v > 0).ok_or(invalid)?;
        let offset = value["offset"].as_i64().ok_or(invalid)?;
        if offset != self.offset || self.total.is_some_and(|v| v != total) {
            return Err("pagination_changed");
        }
        let count = i64::try_from(models.len()).map_err(|_| invalid)?;
        let end = offset.checked_add(count).ok_or(invalid)?;
        let more = value["has_more"].as_bool().ok_or(invalid)?;
        if count > limit || end > total || more != (end < total) || (more && count == 0) {
            return Err(invalid);
        }
        let next = if more {
            if value["next_offset"].as_i64() != Some(end) {
                return Err(invalid);
            }
            Some(end)
        } else {
            if !value.get("next_offset").is_some_and(Value::is_null) {
                return Err(invalid);
            }
            None
        };
        for model in models {
            let name = model["model"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or(invalid)?;
            if !self.models.insert(name.to_owned()) {
                return Err("pagination_changed");
            }
        }
        self.offset = end;
        self.total = Some(total);
        Ok(next)
    }
}
