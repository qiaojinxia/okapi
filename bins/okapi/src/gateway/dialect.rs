//! 出向方言与传输解耦（IMPLEMENTATION §11.35）。
//!
//! chat 族的 `(入口, 方言)` 分派矩阵只认三种出向方言：`openai` / `anthropic` / `gemini`。
//! `bedrock` 说 Anthropic 方言、`vertex` 按模型说 Anthropic 或 Gemini 方言——它们只是换了
//! URL 与鉴权的传输层，所以协议转换一行不加，这里把"取上游响应"按 provider 分派一次即可。

use super::openai_dialect::outbound;
use super::state::AppState;
use bytes::Bytes;
use okapi_providers::UpstreamError;
use okapi_providers::anthropic::MessagesResponse;
use okapi_providers::gemini::GeminiResponse;
use okapi_store::channels::ChannelCandidate;

/// 渠道对某上游模型的出向方言。`codex` 是 OpenAI 方言但**只有 Responses 面**，
/// 由 `responses_native` 恒 true 保证走直转路径（见 `okapi_store::channels::responses_native_for`）。
#[must_use]
pub fn upstream_dialect<'a>(provider: &'a str, upstream_model: &str) -> &'a str {
    match provider {
        "anthropic" | "bedrock" | "anthropic_max" => "anthropic",
        "gemini" => "gemini",
        "vertex" => {
            if okapi_providers::vertex::is_anthropic_model(upstream_model) {
                "anthropic"
            } else {
                "gemini"
            }
        }
        _ => "openai",
    }
}

/// 只承诺 chat 族入口的 provider：embeddings / images / audio / videos / realtime 不路由。
#[must_use]
pub fn chat_only(provider: &str) -> bool {
    matches!(provider, "bedrock" | "vertex" | "anthropic_max" | "codex")
}

/// bedrock / vertex 没有可猜的缺省地址：缺 api_base 是构造错误，不回退到公网官方地址。
fn required_base(cand: &ChannelCandidate) -> Result<&str, UpstreamError> {
    cand.api_base
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| UpstreamError::Build(format!("{}_api_base_missing", cand.provider)))
}

impl AppState {
    /// Anthropic 方言一跳：直连 / Bedrock InvokeModel / Vertex rawPredict 按 provider 选。
    pub async fn messages_via(
        &self,
        cand: &ChannelCandidate,
        base: &str,
        upstream_model: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<MessagesResponse, UpstreamError> {
        let outbound = outbound(cand);
        match cand.provider.as_str() {
            "bedrock" => {
                self.bedrock
                    .messages(
                        required_base(cand)?,
                        cand.aws_region.as_deref(),
                        &cand.credential,
                        upstream_model,
                        body,
                        stream,
                        &outbound,
                    )
                    .await
            }
            // 订阅凭证：取可用 access token（必要时四步锁刷新），Bearer + oauth beta + 系统提示首句
            "anthropic_max" => {
                let cred = super::oauth_cred::fresh_credential(self, cand).await?;
                okapi_providers::oauth::anthropic_max::messages(
                    self.anthropic.http(),
                    base,
                    &cred.access_token,
                    body,
                    stream,
                    &outbound,
                )
                .await
            }
            "vertex" => {
                self.vertex
                    .messages(
                        required_base(cand)?,
                        &cand.credential,
                        upstream_model,
                        body,
                        stream,
                        &outbound,
                    )
                    .await
            }
            _ => {
                self.anthropic
                    .messages(base, &cand.credential, body, stream, &outbound)
                    .await
            }
        }
    }

    /// Gemini 方言一跳：直连 / Vertex generateContent 按 provider 选。
    pub async fn generate_via(
        &self,
        cand: &ChannelCandidate,
        base: &str,
        upstream_model: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<GeminiResponse, UpstreamError> {
        let outbound = outbound(cand);
        if cand.provider == "vertex" {
            self.vertex
                .generate(
                    required_base(cand)?,
                    &cand.credential,
                    upstream_model,
                    body,
                    stream,
                    &outbound,
                )
                .await
        } else {
            self.gemini
                .generate(
                    base,
                    &cand.credential,
                    upstream_model,
                    body,
                    stream,
                    &outbound,
                )
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_by_provider_and_model() {
        assert_eq!(upstream_dialect("openai", "gpt-4o"), "openai");
        assert_eq!(upstream_dialect("openai_compat", "x"), "openai");
        assert_eq!(upstream_dialect("azure", "x"), "openai");
        assert_eq!(upstream_dialect("anthropic", "claude-3"), "anthropic");
        assert_eq!(
            upstream_dialect("bedrock", "us.anthropic.claude-v1:0"),
            "anthropic"
        );
        assert_eq!(upstream_dialect("gemini", "gemini-2.5-pro"), "gemini");
        assert_eq!(
            upstream_dialect("vertex", "claude-sonnet-4-5@20250929"),
            "anthropic"
        );
        assert_eq!(upstream_dialect("vertex", "gemini-2.5-flash"), "gemini");
        assert!(chat_only("bedrock") && chat_only("vertex") && !chat_only("azure"));
    }
}
