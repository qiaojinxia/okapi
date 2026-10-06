//! Native server-tool observations. Counts are not Tokens or resource duration.
use crate::DomainError;
use serde::{Deserialize, Serialize};

/// Provider tag preserves the original usage contract across protocol conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ServerToolUsage {
    Anthropic(AnthropicToolUsage),
}

/// Missing/null is unknown; explicit zero is observed. No inference from tool names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicToolUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_requests: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch_requests: Option<u32>,
    /// Request count only: this is not the separately billed container duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution_requests: Option<u32>,
}

fn invalid() -> DomainError {
    DomainError::InvalidTokenUsage {
        reason: "invalid or regressing server tool usage",
    }
}

impl AnthropicToolUsage {
    pub fn validate(self) -> Result<(), DomainError> {
        for n in [
            self.web_search_requests,
            self.web_fetch_requests,
            self.code_execution_requests,
        ]
        .into_iter()
        .flatten()
        {
            i32::try_from(n).map_err(|_| invalid())?;
        }
        Ok(())
    }

    /// Merge cumulative snapshots; never add replayed deltas or heal a regression.
    pub fn with_previous(self, previous: Self) -> Result<Self, DomainError> {
        self.validate()?;
        previous.validate()?;
        let merge = |next: Option<u32>, before: Option<u32>| {
            if next.zip(before).is_some_and(|(n, p)| n < p) {
                return Err(invalid());
            }
            Ok(next.or(before))
        };
        Ok(Self {
            web_search_requests: merge(self.web_search_requests, previous.web_search_requests)?,
            web_fetch_requests: merge(self.web_fetch_requests, previous.web_fetch_requests)?,
            code_execution_requests: merge(
                self.code_execution_requests,
                previous.code_execution_requests,
            )?,
        })
    }

    /// Independent requests: a complete total requires observation of every input.
    pub fn checked_add(self, other: Self) -> Result<Self, DomainError> {
        self.validate()?;
        other.validate()?;
        let add = |a: Option<u32>, b: Option<u32>| match (a, b) {
            (Some(a), Some(b)) => a
                .checked_add(b)
                .filter(|n| i32::try_from(*n).is_ok())
                .map(Some)
                .ok_or_else(invalid),
            _ => Ok(None),
        };
        Ok(Self {
            web_search_requests: add(self.web_search_requests, other.web_search_requests)?,
            web_fetch_requests: add(self.web_fetch_requests, other.web_fetch_requests)?,
            code_execution_requests: add(
                self.code_execution_requests,
                other.code_execution_requests,
            )?,
        })
    }
}

impl ServerToolUsage {
    pub fn validate(self) -> Result<(), DomainError> {
        match self {
            Self::Anthropic(u) => u.validate(),
        }
    }

    pub fn checked_add(self, other: Self) -> Result<Self, DomainError> {
        match (self, other) {
            (Self::Anthropic(a), Self::Anthropic(b)) => a.checked_add(b).map(Self::Anthropic),
        }
    }

    pub fn with_previous(self, previous: Self) -> Result<Self, DomainError> {
        match (self, previous) {
            (Self::Anthropic(a), Self::Anthropic(b)) => a.with_previous(b).map(Self::Anthropic),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenUsage;

    fn tools(search: Option<u32>, fetch: Option<u32>) -> ServerToolUsage {
        ServerToolUsage::Anthropic(AnthropicToolUsage {
            web_search_requests: search,
            web_fetch_requests: fetch,
            code_execution_requests: None,
        })
    }

    #[test]
    fn execution_counts_keep_cumulative_and_independent_request_completeness() {
        let execution = |count| AnthropicToolUsage {
            code_execution_requests: count,
            ..AnthropicToolUsage::default()
        };
        let before = execution(Some(2));
        assert_eq!(before.with_previous(before).unwrap(), before);
        assert_eq!(execution(None).with_previous(before).unwrap(), before);
        assert!(execution(Some(1)).with_previous(before).is_err());
        assert_eq!(
            before.checked_add(execution(Some(3))).unwrap(),
            execution(Some(5))
        );
        assert_eq!(
            before.checked_add(execution(None)).unwrap(),
            execution(None)
        );
        assert_eq!(
            execution(Some(0)).checked_add(execution(Some(0))).unwrap(),
            execution(Some(0))
        );
        assert!(execution(Some(2_147_483_648)).validate().is_err());
        assert!(
            execution(Some(2_147_483_647))
                .checked_add(execution(Some(1)))
                .is_err()
        );
        let usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            server_tool_usage: Some(ServerToolUsage::Anthropic(before)),
            ..TokenUsage::default()
        };
        assert_eq!(
            (
                usage.total_raw(),
                usage.prompt_uncached(),
                usage.text_completion()
            ),
            (150, 100, 50)
        );
    }

    #[test]
    fn independent_request_totals_require_observation_of_every_axis() -> Result<(), DomainError> {
        assert_eq!(
            tools(Some(2), Some(3)).checked_add(tools(Some(4), None))?,
            tools(Some(6), None)
        );
        assert_eq!(
            tools(Some(0), Some(0)).checked_add(tools(Some(0), Some(0)))?,
            tools(Some(0), Some(0))
        );
        let a = TokenUsage {
            prompt_tokens: 100,
            server_tool_usage: Some(tools(Some(2), Some(3))),
            ..TokenUsage::default()
        };
        let total = a.checked_add(TokenUsage {
            completion_tokens: 50,
            ..TokenUsage::default()
        })?;
        assert_eq!(total.server_tool_usage, None);
        assert_eq!(total.total_raw(), 150);
        Ok(())
    }

    #[test]
    fn cumulative_counts_replace_and_missing_axes_retain_prior_observation()
    -> Result<(), DomainError> {
        let before = tools(Some(2), Some(3));
        assert_eq!(before.with_previous(before)?, before);
        assert_eq!(
            tools(None, Some(4)).with_previous(before)?,
            tools(Some(2), Some(4))
        );
        assert_eq!(
            tools(Some(0), None).with_previous(tools(None, None))?,
            tools(Some(0), None)
        );
        assert!(tools(Some(1), None).with_previous(before).is_err());
        assert!(tools(None, Some(0)).with_previous(before).is_err());
        Ok(())
    }

    #[test]
    fn storage_bounds_and_independent_sum_overflow_are_rejected() {
        let max = 2_147_483_647_u32;
        assert!(tools(Some(max), Some(max)).validate().is_ok());
        assert!(tools(Some(max + 1), None).validate().is_err());
        assert!(
            tools(Some(max), None)
                .checked_add(tools(Some(1), None))
                .is_err()
        );
        assert!(
            tools(None, Some(max))
                .checked_add(tools(None, Some(1)))
                .is_err()
        );
    }

    #[test]
    fn tools_do_not_enter_token_totals_segments_or_provenance() {
        for count in [0, 1, 8000, 2_147_483_647] {
            let usage = TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                server_tool_usage: Some(tools(Some(count), None)),
                ..TokenUsage::default()
            };
            assert!(usage.validate().is_ok());
            assert_eq!(
                (
                    usage.total_raw(),
                    usage.prompt_uncached(),
                    usage.text_completion()
                ),
                (150, 100, 50)
            );
            assert_eq!(
                (usage.prompt_source(), usage.completion_source()),
                ("unknown", "unknown")
            );
        }
    }
}
