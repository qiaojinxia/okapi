//! token 用量：计费引擎的用量输入。

use crate::error::DomainError;
use serde::{Deserialize, Serialize};

/// Original provider totals in normalized billing units. Missing axes are estimated;
/// a missing outer object on TokenUsage means provenance was not recorded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamTokenCounts {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

fn source(recorded: bool, reported: Option<u32>, settled: u32) -> &'static str {
    if recorded {
        match reported {
            None => "estimated",
            Some(count) if count == settled => "upstream",
            Some(_) => "local_override",
        }
    } else {
        "unknown"
    }
}

/// Modal subsets of a cache total; text is the remainder, never another charge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheModalities {
    pub audio_tokens: u32,
    pub image_tokens: u32,
}

impl CacheModalities {
    #[must_use]
    pub fn total_modal(&self) -> u64 {
        u64::from(self.audio_tokens) + u64::from(self.image_tokens)
    }
}

/// 一次请求的 token 用量。
///
/// 字段名对齐 OpenAI 官方 usage 细分（`prompt_tokens_details` /
/// `completion_tokens_details`，见 openai-python `completion_usage.py`），
/// 故上游响应可直接反序列化，无需逐 provider 起别名。
///
/// # 计费分段（DESIGN §3.2）
///
/// prompt 侧五段互斥，合计 = `prompt_tokens`：
/// - `cached_tokens`：缓存**读取**，按模型的 cache_ratio 计价；
/// - `cache_write_tokens`：缓存**写入**，按模型的 cache_write_ratio 计价；
/// - `audio_prompt_tokens`：音频输入，按 audio_ratio 加价（gpt-4o-audio 官方 16×）；
/// - `image_prompt_tokens`：图片输入，按 image_ratio；
/// - 余下 `prompt_uncached()`：常规文本，1.0×。
///
/// completion 侧：`audio_completion_tokens` 按 audio_ratio × audio_completion_ratio
/// （与 new-api 同语义：音频输出相对音频输入再乘一档），余下按 completion_ratio。
///
/// Cache totals include their modal subsets. The input audio/image fields contain
/// only uncached tokens. Adapters with detailed cache usage must subtract the
/// intersections before constructing this value. None preserves legacy adapters'
/// text-priced cache behavior; it does not assert that caches are text-only.
/// Image output is independent of text and audio output.
///
/// `reasoning_tokens` 计入 completion 总数（仅统计拆分，不重复计费）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_usage: Option<UpstreamTokenCounts>,
    pub prompt_tokens: u32,
    pub cached_tokens: u32,
    /// Known cache intersections. None keeps the legacy text-priced cache behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_modalities: Option<CacheModalities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_modalities: Option<CacheModalities>,
    /// 上游是否明确上报缓存读取；缺字段/估算不等于真实零命中。
    #[serde(default)]
    pub cache_read_reported: bool,
    /// 上游是否明确上报缓存写入。只描述采集状态，不改变计费数值。
    #[serde(default)]
    pub cache_write_reported: bool,
    /// 缓存写入 token（Anthropic `cache_creation_input_tokens`）；含在 prompt_tokens 内。
    #[serde(default)]
    pub cache_write_tokens: u32,
    /// 音频输入 token（OpenAI `prompt_tokens_details.audio_tokens`）；含在 prompt_tokens 内。
    #[serde(default)]
    pub audio_prompt_tokens: u32,
    /// 图片输入 token（OpenAI `prompt_tokens_details.image_tokens`）；含在 prompt_tokens 内。
    #[serde(default)]
    pub image_prompt_tokens: u32,
    pub completion_tokens: u32,
    /// 音频输出 token（OpenAI `completion_tokens_details.audio_tokens`）；含在 completion 内。
    #[serde(default)]
    pub audio_completion_tokens: u32,
    #[serde(default)]
    pub image_completion_tokens: u32,
    pub reasoning_tokens: u32,
}

impl TokenUsage {
    #[must_use]
    pub fn prompt_source(&self) -> &'static str {
        source(
            self.upstream_usage.is_some(),
            self.upstream_usage.and_then(|u| u.prompt_tokens),
            self.prompt_tokens,
        )
    }

    #[must_use]
    pub fn completion_source(&self) -> &'static str {
        source(
            self.upstream_usage.is_some(),
            self.upstream_usage.and_then(|u| u.completion_tokens),
            self.completion_tokens,
        )
    }

    /// prompt 侧各计价段合计（不含常规文本段）。
    fn prompt_segments(&self) -> u64 {
        u64::from(self.cached_tokens)
            + u64::from(self.cache_write_tokens)
            + u64::from(self.audio_prompt_tokens)
            + u64::from(self.image_prompt_tokens)
    }

    /// 校验不变量；计费入口必须先调用。
    pub fn validate(&self) -> Result<(), DomainError> {
        if self
            .cache_read_modalities
            .is_some_and(|v| v.total_modal() > u64::from(self.cached_tokens))
            || self
                .cache_write_modalities
                .is_some_and(|v| v.total_modal() > u64::from(self.cache_write_tokens))
        {
            return Err(DomainError::InvalidTokenUsage {
                reason: "cache modalities exceed cache total",
            });
        }
        // 各段都含在 prompt 内且互斥，合计不得越界——否则 prompt_uncached 被截断为 0，
        // 常规文本段静默漏计费
        if self.prompt_segments() > u64::from(self.prompt_tokens) {
            return Err(DomainError::InvalidTokenUsage {
                reason: "prompt segments (cached + cache_write + audio + image) > prompt_tokens",
            });
        }
        if self.reasoning_tokens > self.completion_tokens {
            return Err(DomainError::InvalidTokenUsage {
                reason: "reasoning_tokens > completion_tokens",
            });
        }
        // 音频输出与 reasoning 同为 completion 的子集，各自独立不得越界
        if u64::from(self.audio_completion_tokens) + u64::from(self.image_completion_tokens)
            > u64::from(self.completion_tokens)
        {
            return Err(DomainError::InvalidTokenUsage {
                reason: "audio + image completion tokens > completion_tokens",
            });
        }
        Ok(())
    }

    /// 常规文本输入部分（扣除缓存读写与音频、图片段后的余量）。
    #[must_use]
    pub fn prompt_uncached(&self) -> u32 {
        // prompt_segments 已由 validate 保证不越界；此处 saturating 仅作防御
        u32::try_from(u64::from(self.prompt_tokens).saturating_sub(self.prompt_segments()))
            .unwrap_or(0)
    }

    /// 常规文本输出部分（扣除音频输出段后的余量）。
    #[must_use]
    pub const fn text_completion(&self) -> u32 {
        self.completion_tokens
            .saturating_sub(self.audio_completion_tokens)
            .saturating_sub(self.image_completion_tokens)
    }

    #[must_use]
    pub fn cached_text(&self) -> u32 {
        u32::try_from(
            u64::from(self.cached_tokens)
                .saturating_sub(self.cache_read_modalities.unwrap_or_default().total_modal()),
        )
        .unwrap_or(0)
    }

    #[must_use]
    pub fn cache_write_text(&self) -> u32 {
        u32::try_from(
            u64::from(self.cache_write_tokens).saturating_sub(
                self.cache_write_modalities
                    .unwrap_or_default()
                    .total_modal(),
            ),
        )
        .unwrap_or(0)
    }

    /// 原始总 token 数（prompt + completion，不含倍率加权），用于阶梯档位判定。
    #[must_use]
    pub fn total_raw(&self) -> u64 {
        u64::from(self.prompt_tokens) + u64::from(self.completion_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_subsets_and_output_modalities_are_validated_without_double_counting() {
        let usage = TokenUsage {
            prompt_tokens: 100,
            cached_tokens: 50,
            image_prompt_tokens: 40,
            cache_read_modalities: Some(CacheModalities {
                image_tokens: 40,
                audio_tokens: 0,
            }),
            completion_tokens: 100,
            image_completion_tokens: 70,
            audio_completion_tokens: 20,
            ..TokenUsage::default()
        };
        assert!(usage.validate().is_ok());
        assert_eq!(usage.prompt_uncached(), 10);
        assert_eq!(usage.cached_text(), 10);
        assert_eq!(usage.text_completion(), 10);
        assert_eq!(usage.total_raw(), 200);
        let mut invalid = usage;
        invalid.cache_read_modalities = Some(CacheModalities {
            image_tokens: 40,
            audio_tokens: 11,
        });
        assert!(invalid.validate().is_err());
        invalid = usage;
        invalid.image_completion_tokens = 81;
        assert!(invalid.validate().is_err());
        invalid = usage;
        invalid.cache_write_modalities = Some(CacheModalities {
            image_tokens: 1,
            audio_tokens: 0,
        });
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn validate_rejects_cached_exceeding_prompt() {
        let usage = TokenUsage {
            prompt_tokens: 10,
            cached_tokens: 11,
            ..TokenUsage::default()
        };
        assert!(usage.validate().is_err());
    }

    #[test]
    fn prompt_uncached_subtracts_cached() {
        let usage = TokenUsage {
            prompt_tokens: 1000,
            cached_tokens: 800,
            ..TokenUsage::default()
        };
        assert_eq!(usage.prompt_uncached(), 200);
        assert_eq!(usage.total_raw(), 1000);
    }

    /// prompt 三段互斥：读取 + 写入 + 常规 = prompt_tokens（缺一不可，否则漏计费）。
    #[test]
    fn prompt_splits_into_three_exclusive_segments() {
        let usage = TokenUsage {
            prompt_tokens: 1000,
            cached_tokens: 600,
            cache_write_tokens: 300,
            ..TokenUsage::default()
        };
        assert_eq!(usage.prompt_uncached(), 100);
        assert_eq!(
            usage.prompt_uncached() + usage.cached_tokens + usage.cache_write_tokens,
            usage.prompt_tokens,
            "三段必须恰好覆盖 prompt 总数"
        );
        assert!(usage.validate().is_ok());
    }

    #[test]
    fn validate_rejects_cache_segments_exceeding_prompt() {
        let usage = TokenUsage {
            prompt_tokens: 100,
            cached_tokens: 60,
            cache_write_tokens: 50,
            ..TokenUsage::default()
        };
        assert!(
            usage.validate().is_err(),
            "读+写越界必须拒绝，不能让常规段被截断为 0"
        );
    }

    /// prompt 五段互斥且恰好覆盖总数（多模态请求的完整分解）。
    #[test]
    fn prompt_splits_into_five_exclusive_segments() {
        let usage = TokenUsage {
            prompt_tokens: 1000,
            cached_tokens: 200,
            cache_write_tokens: 100,
            audio_prompt_tokens: 300,
            image_prompt_tokens: 150,
            completion_tokens: 500,
            audio_completion_tokens: 200,
            reasoning_tokens: 50,
            ..TokenUsage::default()
        };
        assert!(usage.validate().is_ok());
        assert_eq!(usage.prompt_uncached(), 250);
        assert_eq!(
            usage.prompt_uncached()
                + usage.cached_tokens
                + usage.cache_write_tokens
                + usage.audio_prompt_tokens
                + usage.image_prompt_tokens,
            usage.prompt_tokens,
            "五段必须恰好覆盖 prompt 总数"
        );
        // completion 侧两段互斥
        assert_eq!(usage.text_completion(), 300);
        assert_eq!(
            usage.text_completion() + usage.audio_completion_tokens,
            usage.completion_tokens
        );
    }

    #[test]
    fn validate_rejects_modal_segments_exceeding_prompt() {
        let usage = TokenUsage {
            prompt_tokens: 100,
            audio_prompt_tokens: 60,
            image_prompt_tokens: 60,
            ..TokenUsage::default()
        };
        assert!(usage.validate().is_err(), "音频+图片越界必须拒绝");
    }

    #[test]
    fn validate_rejects_audio_completion_exceeding_completion() {
        let usage = TokenUsage {
            completion_tokens: 50,
            audio_completion_tokens: 51,
            ..TokenUsage::default()
        };
        assert!(usage.validate().is_err());
    }
}
