//! 自用订阅凭证的 OAuth 流程（IMPLEMENTATION §11.38，实验性）。
//!
//! 只有两家有足够公开资料可以做实：Anthropic（Claude Pro/Max，`anthropic_max`）与 OpenAI Codex
//! （ChatGPT 订阅，`codex`）。两家的 client_id / 端点都是官方客户端私有的、随时会变。
//!
//! 出向形态分两档：缺省**透传**——只发上游为这条路径要求的东西，不做设备指纹、不编造
//! User-Agent，真实客户端自带的身份头由 gateway 透传进 `Outbound.extra_headers`（2026-09 对照
//! Sub2API / CLIProxyAPI 核对过要求项）；渠道配置 `extensions.client_profile` 后由
//! [`crate::profiles`] 按最新抓包的官方 CLI 整形，[`crate::profiles::identity`] 提供跨请求稳定的身份。前者给自用；
//! 后者是对抗性工程，需跟随上游检测与官方 CLI 版本持续维护。
//!
//! 共用件：PKCE（S256）、授权 URL 拼装、token 响应形状；两家的差异在各自子模块。

pub mod anthropic_max;
pub mod codex;
pub mod quota;

use crate::error::UpstreamError;
use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// PKCE 一对：verifier（随机 base64url）与 challenge（`base64url(sha256(verifier))`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// 从 32 字节随机数派生（调用方给随机源，便于测试）。
    #[must_use]
    pub fn from_bytes(random: &[u8; 32]) -> Self {
        let verifier = base64url(random);
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }

    pub fn generate() -> Result<Self, UpstreamError> {
        let mut bytes = [0u8; 32];
        aws_lc_rs::rand::fill(&mut bytes)
            .map_err(|_| UpstreamError::Build("pkce_random".into()))?;
        Ok(Self::from_bytes(&bytes))
    }
}

pub(crate) fn base64url(input: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

/// 换码 / 刷新拿到的 token 三件套（`refresh_token` 可能不轮转，None = 沿用旧的）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    /// codex：从 id_token 取到的 ChatGPT 账号 id；anthropic_max：换码响应里的
    /// `account.uuid` / `organization.uuid`（全伪装的 metadata.user_id 用，常为 None）。
    pub account_id: Option<String>,
    /// 账号邮箱，仅供控制台展示：codex 取 id_token 的 `email`，anthropic_max 取
    /// 换码响应的 `account.email_address`。刷新响应通常不带，沿用旧值。
    pub account_label: Option<String>,
    /// token 响应的 `scope`（已授权范围，空格分隔）；没给就是 None。
    pub scope: Option<String>,
}

/// 通用 token 响应解析：`access_token` 必有，`expires_in` 缺省 1h。
pub(crate) fn parse_tokens(body: &[u8]) -> Result<Tokens, UpstreamError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| UpstreamError::Stream("oauth_token_shape".to_owned()))?;
    let access_token = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UpstreamError::Stream("oauth_token_shape".to_owned()))?
        .to_owned();
    Ok(Tokens {
        access_token,
        refresh_token: v
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        expires_in: v.get("expires_in").and_then(Value::as_i64).unwrap_or(3600),
        account_id: v
            .get("id_token")
            .and_then(Value::as_str)
            .and_then(codex::account_id_from_id_token)
            .or_else(|| {
                // Anthropic 换码响应的账号标识：account.uuid 优先，organization.uuid 兜底
                ["account", "organization"].iter().find_map(|k| {
                    v.get(*k)?
                        .get("uuid")?
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                })
            }),
        scope: v
            .get("scope")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        account_label: v
            .get("id_token")
            .and_then(Value::as_str)
            .and_then(codex::email_from_id_token)
            .or_else(|| {
                v.get("account")?
                    .get("email_address")?
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            }),
    })
}

/// 刷新失败的分类：`invalid_grant`（refresh token 已失效 / 被吊销）与其它瞬态错误要分开——
/// 前者重试无意义，key 该进 invalid；后者保留旧 token 下次再试。
/// 三个 `refresh_token_*` 是 auth.openai.com 的细分码（codex-rs 同样按终态处理）。
#[must_use]
pub fn is_invalid_grant(status: u16, body: &[u8]) -> bool {
    if status == 401 {
        return true;
    }
    let v: Value = serde_json::from_slice(body).unwrap_or_default();
    let code = v
        .get("error")
        .and_then(|e| {
            e.as_str().or_else(|| {
                e.get("code")
                    .or_else(|| e.get("type"))
                    .and_then(Value::as_str)
            })
        })
        .unwrap_or_default();
    matches!(
        code,
        "invalid_grant"
            | "invalid_request"
            | "unauthorized_client"
            | "refresh_token_expired"
            | "refresh_token_reused"
            | "refresh_token_invalidated"
    )
}

/// token 端点的出站修饰：走渠道自己的代理（刷新与 API 请求得从同一个出口出去，
/// 只有代理能出网的部署也才刷得动），但不带渠道给上游 API 配的额外头。
pub(crate) fn token_outbound(proxy_url: Option<&str>) -> crate::http::Outbound {
    crate::http::Outbound {
        proxy_url: proxy_url.map(str::to_owned),
        extra_headers: Vec::new(),
        ..Default::default()
    }
}

/// 表单编码（RFC 3986 非保留字符不编，与 `aws_sigv4::uri_encode` 同规则）。
pub(crate) fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                crate::aws_sigv4::uri_encode(k),
                crate::aws_sigv4::uri_encode(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let pkce = Pkce::from_bytes(&[7u8; 32]);
        assert_eq!(
            pkce.verifier.len(),
            43,
            "32 字节 base64url 无填充 = 43 字符"
        );
        assert_eq!(
            pkce.challenge,
            base64url(&Sha256::digest(pkce.verifier.as_bytes()))
        );
        assert!(!pkce.verifier.contains('=') && !pkce.verifier.contains('+'));
        assert_ne!(
            Pkce::generate().unwrap().verifier,
            Pkce::generate().unwrap().verifier
        );
    }

    #[test]
    fn account_label_comes_from_exchange_response() {
        let t = parse_tokens(
            br#"{"access_token":"a","account":{"uuid":"u-1","email_address":"me@example.com"},"organization":{"uuid":"o-1"}}"#,
        )
        .unwrap();
        assert_eq!(t.account_id.as_deref(), Some("u-1"));
        assert_eq!(t.account_label.as_deref(), Some("me@example.com"));
        // id_token payload {"email":"dev@example.com"}; signature is irrelevant for display.
        let id_token = format!(
            "x.{}.y",
            base64url(br#"{"email":"dev@example.com","https://api.openai.com/auth":{"chatgpt_account_id":"acct"}}"#)
        );
        let body = serde_json::json!({"access_token": "a", "id_token": id_token}).to_string();
        let t = parse_tokens(body.as_bytes()).unwrap();
        assert_eq!(t.account_id.as_deref(), Some("acct"));
        assert_eq!(t.account_label.as_deref(), Some("dev@example.com"));
        assert!(
            parse_tokens(br#"{"access_token":"a"}"#)
                .unwrap()
                .account_label
                .is_none()
        );
    }

    #[test]
    fn token_shape_and_defaults() {
        let t = parse_tokens(br#"{"access_token":"a","refresh_token":"r","expires_in":28800}"#)
            .unwrap();
        assert_eq!(
            t,
            Tokens {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_in: 28800,
                account_id: None,
                account_label: None,
                scope: None,
            }
        );
        let t = parse_tokens(br#"{"access_token":"a"}"#).unwrap();
        assert_eq!(t.expires_in, 3600);
        assert!(t.refresh_token.is_none());
        assert!(parse_tokens(br#"{"token_type":"Bearer"}"#).is_err());
    }

    #[test]
    fn invalid_grant_detection() {
        assert!(is_invalid_grant(400, br#"{"error":"invalid_grant"}"#));
        assert!(is_invalid_grant(
            400,
            br#"{"error":{"type":"invalid_grant","message":"x"}}"#
        ));
        assert!(is_invalid_grant(401, b""));
        assert!(is_invalid_grant(
            400,
            br#"{"error":{"code":"refresh_token_reused","message":"x"}}"#
        ));
        assert!(!is_invalid_grant(500, b"upstream down"));
        assert!(!is_invalid_grant(429, br#"{"error":"rate_limited"}"#));
    }

    #[test]
    fn form_encoding() {
        assert_eq!(
            form_encode(&[("grant_type", "refresh_token"), ("x", "a b:c")]),
            "grant_type=refresh_token&x=a%20b%3Ac"
        );
    }
}
