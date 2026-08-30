//! token 用量：计费引擎的用量输入。

use crate::error::DomainError;
use serde::{Deserialize, Serialize};

/// 一次请求的 token 用量。
///
/// 不变量：`cached_tokens + cache_write_tokens <= prompt_tokens`，
/// `reasoning_tokens <= completion_tokens`。
///
/// 计费口径（prompt 侧三段互斥，DESIGN §3.2）：
/// - `cached_tokens`：缓存**读取**命中，按 cache_ratio 打折（Anthropic 官方 0.1×）；
/// - `cache_write_tokens`：缓存**写入/创建**，按 cache_write_ratio 加价
///   （Anthropic 官方 1.25×@5m TTL / 2.0×@1h；OpenAI 隐式缓存无此段，恒 0）；
/// - 余下 `prompt_uncached()`：常规输入，1.0×。
///
/// reasoning 计入 completion 总数（仅统计拆分，不重复计费）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub cached_tokens: u32,
    /// 缓存写入 token（Anthropic `cache_creation_input_tokens`）；含在 prompt_tokens 内。
    #[serde(default)]
    pub cache_write_tokens: u32,
    pub completion_tokens: u32,
    pub reasoning_tokens: u32,
}

impl TokenUsage {
    /// 校验不变量；计费入口必须先调用。
    pub fn validate(&self) -> Result<(), DomainError> {
        // 读+写两段都含在 prompt 内且互斥，合计不得越界（否则 prompt_uncached 会被截断为 0，
        // 导致常规输入段静默漏计费）
        if u64::from(self.cached_tokens) + u64::from(self.cache_write_tokens)
            > u64::from(self.prompt_tokens)
        {
            return Err(DomainError::InvalidTokenUsage {
                reason: "cached_tokens + cache_write_tokens > prompt_tokens",
            });
        }
        if self.reasoning_tokens > self.completion_tokens {
            return Err(DomainError::InvalidTokenUsage {
                reason: "reasoning_tokens > completion_tokens",
            });
        }
        Ok(())
    }

    /// 常规输入部分（既非缓存读取、也非缓存写入）。
    #[must_use]
    pub const fn prompt_uncached(&self) -> u32 {
        self.prompt_tokens
            .saturating_sub(self.cached_tokens)
            .saturating_sub(self.cache_write_tokens)
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
}
