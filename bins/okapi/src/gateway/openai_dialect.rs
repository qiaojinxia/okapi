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
        context: okapi_providers::profiles::RequestContext {
            extensions: cand.extensions.clone(),
            identity_seed: Some(cand.channel_key_id.to_string()),
            ..Default::default()
        },
    }
}

/// 渠道是否走 Azure 分派。
pub fn is_azure(cand: &ChannelCandidate) -> bool {
    okapi_providers::registry::lookup(&cand.provider)
        .is_some_and(|adapter| adapter.kind == okapi_providers::registry::AdapterKind::Azure)
}

/// 配置里缺少地址、协议又没有缺省地址时的构造错误原因（`classify_fatal` 据此换下一个候选）。
pub const API_BASE_MISSING: &str = "api_base_missing";

/// 非 Azure 渠道的基址：配置的 `api_base`；没配时只有官方 `openai` 有缺省（官方地址）。
/// `openai_compat` 没有缺省——缺了是配置不完整，按构造错误处理，**不回退到 api.openai.com**：
/// 那会把第三方的 key 发给 OpenAI。
pub fn openai_base(cand: &ChannelCandidate) -> Result<String, UpstreamError> {
    okapi_providers::registry::effective_base(&cand.provider, cand.api_base.as_deref())
        .ok_or_else(|| UpstreamError::Build(API_BASE_MISSING.to_owned()))
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
    pub async fn openai_image_stream(
        &self,
        cand: &ChannelCandidate,
        model: &str,
        path: &str,
        body: okapi_providers::image_stream::ImageBody,
    ) -> Result<okapi_providers::image_stream::ImageResponse, UpstreamError> {
        super::account_control::execute(self, cand, model, path, async {
            if is_azure(cand) {
                let (endpoint, version) = azure_target(cand)?;
                self.azure
                    .image_stream(
                        endpoint,
                        version,
                        model,
                        path,
                        &cand.credential,
                        body,
                        &outbound(cand),
                    )
                    .await
            } else {
                self.upstream
                    .image_stream(
                        &openai_base(cand)?,
                        path,
                        &cand.credential,
                        body,
                        &outbound(cand),
                    )
                    .await
            }
        })
        .await
    }

    /// chat completions（流式 / 非流式）。`upstream_model` 是映射后的上游名——Azure 下
    /// 即部署名，进 URL；`body` 的 `model` 字段已由调用方重写为同一值。
    pub async fn openai_chat(
        &self,
        cand: &ChannelCandidate,
        upstream_model: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<ChatResponse, UpstreamError> {
        match self
            .infer_via(
                cand,
                okapi_providers::inference::Surface::Chat,
                &openai_base(cand)?,
                upstream_model,
                body,
                stream,
                &outbound(cand),
            )
            .await?
        {
            okapi_providers::inference::Response::OpenAi(response) => Ok(response),
            _ => Err(UpstreamError::Build("adapter_response_mismatch".into())),
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
        if path.contains("images")
            && let Some(trace) = super::diagnostics::Trace::current()
        {
            trace.media(&body, false);
        }
        super::account_control::execute(self, cand, upstream_model, path, async {
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
                        &openai_base(cand)?,
                        path,
                        &cand.credential,
                        body,
                        &outbound(cand),
                    )
                    .await
            }
        })
        .await
    }

    /// audio/speech：二进制音频出。
    pub async fn openai_speech(
        &self,
        cand: &ChannelCandidate,
        upstream_model: &str,
        body: Bytes,
    ) -> Result<(u16, String, Bytes), UpstreamError> {
        super::account_control::execute(self, cand, upstream_model, "/v1/audio/speech", async {
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
                    .speech(&openai_base(cand)?, &cand.credential, body, &outbound(cand))
                    .await
            }
        })
        .await
    }

    /// audio/transcriptions | translations：multipart 入。
    pub async fn openai_audio_multipart(
        &self,
        cand: &ChannelCandidate,
        upstream_model: &str,
        path: &str,
        parts: Vec<(String, Option<String>, Option<String>, Bytes)>,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        super::account_control::execute(self, cand, upstream_model, path, async {
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
                        &openai_base(cand)?,
                        path,
                        &cand.credential,
                        parts,
                        &outbound(cand),
                    )
                    .await
            }
        })
        .await
    }
}
