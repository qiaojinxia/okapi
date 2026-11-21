use okapi_domain::{ModalitiesReported, TokenUsage};
use serde::Deserialize;

/// OpenAI usage 探针；缺失轴由标记保留，不能把占位零当作实报。
#[derive(Debug, Clone, Copy, Default)]
pub struct UsageProbe {
    pub server_tool_usage: Option<okapi_domain::ServerToolUsage>,
    pub missing_prompt: bool,
    pub missing_completion: bool,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub prompt_tokens_details: PromptTokensDetails,
    pub completion_tokens_details: CompletionTokensDetails,
    /// Internal parse failure. Kept distinct from absent usage so settlement cannot estimate it.
    pub invalid: bool,
}

/// 字段名与 OpenAI 官方 `prompt_tokens_details` 一致（openai-python
/// `completion_usage.py`），故 OpenAI 系响应可直接反序列化。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(try_from = "RawPromptTokensDetails")]
pub struct PromptTokensDetails {
    pub cached_tokens: u32,
    /// 兼容扩展：缓存写入；Anthropic 由 cache_creation_input_tokens 映射。
    pub cache_write_tokens: u32,
    pub cache_write_5m_tokens: Option<u32>,
    pub cache_write_1h_tokens: Option<u32>,
    pub cache_read_reported: bool,
    pub cache_write_reported: bool,
    pub modalities_reported: ModalitiesReported,
    /// 音频输入总量，含其中的缓存部分；价格来自当前模型配置。
    #[serde(default)]
    pub audio_tokens: u32,
    /// 图片输入总量，含其中的缓存部分。
    #[serde(default)]
    pub image_tokens: u32,
    /// Compatible extensions: modal subsets of the cache totals, not extra tokens.
    pub cached_tokens_details: Option<ModalTokensDetails>,
    pub cache_write_tokens_details: Option<ModalTokensDetails>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, serde::Serialize)]
pub struct ModalTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_tokens: Option<u32>,
}

impl ModalTokensDetails {
    /// Explicit fields or complete coverage establish missing subsets as zero.
    #[must_use]
    pub fn reported(self, total: u32) -> ModalitiesReported {
        let covered = u64::from(self.text_tokens.unwrap_or(0))
            + u64::from(self.audio_tokens.unwrap_or(0))
            + u64::from(self.image_tokens.unwrap_or(0))
            == u64::from(total);
        ModalitiesReported {
            audio: self.audio_tokens.is_some() || covered,
            image: self.image_tokens.is_some() || covered,
        }
    }
}

#[derive(Default, Deserialize)]
struct RawPromptTokensDetails {
    #[serde(rename = "cached_tokens")]
    cached: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    cache_read_tokens: Option<u32>,
    prompt_cache_hit_tokens: Option<u32>,
    #[serde(rename = "cache_write_tokens")]
    cache_write: Option<u32>,
    cache_creation_input_tokens: Option<u32>,
    created_cache_tokens: Option<u32>,
    cached_creation_tokens: Option<u32>,
    cache_creation_tokens: Option<u32>,
    cache_write_input_tokens: Option<u32>,
    cache_creation: Option<serde_json::Map<String, serde_json::Value>>,
    cache_write_5m_tokens: Option<u32>,
    cache_write_1h_tokens: Option<u32>,
    claude_cache_creation_5_m_tokens: Option<u32>,
    claude_cache_creation_1_h_tokens: Option<u32>,
    #[serde(rename = "audio_tokens")]
    audio: Option<u32>,
    #[serde(rename = "image_tokens")]
    image: Option<u32>,
    cached_tokens_details: Option<ModalTokensDetails>,
    audio_cached_tokens: Option<u32>,
    cache_write_tokens_details: Option<ModalTokensDetails>,
}

#[derive(Deserialize)]
struct CacheCreationDetails {
    ephemeral_5m_input_tokens: Option<u32>,
    ephemeral_1h_input_tokens: Option<u32>,
}

pub(crate) fn cache_counter(values: &[Option<u32>]) -> Result<Option<u32>, &'static str> {
    let mut observed = None;
    for value in values.iter().copied().flatten() {
        if observed.is_some_and(|previous| previous != value) {
            return Err("conflicting_cache_counters");
        }
        observed = Some(value);
    }
    Ok(observed)
}

impl TryFrom<RawPromptTokensDetails> for PromptTokensDetails {
    type Error = &'static str;

    fn try_from(raw: RawPromptTokensDetails) -> Result<Self, Self::Error> {
        let cached = cache_counter(&[
            raw.cached,
            raw.cache_read_input_tokens,
            raw.cache_read_tokens,
            raw.prompt_cache_hit_tokens,
        ])?;
        // Qwen and vLLM report the same input subset under different names.
        // Matching mirrors are one observation; conflicting mirrors are invalid.
        let cache_write = cache_counter(&[
            raw.cache_write,
            raw.cache_creation_input_tokens,
            raw.created_cache_tokens,
            raw.cached_creation_tokens,
            raw.cache_creation_tokens,
            raw.cache_write_input_tokens,
        ])?;
        let creation = raw
            .cache_creation
            .map(|value| {
                serde_json::from_value::<CacheCreationDetails>(serde_json::Value::Object(value))
                    .map_err(|_| "invalid_cache_creation_details")
            })
            .transpose()?
            .filter(|d| {
                d.ephemeral_5m_input_tokens.is_some() || d.ephemeral_1h_input_tokens.is_some()
            });
        // A supplied native TTL breakdown must cover the aggregate; validate()
        // rejects partial/contradictory totals. An omitted breakdown stays unknown.
        let short = cache_counter(&[
            raw.cache_write_5m_tokens,
            raw.claude_cache_creation_5_m_tokens
                .filter(|n| *n > 0 || cache_write.is_some()),
            creation
                .as_ref()
                .map(|d| d.ephemeral_5m_input_tokens.unwrap_or(0)),
        ])?;
        let long = cache_counter(&[
            raw.cache_write_1h_tokens,
            raw.claude_cache_creation_1_h_tokens
                .filter(|n| *n > 0 || cache_write.is_some()),
            creation
                .as_ref()
                .map(|d| d.ephemeral_1h_input_tokens.unwrap_or(0)),
        ])?;
        // Ark reports cached audio as a subset of cached_tokens, never an
        // additional cache hit. Preserve explicit zero and reject conflicting mirrors.
        let mut cached_details = raw.cached_tokens_details;
        if let Some(audio) = raw.audio_cached_tokens {
            if raw.image.unwrap_or(0) > 0
                && cached_details.is_none_or(|d| d.image_tokens.is_none())
                && cached != Some(audio)
            {
                return Err("ambiguous_cache_image_intersection");
            }
            let details = cached_details.get_or_insert_with(ModalTokensDetails::default);
            details.audio_tokens = cache_counter(&[details.audio_tokens, Some(audio)])?;
        }
        Ok(Self {
            cached_tokens: cached.unwrap_or(0),
            cache_write_tokens: cache_write.unwrap_or(0),
            cache_write_5m_tokens: short,
            cache_write_1h_tokens: long,
            cache_read_reported: cached.is_some(),
            cache_write_reported: cache_write.is_some(),
            audio_tokens: raw.audio.unwrap_or(0),
            image_tokens: raw.image.unwrap_or(0),
            modalities_reported: ModalitiesReported {
                audio: raw.audio.is_some(),
                image: raw.image.is_some(),
            },
            cached_tokens_details: cached_details,
            cache_write_tokens_details: raw.cache_write_tokens_details,
        })
    }
}

impl PromptTokensDetails {
    fn with_previous(mut self, previous: Self) -> Self {
        if !self.cache_read_reported && self.cached_tokens == 0 {
            self.cached_tokens = previous.cached_tokens;
            self.cache_read_reported = previous.cache_read_reported;
            self.cached_tokens_details = self
                .cached_tokens_details
                .or(previous.cached_tokens_details);
        }
        if !self.cache_write_reported && self.cache_write_tokens == 0 {
            self.cache_write_tokens = previous.cache_write_tokens;
            self.cache_write_reported = previous.cache_write_reported;
            if self.cache_write_5m_tokens.is_none() && self.cache_write_1h_tokens.is_none() {
                self.cache_write_5m_tokens = previous.cache_write_5m_tokens;
                self.cache_write_1h_tokens = previous.cache_write_1h_tokens;
            }
            self.cache_write_tokens_details = self
                .cache_write_tokens_details
                .or(previous.cache_write_tokens_details);
        } else if self.cache_write_5m_tokens.is_none()
            && self.cache_write_1h_tokens.is_none()
            && self.cache_write_tokens == previous.cache_write_tokens
        {
            // 与 Anthropic 原生合并（anthropic_usage::merge）同一规则：较新的聚合只重报写入总量、
            // 没带 TTL 拆分时，总量没变则之前的拆分仍然成立（如末帧只给总数）；总量变了旧拆分才过期。
            // 曾直接丢掉拆分，缓存写入只能退回通用倍率，与原生路径计价不一致。
            self.cache_write_5m_tokens = previous.cache_write_5m_tokens;
            self.cache_write_1h_tokens = previous.cache_write_1h_tokens;
        }
        if !self.modalities_reported.audio && self.audio_tokens == 0 {
            self.audio_tokens = previous.audio_tokens;
            self.modalities_reported.audio = previous.modalities_reported.audio;
        }
        if !self.modalities_reported.image && self.image_tokens == 0 {
            self.image_tokens = previous.image_tokens;
            self.modalities_reported.image = previous.modalities_reported.image;
        }
        self
    }

    /// 协议转换时保留缺失状态，避免下游把补出的 0 当成明确上报。
    #[must_use]
    pub fn cache_json(self) -> serde_json::Value {
        let mut value = serde_json::json!({});
        if self.cache_read_reported || self.cached_tokens > 0 {
            value["cached_tokens"] = serde_json::json!(self.cached_tokens);
        }
        if self.cache_write_reported || self.cache_write_tokens > 0 {
            value["cache_write_tokens"] = serde_json::json!(self.cache_write_tokens);
        }
        if let (Some(short), Some(long)) = (self.cache_write_5m_tokens, self.cache_write_1h_tokens)
        {
            value["cache_write_5m_tokens"] = serde_json::json!(short);
            value["cache_write_1h_tokens"] = serde_json::json!(long);
        }
        if self.modalities_reported.audio || self.audio_tokens > 0 {
            value["audio_tokens"] = serde_json::json!(self.audio_tokens);
        }
        if self.modalities_reported.image || self.image_tokens > 0 {
            value["image_tokens"] = serde_json::json!(self.image_tokens);
        }
        if let Some(details) = self.cached_tokens_details {
            value["cached_tokens_details"] = serde_json::json!(details);
        }
        if let Some(details) = self.cache_write_tokens_details {
            value["cache_write_tokens_details"] = serde_json::json!(details);
        }
        value
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(from = "RawCompletionTokensDetails")]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
    /// 音频输出 token（官方字段同名）。
    #[serde(default)]
    pub audio_tokens: u32,
    /// Compatible extension used by native image-producing adapters.
    #[serde(default)]
    pub image_tokens: u32,
    pub modalities_reported: ModalitiesReported,
    pub reasoning_reported: bool,
}

#[derive(Default, Deserialize)]
struct RawCompletionTokensDetails {
    #[serde(rename = "reasoning_tokens")]
    reasoning: Option<u32>,
    #[serde(rename = "audio_tokens")]
    audio: Option<u32>,
    #[serde(rename = "image_tokens")]
    image: Option<u32>,
}

impl From<RawCompletionTokensDetails> for CompletionTokensDetails {
    fn from(raw: RawCompletionTokensDetails) -> Self {
        Self {
            reasoning_tokens: raw.reasoning.unwrap_or(0),
            audio_tokens: raw.audio.unwrap_or(0),
            image_tokens: raw.image.unwrap_or(0),
            modalities_reported: ModalitiesReported {
                audio: raw.audio.is_some(),
                image: raw.image.is_some(),
            },
            reasoning_reported: raw.reasoning.is_some(),
        }
    }
}

impl UsageProbe {
    /// Cumulative stream snapshots replace counts, never add them. A missing axis
    /// retains the previous observation; explicit zero replaces it. Invalid data
    /// poisons the sequence even if a later event looks valid.
    #[must_use]
    pub fn with_previous(mut self, previous: Option<Self>) -> Self {
        let Some(previous) = previous else {
            return self;
        };
        if self.invalid || previous.with_estimates(0, 0).is_err() {
            return Self::invalid();
        }
        self.server_tool_usage = match (self.server_tool_usage, previous.server_tool_usage) {
            (Some(next), Some(before)) => match next.with_previous(before) {
                Ok(tools) => Some(tools),
                Err(_) => return Self::invalid(),
            },
            (next, before) => next.or(before),
        };
        if self.missing_prompt && !previous.missing_prompt {
            self.prompt_tokens = previous.prompt_tokens;
            self.missing_prompt = false;
        }
        if self.missing_completion && !previous.missing_completion {
            self.completion_tokens = previous.completion_tokens;
            self.missing_completion = false;
        }
        self.prompt_tokens_details = self
            .prompt_tokens_details
            .with_previous(previous.prompt_tokens_details);
        self.completion_tokens_details = self
            .completion_tokens_details
            .with_previous(previous.completion_tokens_details);
        if self.with_estimates(0, 0).is_err() {
            return Self::invalid();
        }
        self
    }

    #[must_use]
    pub fn invalid() -> Self {
        Self {
            invalid: true,
            ..Self::default()
        }
    }
    /// Normalize intersecting cache/modality counts without clipping or guessing.
    /// Invalid or ambiguous counts must not become a valid-looking bill.
    pub fn to_token_usage(self) -> Result<TokenUsage, okapi_domain::DomainError> {
        crate::token_usage::normalize(self)
    }

    /// Fill only absent axes, retaining original counts for audit and local overrides.
    pub fn with_estimates(
        self,
        prompt: u32,
        completion: u32,
    ) -> Result<TokenUsage, okapi_domain::DomainError> {
        crate::token_usage::with_estimates(self, prompt, completion)
    }

    /// Conversion JSON must preserve absence, including across a second protocol hop.
    #[must_use]
    pub fn chat_json(self) -> serde_json::Value {
        if self.with_estimates(0, 0).is_err() {
            return serde_json::Value::Null;
        }
        let mut value = serde_json::json!({
            "prompt_tokens_details": self.prompt_tokens_details.cache_json(),
            "completion_tokens_details": self.completion_tokens_details.to_json(),
        });
        if let Some(tools) = self.server_tool_usage {
            value["server_tool_usage"] = serde_json::json!(tools);
        }
        if !self.missing_prompt {
            value["prompt_tokens"] = self.prompt_tokens.into();
        }
        if !self.missing_completion {
            value["completion_tokens"] = self.completion_tokens.into();
        }
        if !self.missing_prompt && !self.missing_completion {
            value["total_tokens"] =
                (u64::from(self.prompt_tokens) + u64::from(self.completion_tokens)).into();
        }
        value
    }
}

impl CompletionTokensDetails {
    fn with_previous(mut self, previous: Self) -> Self {
        if !self.reasoning_reported && self.reasoning_tokens == 0 {
            self.reasoning_tokens = previous.reasoning_tokens;
            self.reasoning_reported = previous.reasoning_reported;
        }
        if !self.modalities_reported.audio && self.audio_tokens == 0 {
            self.audio_tokens = previous.audio_tokens;
            self.modalities_reported.audio = previous.modalities_reported.audio;
        }
        if !self.modalities_reported.image && self.image_tokens == 0 {
            self.image_tokens = previous.image_tokens;
            self.modalities_reported.image = previous.modalities_reported.image;
        }
        self
    }

    #[must_use]
    pub fn to_json(self) -> serde_json::Value {
        let mut value = serde_json::json!({});
        if self.reasoning_reported || self.reasoning_tokens > 0 {
            value["reasoning_tokens"] = serde_json::json!(self.reasoning_tokens);
        }
        if self.modalities_reported.audio || self.audio_tokens > 0 {
            value["audio_tokens"] = serde_json::json!(self.audio_tokens);
        }
        if self.modalities_reported.image || self.image_tokens > 0 {
            value["image_tokens"] = serde_json::json!(self.image_tokens);
        }
        value
    }
}
