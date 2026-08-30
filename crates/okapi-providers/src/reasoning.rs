//! reasoning 模型名后缀（IMPLEMENTATION §4.4，接在别名解析旁）：
//! `-high/-medium/-low`（effort 档）与 `-thinking`/`-thinking-<N>`（思考预算）。
//! 解析顺序由 gateway 保证：模型全名（含别名）直命中优先，未命中才剥后缀重试，
//! 因此真实存在的带后缀模型（如上游确有 `o3-high`）不受影响。
//!
//! 三向注入：
//! - OpenAI：`reasoning_effort`（thinking 预算无标准参数，忽略预算档）；
//! - Anthropic：`thinking: {type: enabled, budget_tokens}`，并保证 max_tokens > 预算；
//! - Gemini：`generationConfig.thinkingConfig.thinkingBudget`（含 includeThoughts）。

use crate::error::UpstreamError;
use bytes::Bytes;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Effort {
    High,
    Medium,
    Low,
}

impl Effort {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// effort 档 → thinking 预算（anthropic/gemini 无 effort 参数时的映射）。
    #[must_use]
    pub fn budget_tokens(self) -> u32 {
        match self {
            Self::High => 16_000,
            Self::Medium => 8_000,
            Self::Low => 2_048,
        }
    }
}

/// 后缀指令：effort 与预算至多其一非空（`-thinking` 无数字 = 缺省预算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningDirective {
    pub effort: Option<Effort>,
    pub budget_tokens: Option<u32>,
}

const DEFAULT_THINKING_BUDGET: u32 = 8_000;
/// anthropic 要求 max_tokens > budget；预算注入时为回答保留的最小余量。
const ANSWER_HEADROOM: u32 = 1_024;

impl ReasoningDirective {
    /// 生效预算（anthropic/gemini 注入用）。
    #[must_use]
    pub fn effective_budget(&self) -> u32 {
        self.budget_tokens
            .or(self.effort.map(Effort::budget_tokens))
            .unwrap_or(DEFAULT_THINKING_BUDGET)
    }
}

/// 剥离 reasoning 后缀：`gpt-x-high` → (`gpt-x`, effort=high)；
/// `claude-y-thinking-4096` → (`claude-y`, budget=4096)。无后缀返回 None。
#[must_use]
pub fn split_reasoning_suffix(name: &str) -> Option<(&str, ReasoningDirective)> {
    for (suffix, effort) in [
        ("-high", Effort::High),
        ("-medium", Effort::Medium),
        ("-low", Effort::Low),
    ] {
        if let Some(base) = name.strip_suffix(suffix)
            && !base.is_empty()
        {
            return Some((
                base,
                ReasoningDirective {
                    effort: Some(effort),
                    budget_tokens: None,
                },
            ));
        }
    }
    if let Some(base) = name.strip_suffix("-thinking")
        && !base.is_empty()
    {
        return Some((
            base,
            ReasoningDirective {
                effort: None,
                budget_tokens: None,
            },
        ));
    }
    // -thinking-<N>
    if let Some(idx) = name.rfind("-thinking-") {
        let (base, rest) = name.split_at(idx);
        let digits = &rest["-thinking-".len()..];
        if !base.is_empty()
            && !digits.is_empty()
            && let Ok(budget) = digits.parse::<u32>()
        {
            return Some((
                base,
                ReasoningDirective {
                    effort: None,
                    budget_tokens: Some(budget),
                },
            ));
        }
    }
    None
}

/// OpenAI 方向注入：effort → `reasoning_effort`（已有同名字段不覆盖——尊重显式请求）。
pub fn apply_openai(body: &Bytes, directive: ReasoningDirective) -> Result<Bytes, UpstreamError> {
    let Some(effort) = directive.effort else {
        return Ok(body.clone()); // 预算档对 OpenAI 无标准参数：原样透传
    };
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    if !obj.contains_key("reasoning_effort") {
        obj.insert("reasoning_effort".into(), json!(effort.as_str()));
    }
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// Anthropic 方向注入（转换产物或透传体均适用）：
/// `thinking: {type: enabled, budget_tokens}`；已带 thinking 字段不覆盖；
/// 预算 ≥ max_tokens 时抬高 max_tokens 保证回答余量（anthropic 硬约束）。
pub fn apply_anthropic(
    body: &Bytes,
    directive: ReasoningDirective,
) -> Result<Bytes, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    if obj.contains_key("thinking") {
        return Ok(body.clone());
    }
    let budget = directive.effective_budget().max(1_024);
    let max_tokens = obj.get("max_tokens").and_then(Value::as_u64).unwrap_or(0);
    let need = u64::from(budget) + u64::from(ANSWER_HEADROOM);
    if max_tokens < need {
        obj.insert("max_tokens".into(), json!(need));
    }
    obj.insert(
        "thinking".into(),
        json!({"type": "enabled", "budget_tokens": budget}),
    );
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// Gemini 方向注入：`generationConfig.thinkingConfig`（已带不覆盖）。
pub fn apply_gemini(body: &Bytes, directive: ReasoningDirective) -> Result<Bytes, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    let cfg = obj.entry("generationConfig").or_insert_with(|| json!({}));
    if let Some(cfg) = cfg.as_object_mut()
        && !cfg.contains_key("thinkingConfig")
    {
        cfg.insert(
            "thinkingConfig".into(),
            json!({
                "thinkingBudget": directive.effective_budget(),
                "includeThoughts": true,
            }),
        );
    }
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}
