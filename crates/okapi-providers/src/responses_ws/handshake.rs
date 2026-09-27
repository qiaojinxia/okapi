use super::MAX_MESSAGE;
use crate::{HttpPool, Outbound, UpstreamError};
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        handshake::{client::generate_key, derive_accept_key},
        protocol::{Role, WebSocketConfig},
    },
};

pub(super) type Socket = WebSocketStream<reqwest::Upgraded>;

pub(super) async fn connect(
    http: &HttpPool,
    url: &str,
    headers: &[(&str, &str)],
    outbound: &Outbound,
) -> Result<(Socket, Option<String>), UpstreamError> {
    let mut url = reqwest::Url::parse(url)
        .map_err(|_| UpstreamError::Build("responses_ws_invalid_url".into()))?;
    let scheme = match url.scheme() {
        "ws" | "http" => "http",
        "wss" | "https" => "https",
        _ => return Err(UpstreamError::Build("responses_ws_invalid_scheme".into())),
    };
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(UpstreamError::Build("responses_ws_invalid_url".into()));
    }
    url.set_scheme(scheme)
        .map_err(|()| UpstreamError::Build("responses_ws_invalid_url".into()))?;
    // Reuse proxy/TLS settings, but never redirect credentials during an upgrade.
    let (client, request) = http
        .websocket(outbound, url)?
        .version(reqwest::Version::HTTP_11)
        .build_split();
    let mut request =
        request.map_err(|_| UpstreamError::Build("responses_ws_invalid_headers".into()))?;
    let key = generate_key();
    prepare_headers(request.headers_mut(), headers, &key)?;
    let mut response = client
        .execute(request)
        .await
        .map_err(|_| UpstreamError::Connect("responses_ws_handshake".into()))?;
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n >= 0);
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| UpstreamError::Connect("responses_ws_error_body".into()))?
        {
            let take = chunk.len().min(64 * 1024 - body.len());
            body.extend_from_slice(&chunk[..take]);
            if body.len() == 64 * 1024 {
                break;
            }
        }
        return Err(UpstreamError::Status {
            status,
            body: Bytes::from(body),
            retry_after_secs,
        });
    }
    validate_headers(response.headers(), &key)?;
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let upgraded = response
        .upgrade()
        .await
        .map_err(|_| UpstreamError::Connect("responses_ws_upgrade".into()))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
        .max_write_buffer_size(MAX_MESSAGE + 256 * 1024);
    Ok((
        WebSocketStream::from_raw_socket(upgraded, Role::Client, Some(config)).await,
        request_id,
    ))
}

fn prepare_headers(
    target: &mut HeaderMap,
    headers: &[(&str, &str)],
    key: &str,
) -> Result<(), UpstreamError> {
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| UpstreamError::Build("responses_ws_invalid_headers".into()))?;
        // Transport headers are solely owned by this handshake; no extensions or subprotocols offered.
        if name.as_str().starts_with("sec-websocket-")
            || matches!(
                name.as_str(),
                "host" | "connection" | "upgrade" | "content-length" | "transfer-encoding"
            )
        {
            return Err(UpstreamError::Build("responses_ws_reserved_header".into()));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|_| UpstreamError::Build("responses_ws_invalid_headers".into()))?;
        target.insert(name, value);
    }
    target.insert("connection", HeaderValue::from_static("Upgrade"));
    target.insert("upgrade", HeaderValue::from_static("websocket"));
    target.insert("sec-websocket-version", HeaderValue::from_static("13"));
    target.insert(
        "sec-websocket-key",
        HeaderValue::from_str(key)
            .map_err(|_| UpstreamError::Build("responses_ws_invalid_key".into()))?,
    );
    Ok(())
}

fn validate_headers(headers: &HeaderMap, key: &str) -> Result<(), UpstreamError> {
    let token = |name: &str, expected: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .flat_map(|h| h.split(','))
            .any(|value| value.trim().eq_ignore_ascii_case(expected))
    };
    if !token("connection", "upgrade")
        || !token("upgrade", "websocket")
        || headers
            .get("sec-websocket-accept")
            .and_then(|v| v.to_str().ok())
            != Some(derive_accept_key(key.as_bytes()).as_str())
        || headers.contains_key("sec-websocket-extensions")
        || headers.contains_key("sec-websocket-protocol")
    {
        return Err(UpstreamError::Connect(
            "responses_ws_invalid_handshake".into(),
        ));
    }
    Ok(())
}
