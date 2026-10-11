//! Claude Pro/Max 订阅经 Claude Code 公开客户端的 OAuth（IMPLEMENTATION §11.38，实验性）。
//!
//! 与 `anthropic` 直连的差别只有四处：`Authorization: Bearer` 而非 `x-api-key`、URL 带
//! `?beta=true`、`anthropic-beta` 必含三个 Claude Code 标记、system 首元素须是 Claude Code 自述句。
//! 其余（SSE 事件、usage）完全一致，所以传输直接复用 `anthropic::send_messages_at`。
//!
//! 两种出向形态：
//! - 未配置客户端扩展（缺省，透传）：只发上游为这条路径**要求**的东西，客户端身份头由
//!   `Outbound.extra_headers` 透传——前面站着真实 Claude Code / Codex CLI 时用这个；
//! - 配置了 `extensions.client_profile`：由 [`crate::profiles`] 按最新抓包的 Claude Code
//!   客户端整形（UA、beta、system billing 块、metadata.user_id、cch），供非官方客户端走订阅额度。
//!
//! 端点与 scope 跟随 Claude Code CLI（2026-09 对照 Sub2API 与实测：旧 `console.anthropic.com`
//! 回调页已 301 到 `platform.claude.com`）。

pub mod account;

use super::{Pkce, Tokens, form_encode, parse_tokens, token_outbound};
use crate::anthropic::{ANTHROPIC_VERSION, MessagesResponse, send_messages_at};
use crate::error::UpstreamError;
use crate::profiles::identity::BETA_TOKEN_COUNTING;
use bytes::Bytes;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

/// Claude Code 公开客户端 id（Anthropic 私有，可能变更）。
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// 手动回调页：浏览器授权后页面直接显示 `code#state`，站长贴回控制面。
pub const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
/// 授权时申请的 scope，与 CLI 登录一致（2.1.294：`org:create_api_key` + 刷新那组）。
const SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins";
/// 刷新时申请的 scope：CLI 每次都带，不含 `org:create_api_key`。
const REFRESH_SCOPE: &str = "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins";
/// 申请 `REFRESH_SCOPE` 被拒（`invalid_scope`，没授过 `user:plugins` 的旧 token）时退回的 scope：
/// CLI 退回 token 当初授到的那组，即加 `user:plugins` 之前的授权 scope。
const GRANTED_SCOPE_BEFORE_PLUGINS: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// 订阅 token 走 Bearer 必须带的 beta 标记。
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// 订阅路径必带的 beta 集合：缺 `claude-code-20250219` 上游可能把请求当非 Claude Code 拒收。
pub const REQUIRED_BETAS: [&str; 3] = [
    "claude-code-20250219",
    OAUTH_BETA,
    "interleaved-thinking-2025-05-14",
];
/// 上游按这一句判定请求来自 Claude Code；必须是 system 数组首元素、逐字一致。
pub const SYSTEM_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
pub const DEFAULT_API_BASE: &str = "https://api.anthropic.com/v1";
const TOKEN_TIMEOUT: Duration = Duration::from_secs(20);

/// 授权 URL：`state` 就是 verifier（Anthropic 的流程把它原样带回，换码时要一起提交）。
#[must_use]
pub fn authorize_url(pkce: &Pkce) -> String {
    format!(
        "{AUTHORIZE_URL}?{}",
        form_encode(&[
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", &pkce.verifier),
        ])
    )
}

/// 回调页显示的是 `code#state`；只贴 code 也收。
#[must_use]
pub fn split_pasted_code(pasted: &str) -> (&str, Option<&str>) {
    let pasted = pasted.trim();
    match pasted.split_once('#') {
        Some((code, state)) => (code, Some(state)),
        None => (pasted, None),
    }
}

/// 换码（JSON 体）。`proxy_url` = 渠道代理，刷新与 API 请求同一出口。
pub async fn exchange(
    http: &crate::http::HttpPool,
    token_url: &str,
    code: &str,
    verifier: &str,
    proxy_url: Option<&str>,
) -> Result<Tokens, UpstreamError> {
    /// 字段顺序即 CLI 的插入顺序（serde_json 的 `Value` 会按字母序重排，这里用结构体）。
    #[derive(Serialize)]
    struct Exchange<'a> {
        grant_type: &'a str,
        code: &'a str,
        redirect_uri: &'a str,
        client_id: &'a str,
        code_verifier: &'a str,
        state: &'a str,
    }
    let body = Exchange {
        grant_type: "authorization_code",
        code,
        redirect_uri: REDIRECT_URI,
        client_id: CLIENT_ID,
        code_verifier: verifier,
        state: verifier,
    };
    post_token(http, token_url, json_body(&body)?, proxy_url).await
}

/// 刷新（JSON 体）。照 CLI：总是申请 `REFRESH_SCOPE`，被拒 `invalid_scope` 再按 token 原有的 scope 重试一次；
/// 不带 `expires_in`（那只出现在 `CLAUDE_CODE_OAUTH_REFRESH_TOKEN` 环境变量登录的路径上）。
///
/// `granted` 是凭证记下的已授 scope。只有它确实不含 `user:plugins`（授权早于 plugins 的旧 token）才退回：
/// 原本就授过的 scope 被拒多半是上游误报，降级重试拿到的新 token 会永久丢掉 plugins，宁可这次刷新失败。
/// 没记录（旧凭证）时按授权早于 plugins 的 scope 退回，与此前行为一致。
pub async fn refresh(
    http: &crate::http::HttpPool,
    token_url: &str,
    refresh_token: &str,
    granted: Option<&str>,
    proxy_url: Option<&str>,
) -> Result<Tokens, UpstreamError> {
    #[derive(Serialize)]
    struct TokenRefresh<'a> {
        grant_type: &'a str,
        refresh_token: &'a str,
        client_id: &'a str,
        scope: &'a str,
    }
    let body = |scope| {
        json_body(&TokenRefresh {
            grant_type: "refresh_token",
            refresh_token,
            client_id: CLIENT_ID,
            scope,
        })
    };
    match post_token(http, token_url, body(REFRESH_SCOPE)?, proxy_url).await {
        Err(UpstreamError::Status {
            status: 400,
            body: rejected,
            ..
        }) if invalid_scope(&rejected) => match downgrade_scope(granted) {
            Some(scope) => post_token(http, token_url, body(scope)?, proxy_url).await,
            None => Err(UpstreamError::Status {
                status: 400,
                body: rejected,
                retry_after_secs: None,
            }),
        },
        other => other,
    }
}

/// `invalid_scope` 之后退回哪个 scope；`None` = 不退（已授过 plugins）。
fn downgrade_scope(granted: Option<&str>) -> Option<&str> {
    match granted {
        None => Some(GRANTED_SCOPE_BEFORE_PLUGINS),
        Some(scope) if scope.split_whitespace().any(|s| s == "user:plugins") => None,
        Some(scope) => Some(scope),
    }
}

fn json_body(body: &impl Serialize) -> Result<String, UpstreamError> {
    serde_json::to_string(body).map_err(|e| UpstreamError::Build(e.to_string()))
}

/// token 端点的 400 是不是 `invalid_scope`（标准 OAuth 写在 `error`，也认 `code` / `error.type`）。
fn invalid_scope(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    [&value["error"], &value["code"], &value["error"]["type"]]
        .iter()
        .any(|field| field.as_str() == Some("invalid_scope"))
}

/// token 端点不跟随重定向：地址可被管理员覆写（`oauth_token_url`），SSRF 闸只看得到填进来的那个 URL。
async fn post_token(
    http: &crate::http::HttpPool,
    token_url: &str,
    body: String,
    proxy_url: Option<&str>,
) -> Result<Tokens, UpstreamError> {
    let url = reqwest::Url::parse(token_url).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let user_agent = crate::profiles::ClaudeCodeRevision::default().axios_user_agent();
    let resp = send_account(
        http,
        reqwest::Method::POST,
        url,
        &[
            ("content-type", "application/json"),
            ("user-agent", user_agent),
        ],
        Some(body),
        TOKEN_TIMEOUT,
        proxy_url,
    )
    .await?;
    let status = resp.status().as_u16();
    let bytes = crate::openai::response_bytes(resp, Some(crate::limits::MAX_BODY)).await?;
    if !(200..300).contains(&status) {
        return Err(UpstreamError::Status {
            status,
            body: bytes,
            retry_after_secs: None,
        });
    }
    parse_tokens(&bytes)
}

/// 账号接口（换码、刷新、`/api/oauth/usage`）照真机发：CLI 在这里用 axios，握手是另一套
/// （`client_tls::Shape::Account`）；头的取舍与顺序按 2.1.293 第一方抓包——`Accept` 打头，调用方的头，
/// 有体时 `Content-Length`，最后 `Accept-Encoding`、`Host`、`Connection: close`（axios 每次新开连接）。
/// 响应按 `Content-Encoding` 由 `client_tls` 解压（`compress` 上游实际不用）。
pub(crate) async fn send_account(
    http: &crate::http::HttpPool,
    method: reqwest::Method,
    url: reqwest::Url,
    headers: &[(&str, &str)],
    body: Option<String>,
    timeout: Duration,
    proxy_url: Option<&str>,
) -> Result<reqwest::Response, UpstreamError> {
    let host = match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => return Err(UpstreamError::Build("account_url_host".into())),
    };
    let mut request = http
        .probe(&token_outbound(proxy_url), method, url)?
        .timeout(timeout)
        .header("accept", "application/json, text/plain, */*");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if let Some(body) = &body {
        request = request.header("content-length", body.len());
    }
    request = request
        .header("accept-encoding", "gzip, compress, deflate, br")
        .header("host", host)
        .header("connection", "close");
    if let Some(body) = body {
        request = request.body(body);
    }
    let request = request
        .build()
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    http.send_claude_account(request, proxy_url).await
}

/// 把 Anthropic Messages 请求体改成订阅路径要求的形状：system 首元素前置自述句（字符串 system
/// 转数组），已是首句则不重复。
pub fn prepare_body(body: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    let prefix = json!({"type": "text", "text": SYSTEM_PREFIX});
    let mut system: Vec<Value> = match obj.remove("system") {
        Some(Value::String(s)) if s.trim().is_empty() => Vec::new(),
        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    };
    let already = system
        .first()
        .and_then(|b| b.get("text").and_then(Value::as_str))
        .is_some_and(|t| t.starts_with(SYSTEM_PREFIX));
    if !already {
        system.insert(0, prefix);
    }
    obj.insert("system".to_owned(), Value::Array(system));
    serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 合并 beta 头：必备三项在前、用户自带的保留、去重。
#[must_use]
pub fn merge_beta(existing: Option<&str>) -> String {
    merge_with(&REQUIRED_BETAS, existing)
}

fn merge_with(required: &[&str], existing: Option<&str>) -> String {
    let mut parts: Vec<&str> = required.to_vec();
    for p in existing
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !parts.contains(&p) {
            parts.push(p);
        }
    }
    parts.join(",")
}

/// 客户端（经透传进 `extra_headers`）自带的 `anthropic-beta` 并进必备集合，并从透传头里摘掉——
/// reqwest 的 `header()` 是追加不是覆盖，留着会发出两行同名头。
fn take_beta(
    outbound: &crate::http::Outbound,
    client_betas_complete: bool,
) -> (crate::http::Outbound, String) {
    let mut outbound = outbound.clone();
    let client_beta = outbound
        .extra_headers
        .iter()
        .position(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
        .map(|i| outbound.extra_headers.remove(i).1);
    let merged = if client_betas_complete && client_beta.is_some() {
        // Preserve observed main/auxiliary sets; authentication adds OAuth, not client features.
        let mut parts: Vec<&str> = client_beta
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.contains(&"oauth-2025-04-20") {
            parts.insert(
                usize::from(parts.first() == Some(&"claude-code-20250219")),
                "oauth-2025-04-20",
            );
        }
        parts.join(",")
    } else {
        merge_beta(client_beta.as_deref())
    };
    (outbound, merged)
}

/// 用订阅 access token 发一次 Messages（`body` 已是 Anthropic 形状，`prepare_body` 在此内部完成）。
/// 渠道配置了客户端扩展（`extensions.client_profile`）时由 [`crate::profiles`] 整形请求，
/// `account_id`（凭证里的账号 UUID）进入模拟身份；未配置时保持透传形态。
pub async fn messages(
    http: &crate::http::HttpPool,
    api_base: &str,
    access_token: &str,
    body: Bytes,
    stream: bool,
    outbound: &crate::http::Outbound,
    account_id: Option<&str>,
) -> Result<MessagesResponse, UpstreamError> {
    let url = format!("{}/messages?beta=true", api_base.trim_end_matches('/'));
    let prepared = crate::profiles::prepare_anthropic(body, stream, false, outbound, account_id)?;
    let shaped = prepared.shaped;
    let body = if shaped {
        prepared.body
    } else {
        Bytes::from(prepare_body(&prepared.body)?)
    };
    let bearer = format!("Bearer {access_token}");
    let (outbound, client_beta) = take_beta(&prepared.outbound, prepared.client_betas_complete);
    let beta = if shaped {
        client_beta
    } else {
        merge_beta(Some(client_beta.as_str()))
    };
    let mut headers: Vec<(String, String)> = vec![
        ("authorization".to_owned(), bearer),
        ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
        ("anthropic-beta".to_owned(), beta),
    ];
    headers.extend(prepared.headers);
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    send_messages_at(http, url, &header_refs, body, stream, &outbound).await
}

/// `count_tokens`（同样要 Bearer + beta 头；系统提示不影响计数语义，照样前置以通过校验）。
pub async fn count_tokens(
    http: &crate::http::HttpPool,
    api_base: &str,
    access_token: &str,
    body: Bytes,
    outbound: &crate::http::Outbound,
    account_id: Option<&str>,
) -> Result<Bytes, UpstreamError> {
    let url = format!(
        "{}/messages/count_tokens?beta=true",
        api_base.trim_end_matches('/')
    );
    let prepared = crate::profiles::prepare_anthropic(body, false, true, outbound, account_id)?;
    let shaped = prepared.shaped;
    let body = if shaped {
        prepared.body
    } else {
        Bytes::from(prepare_body(&prepared.body)?)
    };
    let (outbound, client_beta) = take_beta(&prepared.outbound, prepared.client_betas_complete);
    let beta = if shaped && prepared.client_betas_complete {
        merge_with(&[], Some(&format!("{client_beta},{BETA_TOKEN_COUNTING}")))
    } else if shaped {
        merge_beta(Some(&format!("{client_beta},{BETA_TOKEN_COUNTING}")))
    } else {
        merge_beta(Some(client_beta.as_str()))
    };
    let mut req = http
        .post(&outbound, url)?
        .header("authorization", format!("Bearer {access_token}"))
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", beta)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_mins(2))
        .body(body);
    for (k, v) in prepared.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = http.send(req, &outbound).await?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after_secs = crate::retry_after::seconds(resp.headers());
        let body = crate::openai::response_bytes(resp, Some(crate::limits::MAX_ERROR))
            .await
            .unwrap_or_default();
        return Err(UpstreamError::Status {
            status,
            body,
            retry_after_secs,
        });
    }
    crate::openai::response_bytes(resp, Some(crate::limits::MAX_BODY)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_carries_pkce_and_required_params() {
        let pkce = Pkce::from_bytes(&[1u8; 32]);
        let url = authorize_url(&pkce);
        let parsed = reqwest::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.host_str(), Some("claude.com"));
        assert_eq!(parsed.path(), "/cai/oauth/authorize");
        assert_eq!(q["code"], "true");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["code_challenge"], pkce.challenge);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], pkce.verifier);
        assert_eq!(q["scope"], SCOPE);
        assert!(q["scope"].contains("user:inference"));
        assert_eq!(q["redirect_uri"], REDIRECT_URI);
        assert!(
            REDIRECT_URI.starts_with("https://platform.claude.com/")
                && TOKEN_URL.starts_with("https://platform.claude.com/"),
            "回调页与 token 端点跟随 CLI 迁到 platform.claude.com"
        );
    }

    #[test]
    fn pasted_code_splits_on_hash() {
        assert_eq!(split_pasted_code(" abc#st "), ("abc", Some("st")));
        assert_eq!(split_pasted_code("abc"), ("abc", None));
    }

    #[test]
    fn system_prefix_is_prepended_once_and_string_system_becomes_array() {
        let out = prepare_body(br#"{"model":"m","system":"be brief","messages":[]}"#).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let sys = v["system"].as_array().unwrap();
        assert_eq!(sys[0]["text"], SYSTEM_PREFIX);
        assert_eq!(sys[1]["text"], "be brief");

        let again = prepare_body(&out).unwrap();
        let v2: Value = serde_json::from_slice(&again).unwrap();
        assert_eq!(v2["system"].as_array().unwrap().len(), 2, "已前置不重复");

        let none = prepare_body(br#"{"model":"m","messages":[]}"#).unwrap();
        let v3: Value = serde_json::from_slice(&none).unwrap();
        assert_eq!(v3["system"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn beta_header_merging_dedups_and_keeps_user_flags() {
        let required = REQUIRED_BETAS.join(",");
        assert_eq!(merge_beta(None), required);
        assert_eq!(
            merge_beta(Some("interleaved-thinking-2025-05-14, oauth-2025-04-20")),
            required,
            "客户端重复给的必备项不重复"
        );
        assert_eq!(
            merge_beta(Some("context-1m-2025-08-07")),
            format!("{required},context-1m-2025-08-07"),
            "客户端自带的附加 beta 保留在后"
        );

        let outbound = crate::http::Outbound {
            proxy_url: None,
            extra_headers: vec![
                ("user-agent".to_owned(), "claude-cli/1.0".to_owned()),
                (
                    "Anthropic-Beta".to_owned(),
                    "context-1m-2025-08-07".to_owned(),
                ),
            ],
            ..Default::default()
        };
        let (stripped, beta) = take_beta(&outbound, false);
        assert_eq!(beta, format!("{required},context-1m-2025-08-07"));
        assert_eq!(
            stripped.extra_headers,
            vec![("user-agent".to_owned(), "claude-cli/1.0".to_owned())],
            "合并后透传头里不再有 anthropic-beta，避免发两行"
        );
    }

    #[test]
    fn modern_auxiliary_profile_adds_oauth_without_main_client_features() {
        let mut outbound = crate::Outbound::default();
        outbound.extra_headers.push((
            "anthropic-beta".into(),
            "interleaved-thinking-2025-05-14,structured-outputs-2025-12-15".into(),
        ));
        let (outbound, beta) = take_beta(&outbound, true);
        assert_eq!(
            beta,
            "oauth-2025-04-20,interleaved-thinking-2025-05-14,structured-outputs-2025-12-15"
        );
        assert!(outbound.extra_headers.is_empty());
        assert!(!beta.contains("claude-code-20250219"));
    }

    /// 收一个请求，把原始请求（头 + 体）当响应体回过去。
    async fn echo_once() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            while !raw.ends_with(b"\r\n\r\n") {
                raw.push(tcp.read_u8().await.unwrap());
            }
            let head = String::from_utf8(raw.clone()).unwrap();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .map_or(0, |v| v.parse::<usize>().unwrap());
            let mut body = vec![0u8; length];
            tcp.read_exact(&mut body).await.unwrap();
            raw.extend_from_slice(&body);
            let mut response =
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", raw.len()).into_bytes();
            response.extend_from_slice(&raw);
            tcp.write_all(&response).await.unwrap();
        });
        addr
    }

    /// 依次用 `replies` 应答，每条连接一个请求；把收到的请求体按序交回。
    async fn token_endpoint(
        replies: Vec<(u16, &'static str)>,
    ) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/oauth/token", listener.local_addr().unwrap());
        let (sent, received) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            for (status, reply) in replies {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(tcp.read_u8().await.unwrap());
                }
                let length = String::from_utf8_lossy(&head)
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .map_or(0, |v| v.parse::<usize>().unwrap());
                let mut body = vec![0u8; length];
                tcp.read_exact(&mut body).await.unwrap();
                sent.send(String::from_utf8(body).unwrap()).unwrap();
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\n\r\n{reply}",
                    reply.len()
                );
                tcp.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (url, received)
    }

    /// 字段与顺序照 CLI 源码（`grant_type, refresh_token, client_id, scope`，无 `expires_in`）；
    /// 旧 token 申请不到 `user:plugins` 时退回原授权 scope 再试一次。
    #[tokio::test]
    async fn refresh_asks_for_the_cli_scope_and_falls_back_on_invalid_scope() {
        let http = crate::http::HttpPool::new().unwrap();
        let (url, mut bodies) = token_endpoint(vec![
            (400, r#"{"error":"invalid_scope","error_description":"x"}"#),
            (
                200,
                r#"{"access_token":"a","refresh_token":"r2","expires_in":28800}"#,
            ),
        ])
        .await;
        let tokens = refresh(&http, &url, "r1", None, None).await.unwrap();
        assert_eq!(tokens.access_token, "a");
        assert_eq!(
            bodies.recv().await.unwrap(),
            format!(
                r#"{{"grant_type":"refresh_token","refresh_token":"r1","client_id":"{CLIENT_ID}","scope":"{REFRESH_SCOPE}"}}"#
            )
        );
        assert_eq!(
            bodies.recv().await.unwrap(),
            format!(
                r#"{{"grant_type":"refresh_token","refresh_token":"r1","client_id":"{CLIENT_ID}","scope":"{GRANTED_SCOPE_BEFORE_PLUGINS}"}}"#
            )
        );

        // 别的 400 不重试
        let (url, mut bodies) = token_endpoint(vec![(400, r#"{"error":"invalid_grant"}"#)]).await;
        assert!(matches!(
            refresh(&http, &url, "r1", None, None).await,
            Err(UpstreamError::Status { status: 400, .. })
        ));
        bodies.recv().await.unwrap();
        assert!(bodies.recv().await.is_none());

        // 记录过的旧 scope 照原样退回
        let (url, mut bodies) = token_endpoint(vec![
            (400, r#"{"error":"invalid_scope"}"#),
            (200, r#"{"access_token":"a","expires_in":28800}"#),
        ])
        .await;
        refresh(&http, &url, "r1", Some("user:profile user:inference"), None)
            .await
            .unwrap();
        bodies.recv().await.unwrap();
        assert!(
            bodies
                .recv()
                .await
                .unwrap()
                .contains(r#""scope":"user:profile user:inference""#)
        );
    }

    /// 已授过 `user:plugins` 的 token 被拒 invalid_scope 是上游误报：不降级重试，免得新 token 永久丢掉 plugins。
    #[tokio::test]
    async fn refresh_never_downgrades_a_token_that_already_holds_plugins() {
        let http = crate::http::HttpPool::new().unwrap();
        let (url, mut bodies) = token_endpoint(vec![(400, r#"{"error":"invalid_scope"}"#)]).await;
        assert!(matches!(
            refresh(&http, &url, "r1", Some(REFRESH_SCOPE), None).await,
            Err(UpstreamError::Status { status: 400, .. })
        ));
        bodies.recv().await.unwrap();
        assert!(bodies.recv().await.is_none(), "只发了一次刷新");
    }

    #[test]
    fn token_response_scope_is_recorded() {
        let tokens = crate::oauth::parse_tokens(
            br#"{"access_token":"a","expires_in":1,"scope":"user:inference user:plugins"}"#,
        )
        .unwrap();
        assert_eq!(tokens.scope.as_deref(), Some("user:inference user:plugins"));
    }

    #[tokio::test]
    async fn exchange_body_keeps_the_cli_field_order() {
        let http = crate::http::HttpPool::new().unwrap();
        let (url, mut bodies) =
            token_endpoint(vec![(200, r#"{"access_token":"a","expires_in":28800}"#)]).await;
        exchange(&http, &url, "c", "v", None).await.unwrap();
        assert_eq!(
            bodies.recv().await.unwrap(),
            format!(
                r#"{{"grant_type":"authorization_code","code":"c","redirect_uri":"{REDIRECT_URI}","client_id":"{CLIENT_ID}","code_verifier":"v","state":"v"}}"#
            )
        );
    }

    /// 与 2.1.293 第一方抓包逐行对照。
    #[tokio::test]
    async fn account_requests_go_out_like_the_cli_axios_calls() {
        let http = crate::http::HttpPool::new().unwrap();
        let addr = echo_once().await;
        let body = r#"{"grant_type":"refresh_token"}"#;
        let response = send_account(
            &http,
            reqwest::Method::POST,
            reqwest::Url::parse(&format!("http://{addr}/v1/oauth/token")).unwrap(),
            &[
                ("content-type", "application/json"),
                ("user-agent", "axios/1.15.2"),
            ],
            Some(body.to_owned()),
            TOKEN_TIMEOUT,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            response.text().await.unwrap(),
            format!(
                "POST /v1/oauth/token HTTP/1.1\r\nAccept: application/json, text/plain, */*\r\n\
                 Content-Type: application/json\r\nUser-Agent: axios/1.15.2\r\n\
                 Content-Length: {}\r\nAccept-Encoding: gzip, compress, deflate, br\r\n\
                 Host: {addr}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        );

        let addr = echo_once().await;
        let response = send_account(
            &http,
            reqwest::Method::GET,
            reqwest::Url::parse(&format!("http://{addr}/api/oauth/usage")).unwrap(),
            &[
                ("authorization", "Bearer t"),
                ("anthropic-beta", OAUTH_BETA),
                ("user-agent", "claude-code/2.1.296"),
            ],
            None,
            TOKEN_TIMEOUT,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            response.text().await.unwrap(),
            format!(
                "GET /api/oauth/usage HTTP/1.1\r\nAccept: application/json, text/plain, */*\r\n\
                 Authorization: Bearer t\r\nanthropic-beta: oauth-2025-04-20\r\n\
                 User-Agent: claude-code/2.1.296\r\n\
                 Accept-Encoding: gzip, compress, deflate, br\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
            )
        );
    }
}
