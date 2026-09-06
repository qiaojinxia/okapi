//! OpenAI 方言出向的统一入口：按渠道 provider 分派到官方 / 兼容（Bearer）或 Azure
//! （部署 URL + `api-key` + `api-version`）客户端。
//!
//! 七个 OpenAI 形态端点（chat / embeddings / images / speech / transcriptions /
//! translations / rerank 等中继）此前各自直接调 `state.upstream.*`；Azure 的差异只在
//! URL 与鉴权头，与各端点的计费 / 重试 / 转换逻辑无关，所以收口在这里分派一次，
//! 端点代码只换调用名，不感知 provider。
//!
//! Azure 渠道必须配置 api_base（资源端点因资源而异，没有可猜的缺省；管理面建 / 改渠道
//! 时已校验），运行期缺失按构造错误处理而非回退到 api.openai.com——那会把 Azure 的
//! `api-key` 头打到 OpenAI 上，返回 401 后触发 key 失效冷却，比直接失败更糟。

use super::state::AppState;
use bytes::Bytes;
use okapi_providers::UpstreamError;
use okapi_providers::openai::EmbeddingsResponse;
use okapi_providers::{ChatResponse, Outbound, azure};
use okapi_store::channels::ChannelCandidate;

/// 渠道出站修饰（代理 + 额外头）。热路径每次克隆小 vec，避免 store 依赖 providers。
#[must_use]
pub fn outbound(cand: &ChannelCandidate) -> Outbound {
    Outbound {
        proxy_url: cand.proxy_url.clone(),
        extra_headers: cand.extra_headers.clone(),
    }
}

/// 官方 OpenAI 缺省地址（渠道未配置 api_base 时）。
pub const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";

/// 渠道是否走 Azure 分派。
pub fn is_azure(cand: &ChannelCandidate) -> bool {
    cand.provider == "azure"
}

/// 非 Azure 渠道的 api_base（缺省官方地址）。
pub fn openai_base(cand: &ChannelCandidate) -> String {
    cand.api_base
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned())
}

/// Azure 渠道的（端点, api-version）；端点缺失即构造错误。
fn azure_target(cand: &ChannelCandidate) -> Result<(&str, &str), UpstreamError> {
    let endpoint = cand
        .api_base
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| UpstreamError::Build("azure_api_base_missing".to_owned()))?;
    let api_version = cand
        .api_version
        .as_deref()
        .unwrap_or(azure::DEFAULT_API_VERSION);
    Ok((endpoint, api_version))
}

impl AppState {
    /// chat completions（流式 / 非流式）。`upstream_model` 是映射后的上游名——Azure 下
    /// 即部署名，进 URL；`body` 的 `model` 字段已由调用方重写为同一值。
    pub async fn openai_chat(
        &self,
        cand: &ChannelCandidate,
        upstream_model: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<ChatResponse, UpstreamError> {
        if is_azure(cand) {
            let (endpoint, api_version) = azure_target(cand)?;
            self.azure
                .chat(
                    endpoint,
                    api_version,
                    upstream_model,
                    &cand.credential,
                    body,
                    stream,
                    &outbound(cand),
                )
                .await
        } else {
            self.upstream
                .chat(
                    &openai_base(cand),
                    &cand.credential,
                    body,
                    stream,
                    &outbound(cand),
                )
                .await
        }
    }

    /// 非流式 JSON 端点（`path` 形如 `/embeddings`、`/images/generations`、`/rerank`）。
    pub async fn openai_json(
        &self,
        cand: &ChannelCandidate,
        upstream_model: &str,
        path: &str,
        body: Bytes,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        if is_azure(cand) {
            let (endpoint, api_version) = azure_target(cand)?;
            self.azure
                .json_relay(
                    endpoint,
                    api_version,
                    upstream_model,
                    path,
                    &cand.credential,
                    body,
                    &outbound(cand),
                )
                .await
        } else {
            self.upstream
                .json_relay(
                    &openai_base(cand),
                    path,
                    &cand.credential,
                    body,
                    &outbound(cand),
                )
                .await
        }
    }

    /// audio/speech：二进制音频出。
    pub async fn openai_speech(
        &self,
        cand: &ChannelCandidate,
        upstream_model: &str,
        body: Bytes,
    ) -> Result<(u16, String, Bytes), UpstreamError> {
        if is_azure(cand) {
            let (endpoint, api_version) = azure_target(cand)?;
            self.azure
                .speech(
                    endpoint,
                    api_version,
                    upstream_model,
                    &cand.credential,
                    body,
                    &outbound(cand),
                )
                .await
        } else {
            self.upstream
                .speech(&openai_base(cand), &cand.credential, body, &outbound(cand))
                .await
        }
    }

    /// audio/transcriptions | translations：multipart 入。
    pub async fn openai_audio_multipart(
        &self,
        cand: &ChannelCandidate,
        upstream_model: &str,
        path: &str,
        parts: Vec<(String, Option<String>, Option<String>, Bytes)>,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        if is_azure(cand) {
            let (endpoint, api_version) = azure_target(cand)?;
            self.azure
                .audio_multipart(
                    endpoint,
                    api_version,
                    upstream_model,
                    path,
                    &cand.credential,
                    parts,
                    &outbound(cand),
                )
                .await
        } else {
            self.upstream
                .audio_multipart(
                    &openai_base(cand),
                    path,
                    &cand.credential,
                    parts,
                    &outbound(cand),
                )
                .await
        }
    }
}
