//! token 用量：计费引擎的用量输入。

use crate::error::DomainError;
use serde::{Deserialize, Serialize};

/// 一次请求的 token 用量。
///
/// 字段名对齐 OpenAI 官方 usage 细分（`prompt_tokens_details` /
/// `completion_tokens_details`，见 openai-python `completion_usage.py`），
/// 故上游响应可直接反序列化，无需逐 provider 起别名。
///
/// # 计费分段（DESIGN §3.2）
///
/// prompt 侧五段互斥，合计 = `prompt_tokens`：
/// - `cached_tokens`：缓存**读取**命中，按 cache_ratio 打折（Anthropic 官方 0.1×）；
/// - `cache_write_tokens`：缓存**写入**，按 cache_write_ratio 加价（官方 1.25×@5m）；
/// - `audio_prompt_tokens`：音频输入，按 audio_ratio 加价（gpt-4o-audio 官方 16×）；
/// - `image_prompt_tokens`：图片输入，按 image_ratio；
/// - 余下 `prompt_uncached()`：常规文本，1.0×。
///
/// completion 侧：`audio_completion_tokens` 按 audio_ratio × audio_completion_ratio
/// （与 new-api 同语义：音频输出相对音频输入再乘一档），余下按 completion_ratio。
///
/// # 维度交叉的近似
///
/// OpenAI 语义中"缓存"与"模态"是**交叉**维度（一段音频 token 也可能被缓存命中），
/// 而计费需要互斥分段。本实现按互斥处理：音频/图片段先从 prompt 扣除，剩余再分
/// 常规/缓存读/缓存写。依据是当前各家缓存均只作用于文本前缀（Anthropic
/// cache_control 仅接受文本块、OpenAI 隐式缓存按文本前缀命中），故交叉部分实测为 0；
/// 若未来上游开放多模态缓存，需改为二维矩阵定价并同步 DESIGN §3.2。
///
/// `reasoning_tokens` 计入 completion 总数（仅统计拆分，不重复计费）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub cached_tokens: u32,
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
    pub reasoning_tokens: u32,
}

impl TokenUsage {
    /// prompt 侧各计价段合计（不含常规文本段）。
    fn prompt_segments(&self) -> u64 {
        u64::from(self.cached_tokens)
            + u64::from(self.cache_write_tokens)
            + u64::from(self.audio_prompt_tokens)
            + u64::from(self.image_prompt_tokens)
    }

    /// 校验不变量；计费入口必须先调用。
    pub fn validate(&self) -> Result<(), DomainError> {
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
        if self.audio_completion_tokens > self.completion_tokens {
            return Err(DomainError::InvalidTokenUsage {
                reason: "audio_completion_tokens > completion_tokens",
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
