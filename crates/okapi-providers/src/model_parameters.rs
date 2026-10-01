//! Curated request contracts, not administrator capability declarations.
//! Checked 2026-10-01 against the providers' docs (see docs/model-parameters.md).
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
        let levels: &[&'static str] = match model {
            "claude-fable-5-1" | "claude-fable-5" | "claude-mythos-5-1" | "claude-mythos-5"
            | "claude-opus-5-5" | "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7"
            | "claude-sonnet-5-5" | "claude-sonnet-5" => &["low", "medium", "high", "xhigh", "max"],
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
    if profile.mode == Mode::Budget && dialect == "anthropic" {
        return directive.and_then(|d| preserve_budget_cap(built, source, d));
    }
    if matches!(profile.mode, Mode::Passthrough | Mode::Budget) {
        return None;
    }
    let result = (|| {
        let original: Value =
            serde_json::from_slice(source).map_err(|e| UpstreamError::Build(e.to_string()))?;
        let level = original
            .pointer("/reasoning/effort")
            .or_else(|| original.get("reasoning_effort"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| directive.map(|d| d.effective_effort().as_str().to_owned()));
        let Some(level) = level else {
            return Ok(built.clone());
        };
        if !profile.efforts.contains(&level.as_str()) {
            return Err(UpstreamError::Status { status: 400, body: Bytes::from(serde_json::to_vec(&json!({"error": {"type": "invalid_request_error", "code": "invalid_request_error", "param": "reasoning_effort", "message": format!("Unsupported reasoning effort: {level}")}})).unwrap_or_default()), retry_after_secs: None });
        }
        let mut v: Value =
            serde_json::from_slice(built).map_err(|e| UpstreamError::Build(e.to_string()))?;
        let obj = v
            .as_object_mut()
            .ok_or_else(|| UpstreamError::Build("invalid_body".into()))?;
        obj.remove("reasoning_effort");
        if !native_responses {
            obj.remove("reasoning");
        }
        match dialect {
            "anthropic" => {
                obj.entry("output_config")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or_else(|| UpstreamError::Build("output_config".into()))?
                    .insert("effort".into(), json!(level));
                // Opus 4.5 supports effort independently of thinking; newer
                // models support adaptive. Don't overwrite explicit native thinking.
                if original.get("thinking").is_none()
                    && obj
                        .get("model")
                        .and_then(Value::as_str)
                        .is_none_or(|m| m != "claude-opus-4-5")
                {
                    obj.insert("thinking".into(), json!({"type":"adaptive"}));
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
        serde_json::to_vec(&v)
            .map(Bytes::from)
            .map_err(|e| UpstreamError::Build(e.to_string()))
    })();
    Some(result)
}

/// Legacy Anthropic injection reserves answer headroom by growing max_tokens.
/// A user-supplied output limit is a hard limit: don't enlarge it just because a
/// budget was requested. An omitted limit retains the legacy default behavior.
fn preserve_budget_cap(
    built: &Bytes,
    source: &Bytes,
    directive: ReasoningDirective,
) -> Option<Result<Bytes, UpstreamError>> {
    let original: Value = serde_json::from_slice(source).ok()?;
    let cap = original
        .get("max_completion_tokens")
        .or_else(|| original.get("max_tokens"))?
        .as_u64()?;
    Some((|| {
        if cap <= u64::from(directive.effective_budget().max(1024)) {
            return Err(UpstreamError::Build(
                "max_tokens_must_exceed_thinking_budget".into(),
            ));
        }
        let translated = crate::reasoning::apply_anthropic(built, directive)?;
        let mut v: Value =
            serde_json::from_slice(&translated).map_err(|e| UpstreamError::Build(e.to_string()))?;
        v["max_tokens"] = json!(cap);
        serde_json::to_vec(&v)
            .map(Bytes::from)
            .map_err(|e| UpstreamError::Build(e.to_string()))
    })())
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
    if let Some(cap) = v.as_object_mut().and_then(|o| o.remove("max_tokens")) {
        v.as_object_mut()
            .unwrap()
            .entry("max_completion_tokens")
            .or_insert(cap);
    }
    serde_json::to_vec(&v)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(p.efforts.is_empty());
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
}
