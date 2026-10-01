//! Fetch only image results, with checked DNS addresses pinned for each redirect hop.
use super::StorageError;
use base64::Engine as _;
use reqwest::{Client, Url};
use serde::Deserialize;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchPolicy {
    /// Exact origins explicitly trusted by the operator (including scheme and port).
    /// Only these may use HTTP or resolve to private addresses. Never inherited from channels.
    #[serde(default)]
    pub trusted_origins: Vec<String>,
}

impl FetchPolicy {
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.trusted_origins.len() > 32 {
            return Err(StorageError("image_fetch_policy"));
        }
        for origin in &self.trusted_origins {
            let url = parse_url(origin)?;
            if url.path() != "/"
                || url.query().is_some()
                || url.origin().ascii_serialization() != origin.trim_end_matches('/')
            {
                return Err(StorageError("image_fetch_policy"));
            }
        }
        Ok(())
    }
    fn trusted(&self, url: &Url) -> bool {
        let origin = url.origin().ascii_serialization();
        self.trusted_origins
            .iter()
            .any(|v| v.trim_end_matches('/') == origin)
    }
}

fn parse_url(raw: &str) -> Result<Url, StorageError> {
    let url = Url::parse(raw).map_err(|_| StorageError("image_fetch_url"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(StorageError("image_fetch_url"));
    }
    Ok(url)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 198 && matches!(b, 18 | 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return public_ip(ip.into());
            }
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

async fn client(url: &Url, policy: &FetchPolicy) -> Result<Client, StorageError> {
    let trusted = policy.trusted(url);
    if url.scheme() != "https" && !trusted {
        return Err(StorageError("image_fetch_https_required"));
    }
    let host = url
        .host_str()
        .ok_or(StorageError("image_fetch_url"))?
        .trim_matches(['[', ']']);
    let port = url
        .port_or_known_default()
        .ok_or(StorageError("image_fetch_url"))?;
    let addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| StorageError("image_fetch_dns"))?
        .map_err(|_| StorageError("image_fetch_dns"))?
        .take(17)
        .collect()
    };
    if addresses.is_empty()
        || addresses.len() > 16
        || (!trusted && addresses.iter().any(|a| !public_ip(a.ip())))
    {
        return Err(StorageError("image_fetch_private_target"));
    }
    let mut builder = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_mins(1));
    if url.domain().is_some() {
        builder = builder.resolve_to_addrs(host, &addresses);
    }
    builder
        .build()
        .map_err(|_| StorageError("image_fetch_client"))
}

pub async fn image(
    raw: &str,
    policy: &FetchPolicy,
    limit: usize,
) -> Result<(Vec<u8>, String), StorageError> {
    let bytes = if raw.starts_with("data:") {
        let (header, body) = raw
            .split_once(',')
            .ok_or(StorageError("image_fetch_data_url"))?;
        if !header.starts_with("data:image/")
            || !header.ends_with(";base64")
            || header.matches(';').count() != 1
            || body.len() > limit.saturating_mul(4).div_ceil(3) + 4
        {
            return Err(StorageError("image_fetch_data_url"));
        }
        base64::prelude::BASE64_STANDARD
            .decode(body)
            .map_err(|_| StorageError("image_fetch_data_url"))?
    } else {
        let mut url = parse_url(raw)?;
        let mut result = None;
        for hop in 0..=3 {
            let response = client(&url, policy)
                .await?
                .get(url.clone())
                .header("accept", "image/*")
                .header("accept-encoding", "identity")
                .send()
                .await
                .map_err(|_| StorageError("image_fetch_transport"))?;
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                if hop == 3 {
                    return Err(StorageError("image_fetch_redirect_limit"));
                }
                let next = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(StorageError("image_fetch_redirect"))?;
                url = parse_url(
                    url.join(next)
                        .map_err(|_| StorageError("image_fetch_redirect"))?
                        .as_str(),
                )?;
            } else {
                if response.status() != reqwest::StatusCode::OK {
                    return Err(StorageError("image_fetch_status"));
                }
                result = Some(read_limited(response, limit).await?);
                break;
            }
        }
        result.ok_or(StorageError("image_fetch_redirect_limit"))?
    };
    if bytes.is_empty() || bytes.len() > limit {
        return Err(StorageError("image_fetch_size"));
    }
    let mime = content_type(&bytes).ok_or(StorageError("image_fetch_not_image"))?;
    Ok((bytes, mime.into()))
}

pub(super) async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, StorageError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(StorageError("image_fetch_size"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| StorageError("image_fetch_transport"))?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(StorageError("image_fetch_size"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[must_use]
pub fn content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_special_and_mapped_addresses_are_blocked() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fe80::1",
            "fc00::1",
            "64:ff9b::a00:1",
            "2002:a00:1::1",
            "2001:db8::1",
            "3fff:1::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}
