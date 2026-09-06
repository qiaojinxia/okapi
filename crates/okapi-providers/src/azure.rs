//! Azure OpenAI 上游客户端（IMPLEMENTATION §11.29）。
//!
//! 与官方 OpenAI 同一请求 / 响应形态，只有三处不同：
//! - 鉴权：`api-key: {credential}` 头，而非 `Authorization: Bearer`；
//! - 路径：按**部署**寻址 `{endpoint}/openai/deployments/{deployment}/chat/completions`，
//!   部署名来自渠道 `model_mapping`（规范模型名 → 部署名），未映射时用模型名本身；
//! - 版本：每个请求必带 `?api-version=...` 查询参数（渠道 `settings.api_version`，缺省见
//!   [`DEFAULT_API_VERSION`]）。
//!
//! 因此本模块只负责 URL 与鉴权头的拼装，发送与响应解析复用 [`OpenAiUpstream`] 的
//! `send_*` 内部方法（同一 reqwest client，连接池共享），不复制任何流解析逻辑。

use crate::error::UpstreamError;
use crate::http::Outbound;
use crate::openai::{ChatResponse, EmbeddingsResponse, OpenAiUpstream};
use bytes::Bytes;

/// 渠道未配置 `settings.api_version` 时的缺省：2024-10-21 是当前 GA 数据面版本
/// （chat / embeddings / images / audio 全覆盖）。站长要用预览特性时显式配置。
pub const DEFAULT_API_VERSION: &str = "2024-10-21";

/// 资源端点规范化：站长常把 `https://{res}.openai.azure.com/openai` 或
/// `.../openai/v1` 整段贴进 api_base，这里统一剥到资源根，避免拼出 `/openai/openai/...`。
pub fn normalize_endpoint(api_base: &str) -> &str {
    let base = api_base.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    base.strip_suffix("/openai").unwrap_or(base)
}

/// 部署级 URL：`{endpoint}/openai/deployments/{deployment}{path}?api-version={v}`。
/// 部署名只允许 Azure 命名规则内的字符（字母 / 数字 / `-` / `_` / `.`），其余按 RFC 3986
/// 百分号转义，防止 `/` 或 `?` 把路径拆散。
pub fn deployment_url(endpoint: &str, deployment: &str, path: &str, api_version: &str) -> String {
    format!(
        "{}/openai/deployments/{}{path}?api-version={}",
        normalize_endpoint(endpoint),
        percent_encode(deployment),
        percent_encode(api_version),
    )
}

fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Azure OpenAI 上游：薄封装，持有共享的 [`OpenAiUpstream`]。
#[derive(Clone)]
pub struct AzureUpstream {
    inner: OpenAiUpstream,
}

impl AzureUpstream {
    /// 与 OpenAI 客户端共用 reqwest client（gateway 只维护一个连接池）。
    pub fn new(inner: OpenAiUpstream) -> Self {
        Self { inner }
    }

    fn post(
        &self,
        endpoint: &str,
        deployment: &str,
        path: &str,
        api_version: &str,
        credential: &str,
        outbound: &Outbound,
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        let url = deployment_url(endpoint, deployment, path, api_version);
        Ok(self
            .inner
            .http
            .post(outbound, url)?
            .header("api-key", credential))
    }

    /// chat completions：`body` 的 `model` 已被重写为部署名（Azure 忽略该字段，但保持一致）。
    #[allow(clippy::too_many_arguments)]
    pub async fn chat(
        &self,
        endpoint: &str,
        api_version: &str,
        deployment: &str,
        credential: &str,
        body: Bytes,
        stream: bool,
        outbound: &Outbound,
    ) -> Result<ChatResponse, UpstreamError> {
        let req = self.post(
            endpoint,
            deployment,
            "/chat/completions",
            api_version,
            credential,
            outbound,
        )?;
        self.inner.send_chat(req, body, stream).await
    }

    /// 非流式 JSON 端点（embeddings / images/generations 等），`path` 为部署之后的相对路径。
    #[allow(clippy::too_many_arguments)]
    pub async fn json_relay(
        &self,
        endpoint: &str,
        api_version: &str,
        deployment: &str,
        path: &str,
        credential: &str,
        body: Bytes,
        outbound: &Outbound,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        let req = self.post(
            endpoint,
            deployment,
            path,
            api_version,
            credential,
            outbound,
        )?;
        self.inner.send_json(req, body).await
    }

    /// audio/speech：JSON 入、二进制音频出。
    pub async fn speech(
        &self,
        endpoint: &str,
        api_version: &str,
        deployment: &str,
        credential: &str,
        body: Bytes,
        outbound: &Outbound,
    ) -> Result<(u16, String, Bytes), UpstreamError> {
        let req = self.post(
            endpoint,
            deployment,
            "/audio/speech",
            api_version,
            credential,
            outbound,
        )?;
        self.inner.send_speech(req, body).await
    }

    /// audio/transcriptions | translations：multipart 入、JSON 出。
    #[allow(clippy::too_many_arguments)]
    pub async fn audio_multipart(
        &self,
        endpoint: &str,
        api_version: &str,
        deployment: &str,
        path: &str,
        credential: &str,
        parts: Vec<(String, Option<String>, Option<String>, Bytes)>,
        outbound: &Outbound,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        let req = self.post(
            endpoint,
            deployment,
            path,
            api_version,
            credential,
            outbound,
        )?;
        self.inner.send_multipart(req, parts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalizes_pasted_openai_suffixes() {
        assert_eq!(
            normalize_endpoint("https://r.openai.azure.com/"),
            "https://r.openai.azure.com"
        );
        assert_eq!(
            normalize_endpoint("https://r.openai.azure.com/openai/"),
            "https://r.openai.azure.com"
        );
        assert_eq!(
            normalize_endpoint("https://r.openai.azure.com/openai/v1"),
            "https://r.openai.azure.com"
        );
    }

    #[test]
    fn deployment_url_has_version_and_escapes() {
        assert_eq!(
            deployment_url(
                "https://r.openai.azure.com/openai",
                "gpt-4o_prod.v2",
                "/chat/completions",
                "2024-10-21"
            ),
            "https://r.openai.azure.com/openai/deployments/gpt-4o_prod.v2/chat/completions?api-version=2024-10-21"
        );
        assert_eq!(
            deployment_url(
                "https://r.openai.azure.com",
                "a/b?c",
                "/embeddings",
                "2025-01-01-preview"
            ),
            "https://r.openai.azure.com/openai/deployments/a%2Fb%3Fc/embeddings?api-version=2025-01-01-preview"
        );
    }
}
