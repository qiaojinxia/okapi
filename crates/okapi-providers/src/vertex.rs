//! Google Vertex AI 上游（IMPLEMENTATION §11.35）：服务账号 OAuth + 两条 publisher 路径。
//!
//! `claude*` 走 `publishers/anthropic/models/{m}:rawPredict | :streamRawPredict`（Anthropic Messages
//! 形状，版本字段换成 vertex 值），其余走 `publishers/google/models/{m}:generateContent |
//! :streamGenerateContent?alt=sse`（Gemini 形状）。传输层直接复用 anthropic / gemini 的
//! `send_*_at`，本模块只管换 token、拼 URL、改 body。
//!
//! 凭证 = 服务账号 JSON 原文：JWT RS256（scope cloud-platform，1h）经 `token_uri` 换 access token，
//! 进程内按凭证哈希缓存、到期前 5 分钟刷新、刷新单飞。不是 JSON 的凭证按现成 access token 直接
//! 作 Bearer（测试 / 外部 STS 侧车）。

use crate::anthropic::{MessagesResponse, classify, send_messages_at};
use crate::error::UpstreamError;
use crate::gemini::{GeminiResponse, send_generate_at};
use base64::Engine as _;
use bytes::Bytes;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// rawPredict 请求体的版本字段（Vertex 固定值）。
pub const ANTHROPIC_VERSION: &str = "vertex-2023-10-16";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const TOKEN_TIMEOUT: Duration = Duration::from_secs(20);
/// 到期前多久就刷新：一次长流式请求也不该撞上 token 过期。
const REFRESH_MARGIN_SECS: i64 = 300;
const JWT_TTL_SECS: i64 = 3600;

struct CachedToken {
    token: String,
    expires_at: i64,
}

#[derive(Clone)]
pub struct VertexUpstream {
    http: crate::http::HttpPool,
    tokens: Arc<tokio::sync::Mutex<HashMap<String, CachedToken>>>,
}

/// 该模型在 Vertex 上归 anthropic publisher（其余按 google / Gemini 形状）。
#[must_use]
pub fn is_anthropic_model(model: &str) -> bool {
    model.to_ascii_lowercase().starts_with("claude")
}

/// `api_base` 形状校验：须含 `/projects/{p}/locations/{l}`（管理面写入时调用）。
#[must_use]
pub fn api_base_ok(api_base: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(api_base) else {
        return false;
    };
    let path = url.path();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let pos = segs.iter().position(|s| *s == "projects");
    matches!(pos, Some(i) if segs.len() >= i + 4 && segs[i + 2] == "locations")
}

/// 服务账号 JSON 的三个必要字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccount {
    pub client_email: String,
    pub private_key_pem: String,
    pub token_uri: String,
}

impl ServiceAccount {
    /// 解析服务账号 JSON；不是 JSON / 缺字段返回 None（调用方按现成 token 处理）。
    #[must_use]
    pub fn parse(credential: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(credential.trim()).ok()?;
        Some(Self {
            client_email: v.get("client_email")?.as_str()?.to_owned(),
            private_key_pem: v.get("private_key")?.as_str()?.to_owned(),
            token_uri: v
                .get("token_uri")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_TOKEN_URI)
                .to_owned(),
        })
    }

    /// 签一枚 JWT-bearer 断言（RS256）。
    pub fn jwt_assertion(&self, now: i64) -> Result<String, UpstreamError> {
        let header = base64url(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = json!({
            "iss": self.client_email,
            "scope": SCOPE,
            "aud": self.token_uri,
            "iat": now,
            "exp": now + JWT_TTL_SECS,
        });
        let claims = base64url(claims.to_string().as_bytes());
        let signing_input = format!("{header}.{claims}");
        let der = pem_to_der(&self.private_key_pem)
            .ok_or_else(|| UpstreamError::Build("vertex_private_key_pem".to_owned()))?;
        let key_pair = aws_lc_rs::signature::RsaKeyPair::from_pkcs8(&der)
            .map_err(|_| UpstreamError::Build("vertex_private_key_pkcs8".to_owned()))?;
        let mut signature = vec![0u8; key_pair.public_modulus_len()];
        key_pair
            .sign(
                &aws_lc_rs::signature::RSA_PKCS1_SHA256,
                &aws_lc_rs::rand::SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| UpstreamError::Build("vertex_jwt_sign".to_owned()))?;
        Ok(format!("{signing_input}.{}", base64url(&signature)))
    }
}

fn base64url(input: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

/// `-----BEGIN PRIVATE KEY-----` PEM → DER（PKCS#8）。JSON 里的 `\n` 已由 serde 还原成换行。
fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("-----"))
        .collect();
    base64::engine::general_purpose::STANDARD.decode(body).ok()
}

impl VertexUpstream {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            http: crate::http::HttpPool::new()?,
            tokens: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
    }

    /// 取可用的 access token：服务账号凭证走缓存 / 刷新，其它凭证原样当 token。
    pub async fn access_token(
        &self,
        credential: &str,
        outbound: &crate::http::Outbound,
    ) -> Result<String, UpstreamError> {
        let Some(sa) = ServiceAccount::parse(credential) else {
            return Ok(credential.trim().to_owned());
        };
        let cache_key = hex::encode(Sha256::digest(credential.as_bytes()));
        let now = chrono::Utc::now().timestamp();
        // 持锁跨过刷新请求：同一凭证的并发请求只换一次 token（刷新是小时级事件，串行无妨）
        let mut cache = self.tokens.lock().await;
        if let Some(hit) = cache.get(&cache_key)
            && hit.expires_at - now > REFRESH_MARGIN_SECS
        {
            return Ok(hit.token.clone());
        }
        let assertion = sa.jwt_assertion(now)?;
        // 手拼表单：JWT 是 base64url（表单安全字符），grant_type 只有冒号要编码；省掉 reqwest form feature
        let form = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
        );
        let resp = self
            .http
            .post(outbound, sa.token_uri.as_str())?
            .timeout(TOKEN_TIMEOUT)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form)
            .send()
            .await
            .map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|e| classify(&e))?;
        if !(200..300).contains(&status) {
            return Err(UpstreamError::Status {
                status,
                body,
                retry_after_secs: None,
            });
        }
        let parsed: Value =
            serde_json::from_slice(&body).map_err(|e| UpstreamError::Stream(e.to_string()))?;
        let token = parsed
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| UpstreamError::Stream("vertex_token_shape".to_owned()))?
            .to_owned();
        let expires_in = parsed
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(JWT_TTL_SECS);
        cache.insert(
            cache_key,
            CachedToken {
                token: token.clone(),
                expires_at: now + expires_in,
            },
        );
        Ok(token)
    }

    /// Claude on Vertex：Anthropic Messages → rawPredict / streamRawPredict。
    pub async fn messages(
        &self,
        api_base: &str,
        credential: &str,
        model: &str,
        body: Bytes,
        stream: bool,
        outbound: &crate::http::Outbound,
    ) -> Result<MessagesResponse, UpstreamError> {
        let token = self.access_token(credential, outbound).await?;
        let action = if stream {
            "streamRawPredict"
        } else {
            "rawPredict"
        };
        let url = format!(
            "{}/publishers/anthropic/models/{model}:{action}",
            api_base.trim_end_matches('/')
        );
        let body = raw_predict_body(&body)?;
        let bearer = format!("Bearer {token}");
        send_messages_at(
            &self.http,
            url,
            &[("authorization", bearer.as_str())],
            Bytes::from(body),
            stream,
            outbound,
        )
        .await
    }

    /// Gemini on Vertex：generateContent / streamGenerateContent?alt=sse。
    pub async fn generate(
        &self,
        api_base: &str,
        credential: &str,
        model: &str,
        body: Bytes,
        stream: bool,
        outbound: &crate::http::Outbound,
    ) -> Result<GeminiResponse, UpstreamError> {
        let token = self.access_token(credential, outbound).await?;
        let base = api_base.trim_end_matches('/');
        let url = if stream {
            format!("{base}/publishers/google/models/{model}:streamGenerateContent?alt=sse")
        } else {
            format!("{base}/publishers/google/models/{model}:generateContent")
        };
        let bearer = format!("Bearer {token}");
        send_generate_at(
            &self.http,
            url,
            ("authorization", bearer.as_str()),
            body,
            stream,
            outbound,
        )
        .await
    }
}

/// Anthropic Messages → rawPredict 请求体：去 `model`（在 URL 上）、版本字段换成 vertex 值。
/// `stream` 保留：streamRawPredict 由它判定是否流式返回。
pub fn raw_predict_body(body: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    obj.remove("model");
    obj.insert(
        "anthropic_version".to_owned(),
        Value::String(ANTHROPIC_VERSION.to_owned()),
    );
    serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_routing_and_base_shape() {
        assert!(is_anthropic_model("claude-sonnet-4-5@20250929"));
        assert!(is_anthropic_model("Claude-3-haiku"));
        assert!(!is_anthropic_model("gemini-2.5-pro"));
        assert!(api_base_ok(
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p-1/locations/us-central1"
        ));
        assert!(api_base_ok(
            "https://aiplatform.googleapis.com/v1/projects/p-1/locations/global/"
        ));
        assert!(!api_base_ok(
            "https://aiplatform.googleapis.com/v1/projects/p-1"
        ));
        assert!(!api_base_ok(
            "https://generativelanguage.googleapis.com/v1beta"
        ));
        assert!(!api_base_ok("not a url"));
    }

    #[test]
    fn raw_predict_body_swaps_version_and_drops_model() {
        let out = raw_predict_body(
            br#"{"model":"claude-x","stream":true,"anthropic_version":"2023-06-01","messages":[]}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert!(v.get("model").is_none());
        assert_eq!(v["stream"], true);
        assert_eq!(v["anthropic_version"], ANTHROPIC_VERSION);
    }

    #[test]
    fn service_account_parse_and_non_json_fallback() {
        assert!(ServiceAccount::parse("ya29.plain-token").is_none());
        let sa = ServiceAccount::parse(
            r#"{"type":"service_account","client_email":"a@p.iam.gserviceaccount.com","private_key":"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"}"#,
        )
        .unwrap();
        assert_eq!(sa.client_email, "a@p.iam.gserviceaccount.com");
        assert_eq!(sa.token_uri, DEFAULT_TOKEN_URI);
        assert_eq!(pem_to_der(&sa.private_key_pem), Some(vec![0, 0, 0]));
        // 私钥不是合法 PKCS#8 → 构造错误而非 panic
        assert!(matches!(sa.jwt_assertion(0), Err(UpstreamError::Build(_))));
    }

    #[test]
    fn jwt_has_three_segments_with_rs256_header() {
        use aws_lc_rs::encoding::AsDer as _;
        // 2048 位测试私钥（PKCS#8）：只用于签名形状校验
        let key =
            aws_lc_rs::signature::RsaKeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
        let pkcs8 = key.as_der().unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref())
        );
        let sa = ServiceAccount {
            client_email: "svc@p.iam.gserviceaccount.com".to_owned(),
            private_key_pem: pem,
            token_uri: DEFAULT_TOKEN_URI.to_owned(),
        };
        let jwt = sa.jwt_assertion(1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[0])
            .unwrap();
        assert_eq!(header, br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], "svc@p.iam.gserviceaccount.com");
        assert_eq!(claims["aud"], DEFAULT_TOKEN_URI);
        assert_eq!(claims["exp"], 1_700_003_600);
        assert_eq!(claims["scope"], SCOPE);
        // 签名长度 = 模数长度（2048 位 → 256 字节）
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap();
        assert_eq!(sig.len(), 256);
    }
}
