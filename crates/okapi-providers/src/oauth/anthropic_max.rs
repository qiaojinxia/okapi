//! Claude Pro/Max 订阅经 Claude Code 公开客户端的 OAuth（IMPLEMENTATION §11.38，实验性）。
//!
//! 与 `anthropic` 直连的差别只有四处：`Authorization: Bearer` 而非 `x-api-key`、URL 带
//! `?beta=true`、`anthropic-beta` 必含三个 Claude Code 标记、system 首元素须是 Claude Code 自述句。
//! 其余（SSE 事件、usage）完全一致，所以传输直接复用 `anthropic::send_messages_at`。
//!
//! 两种出向形态（`mimic` 参数选择）：
//! - `None`（缺省，透传）：只发上游为这条路径**要求**的东西，客户端身份头由 `Outbound.extra_headers`
//!   透传——前面站着真实 Claude Code / Codex CLI 时用这个；
//! - `Some(identity)`（`channels.settings.mimic_cc` 开启）：网关替客户端伪造完整 CLI 身份
//!   （全量 beta、`claude-cli` UA、x-stainless 套件、system billing 块、metadata.user_id），
//!   供非官方客户端走订阅额度；对抗性工程，见 [`super::cc_mimic`]。
//!
//! 端点与 scope 跟随 Claude Code CLI（2026-09 对照 Sub2API 与实测：旧 `console.anthropic.com`
//! 回调页已 301 到 `platform.claude.com`）。

use super::{Pkce, Tokens, cc_mimic, form_encode, parse_tokens, token_outbound};
use crate::anthropic::{ANTHROPIC_VERSION, MessagesResponse, classify, send_messages_at};
use crate::error::UpstreamError;
use bytes::Bytes;
use serde_json::{Value, json};
use std::time::Duration;

/// Claude Code 公开客户端 id（Anthropic 私有，可能变更）。
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// 手动回调页：浏览器授权后页面直接显示 `code#state`，站长贴回控制面。
pub const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
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
    post_token(
        http,
        token_url,
        json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": REDIRECT_URI,
            "client_id": CLIENT_ID,
            "code_verifier": verifier,
            "state": verifier,
        }),
        proxy_url,
    )
    .await
}

/// 刷新（JSON 体）。
pub async fn refresh(
    http: &crate::http::HttpPool,
    token_url: &str,
    refresh_token: &str,
    proxy_url: Option<&str>,
) -> Result<Tokens, UpstreamError> {
    post_token(
        http,
        token_url,
        json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": refresh_token,
        }),
        proxy_url,
    )
    .await
}

/// token 端点走不跟随重定向的探针 client：地址可被管理员覆写（`oauth_token_url`），
/// SSRF 闸只看得到填进来的那个 URL。
async fn post_token(
    http: &crate::http::HttpPool,
    token_url: &str,
    body: Value,
    proxy_url: Option<&str>,
) -> Result<Tokens, UpstreamError> {
    let resp = http
        .probe(&token_outbound(proxy_url), reqwest::Method::POST, token_url)?
        .timeout(TOKEN_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| classify(&e))?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| classify(&e))?;
    if !(200..300).contains(&status) {
        return Err(UpstreamError::Status {
            status,
            body: bytes,
            retry_after_secs: None,
        });
    }
    parse_tokens(&bytes)
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
fn take_beta(outbound: &crate::http::Outbound) -> (crate::http::Outbound, String) {
    let mut outbound = outbound.clone();
    let client_beta = outbound
        .extra_headers
        .iter()
        .position(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
        .map(|i| outbound.extra_headers.remove(i).1);
    let merged = merge_beta(client_beta.as_deref());
    (outbound, merged)
}

/// 用订阅 access token 发一次 Messages（`body` 已是 Anthropic 形状，`prepare_body` 在此内部完成）。
/// `mimic` = `Some` 时走全伪装（body 换 [`cc_mimic::prepare_body`]、beta 换全量、身份头伪造并
/// 摘掉透传头里的同键冲突），`None` 保持透传形态。
pub async fn messages(
    http: &crate::http::HttpPool,
    api_base: &str,
    access_token: &str,
    body: Bytes,
    stream: bool,
    outbound: &crate::http::Outbound,
    mimic: Option<&cc_mimic::MimicIdentity>,
) -> Result<MessagesResponse, UpstreamError> {
    let url = format!("{}/messages?beta=true", api_base.trim_end_matches('/'));
    let body = match mimic {
        Some(id) => Bytes::from(cc_mimic::prepare_body(&body, id)?),
        None => Bytes::from(prepare_body(&body)?),
    };
    let bearer = format!("Bearer {access_token}");
    let (mut outbound, client_beta) = take_beta(outbound);
    let beta = match mimic {
        Some(_) => cc_mimic::merge_full_betas(Some(client_beta.as_str())),
        None => merge_beta(Some(client_beta.as_str())),
    };
    if mimic.is_some() {
        outbound = cc_mimic::strip_forged_keys(outbound);
    }
    let mut headers: Vec<(String, String)> = vec![
        ("authorization".to_owned(), bearer),
        ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
        ("anthropic-beta".to_owned(), beta),
    ];
    if let Some(id) = mimic {
        headers.extend(cc_mimic::forge_headers(id, stream));
    }
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
    mimic: Option<&cc_mimic::MimicIdentity>,
) -> Result<Bytes, UpstreamError> {
    let url = format!(
        "{}/messages/count_tokens?beta=true",
        api_base.trim_end_matches('/')
    );
    let body = match mimic {
        Some(id) => Bytes::from(cc_mimic::prepare_body(&body, id)?),
        None => Bytes::from(prepare_body(&body)?),
    };
    let (mut outbound, client_beta) = take_beta(outbound);
    let beta = match mimic {
        Some(_) => cc_mimic::merge_count_betas(Some(client_beta.as_str())),
        None => merge_beta(Some(client_beta.as_str())),
    };
    if mimic.is_some() {
        outbound = cc_mimic::strip_forged_keys(outbound);
    }
    let mut req = http
        .post(&outbound, url)?
        .header("authorization", format!("Bearer {access_token}"))
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", beta)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_mins(2))
        .body(body);
    if let Some(id) = mimic {
        for (k, v) in cc_mimic::forge_headers(id, false) {
            req = req.header(k.as_str(), v.as_str());
        }
    }
    let resp = req.send().await.map_err(|e| classify(&e))?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| classify(&e))?;
    if !(200..300).contains(&status) {
        return Err(UpstreamError::Status {
            status,
            body: bytes,
            retry_after_secs: None,
        });
    }
    Ok(bytes)
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
        };
        let (stripped, beta) = take_beta(&outbound);
        assert_eq!(beta, format!("{required},context-1m-2025-08-07"));
        assert_eq!(
            stripped.extra_headers,
            vec![("user-agent".to_owned(), "claude-cli/1.0".to_owned())],
            "合并后透传头里不再有 anthropic-beta，避免发两行"
        );
    }
}
