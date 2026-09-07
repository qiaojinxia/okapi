//! ChatGPT 订阅经 Codex CLI 公开客户端的 OAuth（IMPLEMENTATION §11.38，实验性）。
//!
//! 后端只有 Responses 一种面（`https://chatgpt.com/backend-api/codex/responses`），请求头比官方
//! API 多两项：`chatgpt-account-id`（从 id_token 的 claim 取）与 `originator`。事件形状与官方
//! Responses 一致，传输直接复用 `responses::send_responses_at`。

use super::{Pkce, Tokens, form_encode, parse_tokens};
use crate::error::UpstreamError;
use crate::openai::ChatResponse;
use crate::openai::classify;
use base64::Engine as _;
use bytes::Bytes;
use serde_json::{Value, json};
use std::time::Duration;

/// Codex CLI 公开客户端 id（OpenAI 私有，可能变更）。
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Codex CLI 的本地回调地址；我们不监听它，站长从浏览器地址栏把 `code` 贴回来。
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
pub const ORIGINATOR: &str = "codex_cli_rs";
pub const DEFAULT_API_BASE: &str = "https://chatgpt.com/backend-api/codex";
const TOKEN_TIMEOUT: Duration = Duration::from_secs(20);

/// 授权 URL：`state` 独立随机值（与 Anthropic 不同，这里不复用 verifier）。
#[must_use]
pub fn authorize_url(pkce: &Pkce, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?{}",
        form_encode(&[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", ORIGINATOR),
        ])
    )
}

/// 站长可能把整个回调 URL 贴回来：从中取 `code`（与 `state`）。
#[must_use]
pub fn split_pasted_code(pasted: &str) -> (String, Option<String>) {
    let pasted = pasted.trim();
    if let Ok(url) = reqwest::Url::parse(pasted) {
        let mut code = None;
        let mut state = None;
        for (k, v) in url.query_pairs() {
            match &*k {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                _ => {}
            }
        }
        if let Some(code) = code {
            return (code, state);
        }
    }
    (pasted.to_owned(), None)
}

/// `id_token`（JWT）里的 ChatGPT 账号 id：`https://api.openai.com/auth`.`chatgpt_account_id`。
/// 只解码不验签——它来自我们自己刚向 token 端点换来的响应。
#[must_use]
pub fn account_id_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 换码（表单体，与 Codex CLI 一致）。
pub async fn exchange(
    http: &crate::http::HttpPool,
    token_url: &str,
    code: &str,
    verifier: &str,
) -> Result<Tokens, UpstreamError> {
    let form = form_encode(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ]);
    let resp = http
        .post(&crate::http::Outbound::default(), token_url)?
        .timeout(TOKEN_TIMEOUT)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form)
        .send()
        .await
        .map_err(|e| classify(&e))?;
    read_tokens(resp).await
}

/// 刷新（JSON 体，与 openai/codex 源码一致）。
pub async fn refresh(
    http: &crate::http::HttpPool,
    token_url: &str,
    refresh_token: &str,
) -> Result<Tokens, UpstreamError> {
    let resp = http
        .post(&crate::http::Outbound::default(), token_url)?
        .timeout(TOKEN_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            json!({
                "client_id": CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            })
            .to_string(),
        )
        .send()
        .await
        .map_err(|e| classify(&e))?;
    read_tokens(resp).await
}

async fn read_tokens(resp: reqwest::Response) -> Result<Tokens, UpstreamError> {
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

/// Responses 请求体改成 Codex 后端接受的形状：`store` 强制 false（该后端不持久化 response）。
pub fn prepare_body(body: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    obj.insert("store".to_owned(), Value::Bool(false));
    serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 用订阅 access token 发一次 Responses。
pub async fn responses(
    http: &crate::http::HttpPool,
    api_base: &str,
    access_token: &str,
    account_id: Option<&str>,
    body: Bytes,
    stream: bool,
    outbound: &crate::http::Outbound,
) -> Result<ChatResponse, UpstreamError> {
    let url = format!("{}/responses", api_base.trim_end_matches('/'));
    let body = Bytes::from(prepare_body(&body)?);
    let bearer = format!("Bearer {access_token}");
    let mut headers: Vec<(&str, &str)> = vec![
        ("authorization", bearer.as_str()),
        ("originator", ORIGINATOR),
        ("openai-beta", "responses=experimental"),
    ];
    if let Some(id) = account_id {
        headers.push(("chatgpt-account-id", id));
    }
    crate::responses::send_responses_at(http, url, &headers, body, stream, outbound).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::base64url;

    fn jwt_with_payload(payload: &str) -> String {
        format!(
            "{}.{}.sig",
            base64url(br#"{"alg":"RS256"}"#),
            base64url(payload.as_bytes())
        )
    }

    #[test]
    fn authorize_url_has_codex_specific_params() {
        let pkce = Pkce::from_bytes(&[2u8; 32]);
        let parsed = reqwest::Url::parse(&authorize_url(&pkce, "st-1")).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.host_str(), Some("auth.openai.com"));
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["code_challenge"], pkce.challenge);
        assert_eq!(q["state"], "st-1");
        assert_eq!(q["id_token_add_organizations"], "true");
        assert_eq!(q["codex_cli_simplified_flow"], "true");
        assert_eq!(q["originator"], ORIGINATOR);
        assert!(q["scope"].contains("offline_access"));
    }

    #[test]
    fn pasted_callback_url_yields_code_and_state() {
        let (code, state) = split_pasted_code(
            "http://localhost:1455/auth/callback?code=abc&state=xyz&scope=openid",
        );
        assert_eq!((code.as_str(), state.as_deref()), ("abc", Some("xyz")));
        let (code, state) = split_pasted_code("  rawcode  ");
        assert_eq!((code.as_str(), state), ("rawcode", None));
    }

    #[test]
    fn account_id_comes_from_openai_auth_claim() {
        let jwt = jwt_with_payload(
            r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-xyz","chatgpt_plan_type":"plus"}}"#,
        );
        assert_eq!(account_id_from_id_token(&jwt).as_deref(), Some("acct-xyz"));
        assert!(account_id_from_id_token(&jwt_with_payload(r#"{"sub":"u"}"#)).is_none());
        assert!(account_id_from_id_token("not-a-jwt").is_none());
        // 经 parse_tokens 整体解析也能带出来
        let body = format!(
            r#"{{"access_token":"a","refresh_token":"r","expires_in":600,"id_token":"{jwt}"}}"#
        );
        assert_eq!(
            parse_tokens(body.as_bytes()).unwrap().account_id.as_deref(),
            Some("acct-xyz")
        );
    }

    #[test]
    fn prepare_body_forces_store_false() {
        let out = prepare_body(br#"{"model":"gpt-5","input":"hi","store":true}"#).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["store"], false);
        assert_eq!(v["model"], "gpt-5");
    }
}
