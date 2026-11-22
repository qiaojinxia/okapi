//! Curated request contracts, not administrator capability declarations.
//! Checked 2026-10-01 against the providers' docs (see docs/model-parameters.md).
//! Claude Fable / Mythos 5.2 were added 2026-11-09 from platform.claude.com/docs/en/models/fable-5-2/whats-new-fable-5-2.
//! Match exact upstream IDs; unknown/custom aliases remain passthrough.

use crate::{ReasoningDirective, UpstreamError};
use bytes::Bytes;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Serialize)]
// Independent capabilities in an API contract, not state-machine flags.
#[allow(clippy::struct_excessive_bools)]
pub struct ParameterProfile {
    pub known: bool,
    pub temperature_max: Option<f64>,
    pub top_p: bool,
    pub sampling_requires_none: bool,
    pub sampling_requires_no_budget: bool,
    pub efforts: Vec<&'static str>,
    pub default_effort: Option<&'static str>,
    pub budget_min: Option<u32>,
    pub budget_max: Option<u32>,
    pub preserve_reasoning: bool,
    #[serde(skip)]
    mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Passthrough,
    OpenAi,
    Kimi,
    ClaudeEffort,
    Budget,
    GeminiLevel,
}

impl Default for ParameterProfile {
    fn default() -> Self {
        Self {
            known: false,
            temperature_max: Some(2.0),
            top_p: true,
            sampling_requires_none: false,
            sampling_requires_no_budget: false,
            efforts: vec![],
            default_effort: None,
            budget_min: None,
            budget_max: None,
            preserve_reasoning: false,
            mode: Mode::Passthrough,
        }
    }
}

impl ParameterProfile {
    /// Safe common choices across routing/failover candidates. Unknown rules do
    /// not assert support for effort or budget. Never disclose channel credentials.
    pub fn intersect(&mut self, other: &Self) {
        self.known &= other.known;
        self.temperature_max = self
            .temperature_max
            .zip(other.temperature_max)
            .map(|(a, b)| a.min(b));
        self.top_p &= other.top_p;
        self.sampling_requires_none |= other.sampling_requires_none;
        self.sampling_requires_no_budget |= other.sampling_requires_no_budget;
        self.efforts.retain(|e| other.efforts.contains(e));
        if self.default_effort != other.default_effort {
            self.default_effort = None;
        }
        self.budget_min = self.budget_min.zip(other.budget_min).map(|(a, b)| a.max(b));
        self.budget_max = self.budget_max.zip(other.budget_max).map(|(a, b)| a.min(b));
        if self
            .budget_min
            .zip(self.budget_max)
            .is_some_and(|(a, b)| a > b)
        {
            self.budget_min = None;
            self.budget_max = None;
        }
        self.preserve_reasoning |= other.preserve_reasoning;
    }

    /// Playground input validation. Explicit incompatible choices are rejected,
    /// not silently coerced; omitted parameters follow the upstream default.
    pub fn validate(&self, value: &Value) -> Result<(), &'static str> {
        let effort = value.get("reasoning_effort").and_then(Value::as_str);
        if value.get("reasoning_effort").is_some()
            && effort.is_none_or(|e| !self.efforts.contains(&e))
        {
            return Err("reasoning_effort");
        }
        let sampling = (!self.sampling_requires_none
            || effort.or(self.default_effort) == Some("none"))
            && (!self.sampling_requires_no_budget
                || value.pointer("/reasoning/max_tokens").is_none());
        for (key, max) in [
            ("temperature", self.temperature_max),
            ("top_p", self.top_p.then_some(1.0)),
        ] {
            if let Some(v) = value.get(key)
                && (!sampling
                    || !v
                        .as_f64()
                        .zip(max)
                        .is_some_and(|(n, m)| n.is_finite() && n >= 0.0 && n <= m))
            {
                return Err(key);
            }
        }
        if let Some(n) = value.pointer("/reasoning/max_tokens") {
            if !n
                .as_u64()
                .zip(self.budget_min.zip(self.budget_max))
                .is_some_and(|(n, (a, b))| n >= u64::from(a) && n <= u64::from(b))
            {
                return Err("reasoning.max_tokens");
            }
            if self.mode == Mode::Budget
                && value
                    .get("max_tokens")
                    .and_then(Value::as_u64)
                    .is_some_and(|cap| n.as_u64().is_some_and(|budget| cap <= budget))
            {
                return Err("max_tokens");
            }
        }
        if let Some(n) = value.get("max_tokens")
            && !n.as_u64().is_some_and(|n| n > 0 && n <= 1_048_576)
        {
            return Err("max_tokens");
        }
        Ok(())
    }
}

/// Models before the 4.6 generation are addressed by dated snapshot IDs on the
/// Claude API (the dateless name is only an alias). Map the official snapshots
/// to their alias so both spellings get the same contract; still an exact list.
fn claude_alias(model: &str) -> &str {
    match model {
        "claude-haiku-4-5-20251001" => "claude-haiku-4-5",
        "claude-sonnet-4-5-20250929" => "claude-sonnet-4-5",
        "claude-opus-4-5-20251101" => "claude-opus-4-5",
        "claude-sonnet-4-20250514" => "claude-sonnet-4",
        "claude-opus-4-20250514" => "claude-opus-4",
        other => other,
    }
}

fn effort(mode: Mode, levels: &[&'static str], default: &'static str) -> ParameterProfile {
    ParameterProfile {
        known: true,
        efforts: levels.to_vec(),
        default_effort: Some(default),
        mode,
        ..ParameterProfile::default()
    }
}

#[must_use]
// Keep the exact-ID contract table together for review against provider docs.
#[allow(clippy::too_many_lines)]
pub fn profile(dialect: &str, model: &str, api_base: Option<&str>) -> ParameterProfile {
    // The Coding Plan's k3 alias is not a generic model called "k3".
    let kimi_k3 = model == "kimi-k3"
        || (model == "k3"
            && api_base
                .is_some_and(|b| b.trim_end_matches('/') == "https://api.kimi.com/coding/v1"));
    if dialect == "openai" && kimi_k3 {
        return ParameterProfile {
            temperature_max: None,
            top_p: false,
            preserve_reasoning: true,
            ..effort(Mode::Kimi, &["low", "high", "max"], "max")
        };
    }
    if dialect == "openai" {
        let mut p = match model {
            "gpt-6-astra" | "gpt-6.1-sol" => effort(
                Mode::OpenAi,
                &["low", "medium", "high", "xhigh", "max"],
                "medium",
            ),
            "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => {
                effort(
                    Mode::OpenAi,
                    &["none", "low", "medium", "high", "xhigh", "max"],
                    "medium",
                )
            }
            "gpt-5.5" => effort(
                Mode::OpenAi,
                &["none", "low", "medium", "high", "xhigh"],
                "medium",
            ),
            "gpt-5.2" | "gpt-5.4" => effort(
                Mode::OpenAi,
                &["none", "low", "medium", "high", "xhigh"],
                "none",
            ),
            "gpt-5.1" => effort(Mode::OpenAi, &["none", "low", "medium", "high"], "none"),
            "gpt-5" | "gpt-5-mini" | "gpt-5-nano" => effort(
                Mode::OpenAi,
                &["minimal", "low", "medium", "high"],
                "medium",
            ),
            "o3" | "o3-mini" | "o4-mini" => {
                effort(Mode::OpenAi, &["low", "medium", "high"], "medium")
            }
            _ => return ParameterProfile::default(),
        };
        p.sampling_requires_none = true;
        if !p.efforts.contains(&"none") {
            p.temperature_max = None;
            p.top_p = false;
        }
        return p;
    }
    if dialect == "anthropic" {
        let model = claude_alias(model);
        let levels: &[&'static str] = match model {
            "claude-fable-5-2" | "claude-fable-5-1" | "claude-fable-5" | "claude-mythos-5-2"
            | "claude-mythos-5-1" | "claude-mythos-5" | "claude-opus-5-5" | "claude-opus-5"
            | "claude-opus-4-8" | "claude-opus-4-7" | "claude-sonnet-5-5" | "claude-sonnet-5" => {
                &["low", "medium", "high", "xhigh", "max"]
            }
            "claude-opus-4-6" | "claude-sonnet-4-6" => &["low", "medium", "high", "max"],
            "claude-opus-4-5" => &["low", "medium", "high"],
            "claude-sonnet-4-5" | "claude-sonnet-4" | "claude-opus-4" | "claude-haiku-4-5" => {
                return ParameterProfile {
                    known: true,
                    temperature_max: Some(1.0),
                    sampling_requires_no_budget: true,
                    budget_min: Some(1024),
                    budget_max: Some(32000),
                    mode: Mode::Budget,
                    ..ParameterProfile::default()
                };
            }
            _ => return ParameterProfile::default(),
        };
        // Adaptive thinking uses effort, not a fabricated fixed budget.
        return ParameterProfile {
            temperature_max: None,
            top_p: false,
            ..effort(
                Mode::ClaudeEffort,
                levels,
                if model == "claude-opus-5-5" {
                    "medium"
                } else {
                    "high"
                },
            )
        };
    }
    if dialect == "gemini" {
        let (levels, default): (&[&'static str], _) = match model {
            "gemini-3.8-flash" | "gemini-3.5-flash" => (&["low", "medium", "high"], "medium"),
            "gemini-3.1-pro-preview" | "gemini-3-pro-preview" => {
                (&["low", "medium", "high"], "high")
            }
            "gemini-3.1-flash-lite" | "gemini-3.1-flash-lite-preview" => {
                (&["minimal", "low", "medium", "high"], "minimal")
            }
            "gemini-3-flash-preview" => (&["minimal", "low", "medium", "high"], "high"),
            "gemini-3.1-flash-image-preview" => (&["minimal", "high"], "high"),
            "gemini-2.5-pro" | "gemini-2.5-flash" | "gemini-2.5-flash-lite" => {
                return ParameterProfile {
                    known: true,
                    budget_min: Some(if model == "gemini-2.5-pro" {
                        128
                    } else if model == "gemini-2.5-flash-lite" {
                        512
                    } else {
                        1
                    }),
                    budget_max: Some(if model == "gemini-2.5-pro" {
                        32768
                    } else {
                        24576
                    }),
                    mode: Mode::Budget,
                    ..ParameterProfile::default()
                };
            }
            _ => return ParameterProfile::default(),
        };
        return effort(Mode::GeminiLevel, levels, default);
    }
    ParameterProfile::default()
}

/// Model-aware effort translation before the legacy budget translator. Returns
/// None for unknown/budget models, preserving the gateway's existing behavior.
pub fn apply_effort(
    profile: &ParameterProfile,
    dialect: &str,
    built: &Bytes,
    source: &Bytes,
    directive: Option<ReasoningDirective>,
    native_responses: bool,
) -> Option<Result<Bytes, UpstreamError>> {
    let budget_anthropic = profile.mode == Mode::Budget && dialect == "anthropic";
    if budget_anthropic && directive.is_none() {
        return None;
    }
    if matches!(profile.mode, Mode::Passthrough | Mode::Budget) && !budget_anthropic {
        return None;
    }
    let original: Value = match serde_json::from_slice(source) {
        Ok(v) => v,
        Err(_) if budget_anthropic => return None,
        Err(e) => return Some(Err(UpstreamError::Build(e.to_string()))),
    };
    let mut value: Value = match serde_json::from_slice(built) {
        Ok(v) => v,
        Err(e) => return Some(Err(UpstreamError::Build(e.to_string()))),
    };
    match apply_effort_value(
        profile,
        dialect,
        &mut value,
        &original,
        directive,
        native_responses,
    )? {
        Ok(false) => Some(Ok(built.clone())),
        Ok(true) => Some(
            serde_json::to_vec(&value)
                .map(Bytes::from)
                .map_err(|e| UpstreamError::Build(e.to_string())),
        ),
        Err(err) => Some(Err(err)),
    }
}

/// Pre-computed mutation plan for [`apply_effort`], enabling copy-on-write bodies to inspect
/// without cloning unless a mutation is needed.
pub enum EffortPlan {
    Fallback,
    Unchanged,
    PreserveBudget {
        cap: u64,
        directive: ReasoningDirective,
    },
    Apply {
        level: String,
        keep_native: bool,
    },
}

/// Inspect `current` and `source` without mutating or cloning `current`.
pub fn plan_effort(
    profile: &ParameterProfile,
    dialect: &str,
    current: &Value,
    source: &Value,
    directive: Option<ReasoningDirective>,
    native_responses: bool,
) -> Result<EffortPlan, UpstreamError> {
    if profile.mode == Mode::Budget && dialect == "anthropic" {
        let Some(d) = directive else {
            return Ok(EffortPlan::Fallback);
        };
        let Some(cap) = source
            .get("max_completion_tokens")
            .or_else(|| source.get("max_tokens"))
            .and_then(Value::as_u64)
        else {
            return Ok(EffortPlan::Fallback);
        };
        if cap <= u64::from(d.effective_budget().max(1024)) {
            return Err(UpstreamError::Build(
                "max_tokens_must_exceed_thinking_budget".into(),
            ));
        }
        let needs_thinking = current
            .as_object()
            .is_none_or(|o| !o.contains_key("thinking"));
        let needs_cap = current.get("max_tokens").and_then(Value::as_u64) != Some(cap);
        if !needs_thinking && !needs_cap {
            return Ok(EffortPlan::Unchanged);
        }
        return Ok(EffortPlan::PreserveBudget { cap, directive: d });
    }
    if matches!(profile.mode, Mode::Passthrough | Mode::Budget) {
        return Ok(EffortPlan::Fallback);
    }
    let unified = source
        .pointer("/reasoning/effort")
        .or_else(|| source.get("reasoning_effort"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    // 同方言的原生写法：客户端自己写了 `output_config.effort`（Claude Code 一直带着）是显式选择，
    // 原样保留。`thinking` 对象折出的档位只是缺省猜测（没有预算时恒为 medium），不能压过它；
    // 上游认不认这个档位也由上游裁决，这张表可能落后于上游，不替它拒。统一字段另有明说才改写。
    let keep_native = dialect == "anthropic"
        && unified.is_none()
        && source
            .pointer("/output_config/effort")
            .is_some_and(Value::is_string);
    // Gemini 同方言原生 `thinkingConfig` 同样原样保留，不用这张表替上游拒或覆盖
    if dialect == "gemini"
        && unified.is_none()
        && source
            .get("generationConfig")
            .or_else(|| source.get("generation_config"))
            .and_then(|c| c.get("thinkingConfig").or_else(|| c.get("thinking_config")))
            .is_some_and(Value::is_object)
        && current.get("reasoning_effort").is_none()
        && current.get("reasoning").is_none()
    {
        return Ok(EffortPlan::Unchanged);
    }
    let level = unified.or_else(|| directive.map(|d| d.effective_effort().as_str().to_owned()));
    let Some(level) = level else {
        return Ok(EffortPlan::Unchanged);
    };
    if !keep_native && !profile.efforts.contains(&level.as_str()) {
        return Err(UpstreamError::Status {
            status: 400,
            body: Bytes::from(
                serde_json::to_vec(&json!({"error": {
                    "type": "invalid_request_error",
                    "code": "invalid_request_error",
                    "param": "reasoning_effort",
                    "message": format!("Unsupported reasoning effort: {level}")
                }}))
                .unwrap_or_default(),
            ),
            retry_after_secs: None,
        });
    }
    if !current.is_object() {
        return Err(UpstreamError::Build("invalid_body".into()));
    }
    if dialect == "anthropic"
        && keep_native
        && current.get("reasoning_effort").is_none()
        && (native_responses || current.get("reasoning").is_none())
        && (source.get("thinking").is_some()
            || current
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|m| claude_alias(m) == "claude-opus-4-5"))
    {
        return Ok(EffortPlan::Unchanged);
    }
    Ok(EffortPlan::Apply { level, keep_native })
}

/// Apply a pre-computed [`EffortPlan`] in-place on `v`.
pub fn apply_effort_plan(
    plan: EffortPlan,
    dialect: &str,
    v: &mut Value,
    source: &Value,
    native_responses: bool,
) -> Result<bool, UpstreamError> {
    match plan {
        EffortPlan::Fallback | EffortPlan::Unchanged => Ok(false),
        EffortPlan::PreserveBudget { cap, directive } => {
            let needs_cap = v.get("max_tokens").and_then(Value::as_u64) != Some(cap);
            let changed = crate::reasoning::apply_anthropic_value(v, directive)?;
            v["max_tokens"] = json!(cap);
            Ok(changed || needs_cap)
        }
        EffortPlan::Apply { level, keep_native } => {
            let obj = v
                .as_object_mut()
                .ok_or_else(|| UpstreamError::Build("invalid_body".into()))?;
            obj.remove("reasoning_effort");
            if !native_responses {
                obj.remove("reasoning");
            }
            match dialect {
                "anthropic" => {
                    if !keep_native {
                        obj.entry("output_config")
                            .or_insert_with(|| json!({}))
                            .as_object_mut()
                            .ok_or_else(|| UpstreamError::Build("output_config".into()))?
                            .insert("effort".into(), json!(level));
                    }
                    // Opus 4.5 supports effort independently of thinking; newer
                    // models support adaptive. Don't overwrite explicit native thinking.
                    if source.get("thinking").is_none()
                        && obj
                            .get("model")
                            .and_then(Value::as_str)
                            .is_none_or(|m| claude_alias(m) != "claude-opus-4-5")
                    {
                        obj.insert("thinking".into(), json!({"type": "adaptive"}));
                    }
                }
                "gemini" => {
                    let config = obj.entry("generationConfig").or_insert_with(|| json!({}));
                    let thinking = config
                        .as_object_mut()
                        .ok_or_else(|| UpstreamError::Build("generationConfig".into()))?
                        .entry("thinkingConfig")
                        .or_insert_with(|| json!({}));
                    let thinking = thinking
                        .as_object_mut()
                        .ok_or_else(|| UpstreamError::Build("thinkingConfig".into()))?;
                    if !thinking.contains_key("thinkingLevel")
                        && !thinking.contains_key("thinkingBudget")
                    {
                        thinking.insert("thinkingLevel".into(), json!(level));
                        thinking.insert("includeThoughts".into(), json!(true));
                    }
                }
                _ if native_responses => {
                    obj.entry("reasoning")
                        .or_insert_with(|| json!({}))
                        .as_object_mut()
                        .ok_or_else(|| UpstreamError::Build("reasoning".into()))?
                        .insert("effort".into(), json!(level));
                }
                _ => {
                    obj.insert("reasoning_effort".into(), json!(level));
                }
            }
            // Reasoning models use the total completion cap, not the deprecated
            // visible-text-only max_tokens. Never increase an explicit cap.
            if dialect == "openai"
                && !native_responses
                && let Some(cap) = obj.remove("max_tokens")
            {
                obj.entry("max_completion_tokens").or_insert(cap);
            }
            Ok(true)
        }
    }
}

/// 同 [`apply_effort`]，只在需要改写时克隆一次。
pub fn apply_effort_value(
    profile: &ParameterProfile,
    dialect: &str,
    built: &mut Value,
    source: &Value,
    directive: Option<ReasoningDirective>,
    native_responses: bool,
) -> Option<Result<bool, UpstreamError>> {
    let plan = match plan_effort(profile, dialect, built, source, directive, native_responses) {
        Ok(EffortPlan::Fallback) => return None,
        Ok(EffortPlan::Unchanged) => return Some(Ok(false)),
        Ok(plan) => plan,
        Err(err) => return Some(Err(err)),
    };
    Some(apply_effort_plan(
        plan,
        dialect,
        built,
        source,
        native_responses,
    ))
}

/// 判断是否需要把 `max_tokens` 转写为 `max_completion_tokens`（零写预检查用）。
#[must_use]
pub fn needs_completion_cap(
    profile: &ParameterProfile,
    dialect: &str,
    value: &Value,
    native_responses: bool,
) -> bool {
    dialect == "openai"
        && !native_responses
        && matches!(profile.mode, Mode::OpenAi | Mode::Kimi)
        && value
            .as_object()
            .is_some_and(|o| o.contains_key("max_tokens"))
}

/// 在已解析请求体上原地执行 `max_tokens` → `max_completion_tokens`；无改动返回 `Ok(false)`。
pub fn completion_cap_value(
    profile: &ParameterProfile,
    dialect: &str,
    value: &mut Value,
    native_responses: bool,
) -> Result<bool, UpstreamError> {
    if !needs_completion_cap(profile, dialect, value, native_responses) {
        return Ok(false);
    }
    let Some(obj) = value.as_object_mut() else {
        return Ok(false);
    };
    let Some(cap) = obj.remove("max_tokens") else {
        return Ok(false);
    };
    obj.entry("max_completion_tokens").or_insert(cap);
    Ok(true)
}

/// Translate a total output cap even when effort is omitted (follow model).
pub fn completion_cap(
    profile: &ParameterProfile,
    dialect: &str,
    body: Bytes,
    native_responses: bool,
) -> Result<Bytes, UpstreamError> {
    if dialect != "openai" || native_responses || !matches!(profile.mode, Mode::OpenAi | Mode::Kimi)
    {
        return Ok(body);
    }
    let mut v: Value =
        serde_json::from_slice(&body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    if !completion_cap_value(profile, dialect, &mut v, native_responses)? {
        return Ok(body);
    }
    serde_json::to_vec(&v)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dated_claude_snapshots_share_their_alias_contract() {
        for (dated, alias) in [
            ("claude-haiku-4-5-20251001", "claude-haiku-4-5"),
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            ("claude-opus-4-5-20251101", "claude-opus-4-5"),
        ] {
            let p = profile("anthropic", dated, None);
            assert!(p.known, "{dated}");
            assert_eq!(p, profile("anthropic", alias, None), "{dated}");
        }
        // Not a prefix rule: an unknown snapshot stays passthrough.
        assert!(!profile("anthropic", "claude-haiku-4-5-20990101", None).known);
    }

    /// Fable 5.2 / Mythos 5.2 (released 2026-11-09, platform.claude.com/docs/en/models/fable-5-2/whats-new-fable-5-2):
    /// all five effort levels with `high` as the API default, adaptive thinking only, so no sampling parameters —
    /// the same contract as Fable 5.1. A later minor version that is not in the table stays passthrough.
    #[test]
    fn fable_and_mythos_5_2_share_the_5_1_contract() {
        let fable_5_1 = profile("anthropic", "claude-fable-5-1", None);
        assert!(fable_5_1.known);
        for model in ["claude-fable-5-2", "claude-mythos-5-2"] {
            let p = profile("anthropic", model, None);
            assert!(p.known, "{model}");
            assert_eq!(
                p.efforts,
                ["low", "medium", "high", "xhigh", "max"],
                "{model}"
            );
            assert_eq!(p.default_effort, Some("high"), "{model}");
            assert_eq!(p, fable_5_1, "{model}");
            assert_eq!(
                p.validate(&json!({"temperature": 0.5})),
                Err("temperature"),
                "{model}"
            );
            assert_eq!(
                p.validate(&json!({"reasoning_effort": "minimal"})),
                Err("reasoning_effort"),
                "{model}"
            );
            assert!(
                p.validate(&json!({"reasoning_effort": "xhigh"})).is_ok(),
                "{model}"
            );
        }
        assert!(!profile("anthropic", "claude-fable-5-3", None).known);
    }

    #[test]
    fn kimi_fixed_sampling_and_exact_alias() {
        let p = profile("openai", "k3", Some("https://api.kimi.com/coding/v1"));
        assert_eq!(p.efforts, ["low", "high", "max"]);
        assert_eq!(p.validate(&json!({"temperature":1})), Err("temperature"));
        assert_eq!(p.validate(&json!({"top_p":1})), Err("top_p"));
        assert_eq!(
            p.validate(&json!({"reasoning_effort":"medium"})),
            Err("reasoning_effort")
        );
        assert!(!profile("openai", "k3", Some("https://custom.invalid")).known);
        assert!(!profile("openai", "my-gpt-6", None).known);
    }
    #[test]
    fn sampling_depends_on_effective_effort() {
        let p = profile("openai", "gpt-6-sol", None);
        assert!(p.validate(&json!({})).is_ok());
        assert_eq!(p.validate(&json!({"temperature":0.5})), Err("temperature"));
        assert!(
            p.validate(&json!({"reasoning_effort":"none", "temperature":0.5}))
                .is_ok()
        );
    }
    #[test]
    fn native_levels_are_not_fabricated_budgets() {
        for (dialect, model, effort, pointer) in [
            (
                "anthropic",
                "claude-opus-4-6",
                "max",
                "/output_config/effort",
            ),
            (
                "gemini",
                "gemini-3.8-flash",
                "medium",
                "/generationConfig/thinkingConfig/thinkingLevel",
            ),
        ] {
            let source = Bytes::from(
                json!({"model":model, "reasoning_effort":effort, "max_tokens":1234}).to_string(),
            );
            let out = apply_effort(
                &profile(dialect, model, None),
                dialect,
                &source,
                &source,
                None,
                false,
            )
            .unwrap()
            .unwrap();
            let v: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(v.pointer(pointer), Some(&json!(effort)));
            assert!(v.get("reasoning_effort").is_none());
            assert!(v.pointer("/thinking/budget_tokens").is_none());
            assert!(
                v.pointer("/generationConfig/thinkingConfig/thinkingBudget")
                    .is_none()
            );
            assert_eq!(v["max_tokens"], 1234);
        }
    }
    #[test]
    fn defaults_and_caps_survive_and_intersections_are_conservative() {
        let mut p = profile("openai", "kimi-k3", None);
        let body = Bytes::from_static(br#"{"model":"kimi-k3","max_tokens":256}"#);
        let v: Value =
            serde_json::from_slice(&completion_cap(&p, "openai", body, false).unwrap()).unwrap();
        assert_eq!(v["max_completion_tokens"], 256);
        assert!(v.get("reasoning_effort").is_none());
        p.intersect(&profile("openai", "gpt-6-astra", None));
        assert_eq!(p.efforts, ["low", "high", "max"]);
        assert_eq!(p.default_effort, None);
        p.intersect(&ParameterProfile::default());
        assert!(p.efforts.is_empty(), "p.efforts should be empty");
    }

    #[test]
    fn explicit_budget_does_not_enlarge_explicit_output_cap() {
        let body = Bytes::from_static(br#"{"model":"claude-sonnet-4-5","max_tokens":2049}"#);
        let d = ReasoningDirective {
            effort: None,
            budget_tokens: Some(2048),
        };
        let out = apply_effort(
            &profile("anthropic", "claude-sonnet-4-5", None),
            "anthropic",
            &body,
            &body,
            Some(d),
            false,
        )
        .unwrap()
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["max_tokens"], 2049);
        assert_eq!(v["thinking"]["budget_tokens"], 2048);
    }

    /// 同方言透传一份原生 Messages 请求（built = source），指令按网关的做法从请求体解析。
    fn native_messages(model: &str, source: &Value) -> Result<Value, UpstreamError> {
        let bytes = Bytes::from(source.to_string());
        let directive = crate::reasoning::parse_request_value(source).merge(None);
        apply_effort(
            &profile("anthropic", model, None),
            "anthropic",
            &bytes,
            &bytes,
            directive,
            false,
        )
        .expect("按档位的 Claude 模型走 apply_effort")
        .map(|out| serde_json::from_slice(&out).unwrap())
    }

    /// Claude Code 的原生请求：`thinking:{adaptive}` + `output_config.effort`。此前 `thinking` 对象
    /// 折出的缺省档（没有预算时恒为 medium）把客户端自己选的档位整个盖掉：`/effort high`、`max`
    /// 一律变 medium，`low` 反而更费。
    #[test]
    fn native_effort_survives_adaptive_thinking() {
        let all: &[&str] = &["low", "medium", "high", "xhigh", "max"];
        for (model, efforts) in [
            ("claude-sonnet-5-5", all),
            ("claude-opus-5-5", all),
            ("claude-fable-5-2", all),
            ("claude-opus-4-6", &all[..3]),
        ] {
            for effort in efforts {
                let source = json!({
                    "model": model, "max_tokens": 4096,
                    "thinking": {"type": "adaptive", "display": "updates"},
                    "output_config": {"effort": effort},
                    "messages": [{"role": "user", "content": "hi"}],
                });
                let out = native_messages(model, &source).unwrap();
                assert_eq!(out["output_config"]["effort"], *effort, "{model}");
                assert_eq!(
                    out["thinking"], source["thinking"],
                    "{model}: thinking 原样"
                );
            }
        }
    }

    /// 原生档位是客户端对上游说的话：这张表可能落后于上游，不替上游拒（上游不认会自己 400）。
    #[test]
    fn native_effort_is_not_judged_by_the_table() {
        let source = json!({
            "model": "claude-opus-4-6", "max_tokens": 4096,
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "xhigh"},
            "messages": [],
        });
        assert!(
            !profile("anthropic", "claude-opus-4-6", None)
                .efforts
                .contains(&"xhigh")
        );
        let out = native_messages("claude-opus-4-6", &source).unwrap();
        assert_eq!(out["output_config"]["effort"], "xhigh");
    }

    /// 模型名后缀负责打开思考，档位仍是客户端原生写的那个。
    #[test]
    fn native_effort_is_kept_when_a_suffix_enables_thinking() {
        let source = json!({
            "model": "claude-sonnet-5-5", "max_tokens": 4096,
            "output_config": {"effort": "max"},
            "messages": [],
        });
        let bytes = Bytes::from(source.to_string());
        let suffix = ReasoningDirective {
            effort: Some(crate::reasoning::Effort::Low),
            budget_tokens: None,
        };
        let out = apply_effort(
            &profile("anthropic", "claude-sonnet-5-5", None),
            "anthropic",
            &bytes,
            &bytes,
            Some(suffix),
            false,
        )
        .unwrap()
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["output_config"]["effort"], "max");
        assert_eq!(v["thinking"], json!({"type": "adaptive"}));
    }

    /// 只写 effort、没开思考的原生请求不被动：不替客户端打开 thinking。
    #[test]
    fn native_effort_without_thinking_is_untouched() {
        let source = json!({
            "model": "claude-opus-5-5", "max_tokens": 4096,
            "output_config": {"effort": "high"},
            "messages": [],
        });
        let out = native_messages("claude-opus-5-5", &source).unwrap();
        assert_eq!(out, source);
    }

    /// 统一写法（`reasoning_effort` / `reasoning.effort`）是另一种显式选择：照旧压过原生字段并受这张表约束。
    #[test]
    fn unified_effort_still_wins_over_the_native_field() {
        let source = json!({
            "model": "claude-sonnet-5-5", "max_tokens": 4096,
            "thinking": {"type": "adaptive"},
            "reasoning_effort": "high",
            "output_config": {"effort": "low"},
            "messages": [],
        });
        let out = native_messages("claude-sonnet-5-5", &source).unwrap();
        assert_eq!(out["output_config"]["effort"], "high");
        assert!(out.get("reasoning_effort").is_none());

        let rejected = json!({
            "model": "claude-sonnet-5-5", "max_tokens": 4096,
            "reasoning_effort": "minimal",
            "output_config": {"effort": "low"},
            "messages": [],
        });
        assert!(matches!(
            native_messages("claude-sonnet-5-5", &rejected),
            Err(UpstreamError::Status { status: 400, .. })
        ));
    }

    /// 没有原生档位时，预算式 thinking 仍折成档位（新模型不收预算）；`thinking` 对象原样留着。
    #[test]
    fn budget_thinking_without_native_effort_still_translates() {
        let source = json!({
            "model": "claude-opus-5-5", "max_tokens": 64000,
            "thinking": {"type": "enabled", "budget_tokens": 16000},
            "messages": [],
        });
        let out = native_messages("claude-opus-5-5", &source).unwrap();
        assert_eq!(out["output_config"]["effort"], "high");
        assert_eq!(out["thinking"], source["thinking"]);
    }

    #[test]
    fn native_adaptive_effort_is_zero_copy_passthrough() {
        let raw = br#"{"messages":[{"role":"user","content":"hi"}],"output_config":{"effort":"high"},"thinking":{"type":"adaptive"},"max_tokens":4096,"model":"claude-sonnet-5-5"}"#;
        let bytes = Bytes::from_static(raw);
        let source: Value = serde_json::from_slice(&bytes).unwrap();
        let directive = crate::reasoning::parse_request_value(&source).merge(None);
        let out = apply_effort(
            &profile("anthropic", "claude-sonnet-5-5", None),
            "anthropic",
            &bytes,
            &bytes,
            directive,
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.as_ref(), raw);
        let plan = plan_effort(
            &profile("anthropic", "claude-sonnet-5-5", None),
            "anthropic",
            &source,
            &source,
            directive,
            false,
        )
        .unwrap();
        assert!(matches!(plan, EffortPlan::Unchanged));
    }

    #[test]
    fn native_gemini_thinking_config_is_zero_copy_passthrough_even_if_budget_maps_off_table() {
        // gemini-3.1-flash-image-preview 的表里只有 minimal/high；原生 thinkingBudget=8192 折算为 medium，
        // 同方言透传不得被表拒，且保持零拷贝 Unchanged。
        let raw = br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"thinkingConfig":{"thinkingBudget":8192}}}"#;
        let source: Value = serde_json::from_slice(raw).unwrap();
        let directive = crate::reasoning::parse_request_value(&source).merge(None);
        assert!(directive.is_some());
        let plan = plan_effort(
            &profile("gemini", "gemini-3.1-flash-image-preview", None),
            "gemini",
            &source,
            &source,
            directive,
            false,
        )
        .unwrap();
        assert!(matches!(plan, EffortPlan::Unchanged));
    }
}
