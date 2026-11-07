//! A checked protocol/adapter operation, not a normalized request or response IR.
use super::ingress::Ingress;
use okapi_providers::registry::{
    self, AdapterKind, ApiFormat, Dialect, NativeResponses, ProviderDescriptor, WireTransport,
};
use okapi_store::ChannelCandidate;
use serde_json::Value;

#[derive(Clone, Copy, Default)]
pub(crate) struct Requirements {
    pub tools: bool,
    pub vision: bool,
    pub server_tools: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    ChatCompletions,
    Messages,
    GenerateContent,
    Responses,
    ResponsesCompact,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExecutionPlan {
    pub adapter: &'static ProviderDescriptor,
    pub dialect: Dialect,
    pub operation: Operation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PlanError {
    #[error("adapter_unregistered")]
    Unregistered,
    #[error("unsupported_endpoint")]
    Endpoint,
    #[error("upstream_capability_unsupported")]
    Capability,
    #[error("request_extension_unsupported")]
    Extension,
}

impl From<PlanError> for okapi_providers::UpstreamError {
    fn from(error: PlanError) -> Self {
        Self::Build(error.to_string())
    }
}

impl ExecutionPlan {
    pub fn compile(
        ingress: Ingress,
        channel: &ChannelCandidate,
        model: &str,
        requirements: Requirements,
    ) -> Result<Self, PlanError> {
        okapi_providers::profiles::validate_extensions(&channel.provider, &channel.extensions)
            .map_err(|_| PlanError::Extension)?;
        Self::compile_target(
            ingress,
            &channel.provider,
            channel.upstream_model(model),
            channel.responses_native,
            &channel.capabilities,
            requirements,
        )
    }

    pub fn compile_target(
        ingress: Ingress,
        provider: &str,
        upstream_model: &str,
        native: bool,
        caps: &Value,
        requirements: Requirements,
    ) -> Result<Self, PlanError> {
        let adapter = registry::lookup(provider).ok_or(PlanError::Unregistered)?;
        if !adapter.accepts(format(ingress), upstream_model, native) {
            return Err(PlanError::Endpoint);
        }
        let dialect = adapter.dialect(upstream_model);
        let native = adapter.native_responses.enabled(Some(native));
        let operation = match ingress {
            Ingress::ResponsesCompact => Operation::ResponsesCompact,
            Ingress::Responses if native => Operation::Responses,
            Ingress::OpenAi | Ingress::Anthropic | Ingress::Responses | Ingress::Gemini => {
                match dialect {
                    Dialect::OpenAi => Operation::ChatCompletions,
                    Dialect::Anthropic => Operation::Messages,
                    Dialect::Gemini => Operation::GenerateContent,
                    Dialect::Opaque => return Err(PlanError::Endpoint),
                }
            }
        };
        if (requirements.tools || requirements.server_tools) && denies(caps, "tools")
            || requirements.vision && denies(caps, "vision")
            || ingress == Ingress::ResponsesCompact && denies(caps, "compact")
            || requirements.server_tools
                && (dialect != Dialect::Anthropic || operation == Operation::Responses)
        {
            return Err(PlanError::Capability);
        }
        Ok(Self {
            adapter,
            dialect,
            operation,
        })
    }

    /// 这次转发要不要把请求从入口方言转成别家的（转了就可能丢工具，见 `convert::tool_loss`）。
    pub const fn converts(self, ingress: Ingress) -> bool {
        !matches!(
            (ingress, self.operation),
            (Ingress::OpenAi, Operation::ChatCompletions)
                | (Ingress::Anthropic, Operation::Messages)
                | (Ingress::Gemini, Operation::GenerateContent)
                | (Ingress::Responses, Operation::Responses)
                | (Ingress::ResponsesCompact, _)
        )
    }

    pub const fn native_responses(self) -> bool {
        matches!(
            self.operation,
            Operation::Responses | Operation::ResponsesCompact
        )
    }

    /// 这次尝试打到上游的端点（`{model}` 是上游模型名的占位）：账单与日志的 `upstream_endpoint`、
    /// 诊断里每次尝试记的端点都用它，同一次尝试只有一个标签。
    pub const fn endpoint(self, stream: bool) -> &'static str {
        match (self.operation, self.adapter.kind) {
            (Operation::ResponsesCompact, _) => "/v1/responses/compact",
            (Operation::Responses, _) => "/v1/responses",
            (Operation::Messages, AdapterKind::Bedrock) if stream => {
                "/model/{model}/invoke-with-response-stream"
            }
            (Operation::Messages, AdapterKind::Bedrock) => "/model/{model}/invoke",
            (Operation::Messages, AdapterKind::Vertex) if stream => {
                "/publishers/anthropic/models/{model}:streamRawPredict"
            }
            (Operation::Messages, AdapterKind::Vertex) => {
                "/publishers/anthropic/models/{model}:rawPredict"
            }
            (Operation::Messages, _) => "/v1/messages",
            (Operation::GenerateContent, AdapterKind::Vertex) if stream => {
                "/publishers/google/models/{model}:streamGenerateContent"
            }
            (Operation::GenerateContent, AdapterKind::Vertex) => {
                "/publishers/google/models/{model}:generateContent"
            }
            (Operation::GenerateContent, _) if stream => {
                "/v1beta/models/{model}:streamGenerateContent"
            }
            (Operation::GenerateContent, _) => "/v1beta/models/{model}:generateContent",
            (Operation::ChatCompletions, AdapterKind::Azure) => {
                "/openai/deployments/{model}/chat/completions"
            }
            (Operation::ChatCompletions, _) => "/v1/chat/completions",
        }
    }

    pub const fn can_fallback_to_chat(self) -> bool {
        matches!(self.operation, Operation::Responses)
            && matches!(
                self.adapter.native_responses,
                NativeResponses::Optional { .. }
            )
    }

    /// WS ingress currently requires a registered native WS adapter even when bridging HTTP.
    pub fn websocket_ingress(self) -> bool {
        self.operation == Operation::Responses
            && self
                .adapter
                .supports_transport(WireTransport::ResponsesWebSocket)
    }

    pub fn supports_transport(self, transport: WireTransport, caps: &Value) -> bool {
        self.adapter.supports_transport(transport)
            && !denies(
                caps,
                match transport {
                    WireTransport::Http => "responses_http",
                    WireTransport::ResponsesWebSocket => "responses_websocket",
                },
            )
    }
}

pub(crate) fn format(ingress: Ingress) -> ApiFormat {
    match ingress {
        Ingress::OpenAi => ApiFormat::ChatCompletions,
        Ingress::Anthropic => ApiFormat::Messages,
        Ingress::Responses => ApiFormat::Responses,
        Ingress::ResponsesCompact => ApiFormat::ResponsesCompact,
        Ingress::Gemini => ApiFormat::Gemini,
    }
}

fn denies(caps: &Value, name: &str) -> bool {
    caps.get(name).and_then(Value::as_bool) == Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Owned JSON keeps the protocol matrix fixtures readable; production borrows capabilities.
    #[allow(clippy::needless_pass_by_value)]
    fn plan(
        ingress: Ingress,
        provider: &str,
        native: bool,
        caps: Value,
    ) -> Result<ExecutionPlan, PlanError> {
        ExecutionPlan::compile_target(
            ingress,
            provider,
            "model",
            native,
            &caps,
            Requirements::default(),
        )
    }

    #[test]
    fn native_responses_preserve_operation_and_compatibility_is_explicit() {
        assert_eq!(
            plan(Ingress::Responses, "openai", true, json!({}))
                .unwrap()
                .operation,
            Operation::Responses
        );
        assert_eq!(
            plan(Ingress::Responses, "openai_compat", false, json!({}))
                .unwrap()
                .operation,
            Operation::ChatCompletions
        );
        assert_eq!(
            plan(Ingress::Responses, "anthropic", true, json!({}))
                .unwrap()
                .operation,
            Operation::Messages
        );
        assert_eq!(
            plan(Ingress::Responses, "codex", false, json!({}))
                .unwrap()
                .operation,
            Operation::Responses
        );
        assert!(plan(Ingress::Responses, "unregistered", true, json!({})).is_err());
        assert!(plan(Ingress::Responses, "custom_pass", false, json!({})).is_err());
    }

    #[test]
    fn compact_never_degrades_to_chat_or_ignores_a_denial() {
        assert!(plan(Ingress::ResponsesCompact, "openai_compat", false, json!({})).is_err());
        assert!(
            plan(
                Ingress::ResponsesCompact,
                "openai",
                true,
                json!({"compact":false})
            )
            .is_err()
        );
        assert!(plan(Ingress::ResponsesCompact, "anthropic", true, json!({})).is_err());
        assert_eq!(
            plan(Ingress::ResponsesCompact, "codex", true, json!({}))
                .unwrap()
                .operation,
            Operation::ResponsesCompact
        );
    }

    #[test]
    fn only_optional_responses_adapters_can_fallback_to_chat() {
        for provider in ["openai", "openai_compat"] {
            assert!(
                plan(Ingress::Responses, provider, true, json!({}))
                    .unwrap()
                    .can_fallback_to_chat()
            );
        }
        for ingress in [Ingress::Responses, Ingress::ResponsesCompact] {
            assert!(
                !plan(ingress, "codex", true, json!({}))
                    .unwrap()
                    .can_fallback_to_chat()
            );
        }
        assert!(
            !plan(Ingress::ResponsesCompact, "openai", true, json!({}))
                .unwrap()
                .can_fallback_to_chat()
        );
    }

    #[test]
    fn registry_and_stored_native_policy_agree_for_existing_channels() {
        for adapter in registry::BUILT_INS {
            for configured in [None, Some(true), Some(false)] {
                assert_eq!(
                    adapter.native_responses.enabled(configured),
                    okapi_store::channels::responses_native_for(adapter.id, configured),
                    "{} / {configured:?}",
                    adapter.id
                );
            }
        }
    }

    #[test]
    fn explicit_capability_denials_filter_without_changing_unknown_defaults() {
        let needs = Requirements {
            tools: true,
            vision: true,
            server_tools: false,
        };
        for caps in [json!({}), json!({"tools":null,"vision":true})] {
            assert!(
                ExecutionPlan::compile_target(Ingress::OpenAi, "openai", "x", true, &caps, needs)
                    .is_ok()
            );
        }
        for caps in [json!({"tools":false}), json!({"vision":false})] {
            assert_eq!(
                ExecutionPlan::compile_target(Ingress::OpenAi, "openai", "x", true, &caps, needs)
                    .unwrap_err(),
                PlanError::Capability
            );
        }
    }

    #[test]
    fn server_tools_require_the_existing_anthropic_operation() {
        let needs = Requirements {
            server_tools: true,
            ..Requirements::default()
        };
        for provider in ["anthropic", "anthropic_max", "bedrock"] {
            assert!(
                ExecutionPlan::compile_target(
                    Ingress::Responses,
                    provider,
                    "claude",
                    false,
                    &json!({}),
                    needs
                )
                .is_ok()
            );
        }
        assert!(
            ExecutionPlan::compile_target(
                Ingress::Responses,
                "openai",
                "x",
                true,
                &json!({}),
                needs
            )
            .is_err()
        );
    }

    #[test]
    fn websocket_registration_and_channel_capabilities_are_both_required() {
        let native = plan(Ingress::Responses, "openai", true, json!({})).unwrap();
        assert!(native.websocket_ingress());
        assert!(!native.supports_transport(
            WireTransport::ResponsesWebSocket,
            &json!({"responses_websocket":false})
        ));
        assert!(
            native.supports_transport(WireTransport::Http, &json!({"responses_websocket":false}))
        );
        assert!(
            !plan(Ingress::Responses, "openai_compat", true, json!({}))
                .unwrap()
                .websocket_ingress()
        );
        assert!(
            !plan(Ingress::Responses, "openai", false, json!({}))
                .unwrap()
                .websocket_ingress()
        );
    }

    /// 标签改由计划推导之前的那张表（按入口 × provider），留作对照。
    fn previous_label(
        ingress: Ingress,
        provider: &str,
        responses_native: bool,
        stream: bool,
    ) -> &'static str {
        match (ingress, provider) {
            (Ingress::ResponsesCompact, _) => "/v1/responses/compact",
            (Ingress::Responses, _) if responses_native => "/v1/responses",
            (_, "anthropic" | "anthropic_max") => "/v1/messages",
            (_, "bedrock") if stream => "/model/{model}/invoke-with-response-stream",
            (_, "bedrock") => "/model/{model}/invoke",
            (_, "vertex") if stream => "/publishers/{publisher}/models/{model}:stream",
            (_, "vertex") => "/publishers/{publisher}/models/{model}:predict",
            (Ingress::OpenAi | Ingress::Responses | Ingress::Gemini, "gemini") if stream => {
                "/v1beta/models/{model}:streamGenerateContent"
            }
            (Ingress::OpenAi | Ingress::Responses | Ingress::Gemini, "gemini") => {
                "/v1beta/models/{model}:generateContent"
            }
            (_, "azure") => "/openai/deployments/{model}/chat/completions",
            _ => "/v1/chat/completions",
        }
    }

    /// 每种编得出计划的 入口 × provider × 模型方言 × 直转开关 × 流式 组合，标签都和原来那张表一样，
    /// 只有 Vertex 改记真实路径（Claude 走 rawPredict，Gemini 走 generateContent）。
    #[test]
    fn endpoint_labels_keep_their_values_except_vertex() {
        let mut compared = 0;
        for provider in [
            "openai",
            "openai_compat",
            "azure",
            "anthropic",
            "anthropic_max",
            "bedrock",
            "vertex",
            "gemini",
            "codex",
            "custom_pass",
        ] {
            for model in ["gpt-5", "claude-sonnet-4-5@20250929", "gemini-2.5-pro"] {
                for ingress in [
                    Ingress::OpenAi,
                    Ingress::Anthropic,
                    Ingress::Responses,
                    Ingress::ResponsesCompact,
                    Ingress::Gemini,
                ] {
                    for native in [false, true] {
                        let Ok(plan) = ExecutionPlan::compile_target(
                            ingress,
                            provider,
                            model,
                            native,
                            &json!({}),
                            Requirements::default(),
                        ) else {
                            continue;
                        };
                        // 渠道上存的是生效值（`okapi_store::channels::responses_native_for`）
                        let effective =
                            okapi_domain::provider_contract::native_responses_for(provider)
                                .enabled(Some(native));
                        for stream in [false, true] {
                            let case = format!("{provider} {model} {ingress:?} {native} {stream}");
                            let expected = match (provider, plan.dialect, stream) {
                                ("vertex", Dialect::Anthropic, false) => {
                                    "/publishers/anthropic/models/{model}:rawPredict"
                                }
                                ("vertex", Dialect::Anthropic, true) => {
                                    "/publishers/anthropic/models/{model}:streamRawPredict"
                                }
                                ("vertex", _, false) => {
                                    "/publishers/google/models/{model}:generateContent"
                                }
                                ("vertex", _, true) => {
                                    "/publishers/google/models/{model}:streamGenerateContent"
                                }
                                _ => previous_label(ingress, provider, effective, stream),
                            };
                            assert_eq!(plan.endpoint(stream), expected, "{case}");
                            compared += 1;
                        }
                    }
                }
            }
        }
        assert!(compared > 200, "对照的组合太少，循环条件写错了：{compared}");
    }
}
