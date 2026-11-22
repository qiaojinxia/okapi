//! 模型验真综合判定与 L1 协议/能力探针数据结构。

use super::fingerprint::{FingerprintReport, infer_model_family};
use super::tokenizer::{ClaimStatus, TokenizerPosterior, TokenizerVerdict};
use lmfpd::Decision;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelNameCheck {
    pub declared_model: String,
    pub upstream_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_family: Option<String>,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ModelNameCheck {
    #[must_use]
    pub fn evaluate(
        declared_model: &str,
        upstream_model: &str,
        response_model: Option<String>,
    ) -> Self {
        let declared_family = infer_model_family(upstream_model)
            .or_else(|| infer_model_family(declared_model))
            .map(str::to_owned);
        let response_family = response_model
            .as_deref()
            .and_then(infer_model_family)
            .map(str::to_owned);

        let (status, detail) = match (&response_model, &declared_family, &response_family) {
            (None, _, _) => (
                CheckStatus::Warn,
                Some("upstream response omitted model field".to_owned()),
            ),
            (Some(_), Some(decl), Some(resp)) if decl != resp => (
                CheckStatus::Fail,
                Some(format!("declared family `{decl}` != response family `{resp}`")),
            ),
            (Some(_), Some(_), None) => (
                CheckStatus::Warn,
                Some("response model family could not be recognized".to_owned()),
            ),
            (Some(_), _, _) => (CheckStatus::Pass, None),
        };

        Self {
            declared_model: declared_model.to_owned(),
            upstream_model: upstream_model.to_owned(),
            response_model,
            declared_family,
            response_family,
            status,
            detail,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallCheck {
    pub status: CheckStatus,
    pub nonce: String,
    pub tool_called: bool,
    pub nonce_matched: bool,
    pub arguments_valid_json: bool,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureCheck {
    pub status: CheckStatus,
    pub applicable: bool,
    pub has_thinking_block: bool,
    pub has_signature: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_mode: Option<String>,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SignatureCheck {
    #[must_use]
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Skipped,
            applicable: false,
            has_thinking_block: false,
            has_signature: false,
            signature_len: None,
            thinking_mode: None,
            latency_ms: 0,
            detail: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolCheckResult {
    pub model_name: ModelNameCheck,
    pub tool_call: ToolCallCheck,
    pub signature: SignatureCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizerReport {
    pub posterior: TokenizerPosterior,
    pub verdict: TokenizerVerdict,
    pub probes_run: usize,
    pub baseline_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_drift: Option<i64>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallVerdict {
    Verified,
    Warning,
    Mismatch,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyWarning {
    pub code: String,
    pub severity: String,
    pub message: String,
}

impl VerifyWarning {
    fn new(code: &str, severity: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            severity: severity.to_owned(),
            message: message.into(),
        }
    }
}

pub struct VerdictInput<'a> {
    pub protocol: Option<&'a ProtocolCheckResult>,
    pub tokenizer: Option<&'a TokenizerReport>,
    pub fingerprint: Option<&'a FingerprintReport>,
    pub mimic_profile_active: bool,
    pub errors: &'a [String],
}

/// 汇总 L1 协议探针、贝叶斯 Tokenizer 指纹与 L2 行为指纹的证据，给出保守、可解释的综合结论。
#[must_use]
pub fn synthesize_verdict(input: &VerdictInput<'_>) -> (OverallVerdict, Vec<VerifyWarning>) {
    let mut warnings = Vec::new();
    let mut has_mismatch = false;
    let mut has_warning = false;
    let mut completed = 0usize;

    if input.mimic_profile_active {
        warnings.push(VerifyWarning::new(
            "mimic_profile_active",
            "info",
            "Channel has Claude Code mimic profile active; injected system prompt can bias behavioral integer distributions.",
        ));
    }

    if let Some(proto) = input.protocol {
        completed += 1;
        evaluate_protocol(proto, &mut warnings, &mut has_mismatch, &mut has_warning);
    }

    if let Some(tok) = input.tokenizer {
        completed += 1;
        evaluate_tokenizer(tok, &mut warnings, &mut has_mismatch, &mut has_warning);
    }

    if let Some(fp) = input.fingerprint {
        completed += 1;
        evaluate_fingerprint(
            fp,
            input.mimic_profile_active,
            &mut warnings,
            &mut has_mismatch,
            &mut has_warning,
        );
    }

    for err in input.errors {
        warnings.push(VerifyWarning::new("check_error", "error", err.clone()));
    }

    let overall = if completed == 0 && !input.errors.is_empty() {
        OverallVerdict::Error
    } else if has_mismatch {
        OverallVerdict::Mismatch
    } else if has_warning || !input.errors.is_empty() {
        OverallVerdict::Warning
    } else {
        OverallVerdict::Verified
    };

    (overall, warnings)
}

fn evaluate_protocol(
    proto: &ProtocolCheckResult,
    warnings: &mut Vec<VerifyWarning>,
    has_mismatch: &mut bool,
    has_warning: &mut bool,
) {
    match proto.model_name.status {
        CheckStatus::Fail => {
            *has_mismatch = true;
            warnings.push(VerifyWarning::new(
                "response_model_family_mismatch",
                "error",
                proto
                    .model_name
                    .detail
                    .clone()
                    .unwrap_or_else(|| "Response model family does not match declared model.".into()),
            ));
        }
        CheckStatus::Warn => {
            *has_warning = true;
            if let Some(detail) = &proto.model_name.detail {
                warnings.push(VerifyWarning::new(
                    "response_model_warn",
                    "warn",
                    detail.clone(),
                ));
            }
        }
        CheckStatus::Pass | CheckStatus::Skipped | CheckStatus::Error => {}
    }

    if matches!(proto.tool_call.status, CheckStatus::Fail | CheckStatus::Error) {
        *has_warning = true;
        warnings.push(VerifyWarning::new(
            "tool_call_failed",
            "warn",
            proto
                .tool_call
                .detail
                .clone()
                .unwrap_or_else(|| "Tool calling capability verification failed.".into()),
        ));
    }

    if proto.signature.applicable {
        match proto.signature.status {
            CheckStatus::Fail | CheckStatus::Warn => {
                *has_warning = true;
                warnings.push(VerifyWarning::new(
                    "signature_missing",
                    "warn",
                    proto.signature.detail.clone().unwrap_or_else(|| {
                        "Anthropic thinking.signature was absent or unverified.".into()
                    }),
                ));
            }
            CheckStatus::Error => {
                *has_warning = true;
                if let Some(detail) = &proto.signature.detail {
                    warnings.push(VerifyWarning::new(
                        "signature_probe_error",
                        "warn",
                        detail.clone(),
                    ));
                }
            }
            CheckStatus::Pass | CheckStatus::Skipped => {}
        }
    }
}

fn evaluate_tokenizer(
    tok: &TokenizerReport,
    warnings: &mut Vec<VerifyWarning>,
    has_mismatch: &mut bool,
    has_warning: &mut bool,
) {
    let drifted = tok.baseline_drift.is_some_and(|d| d > 2);
    if drifted {
        *has_warning = true;
        warnings.push(VerifyWarning::new(
            "tokenizer_baseline_drift",
            "warn",
            format!(
                "Prompt token baseline drifted by {} tokens between start and end; upstream may inject dynamic context.",
                tok.baseline_drift.unwrap_or(0)
            ),
        ));
    }

    if let Some(claim) = &tok.verdict.claim {
        match claim.status {
            ClaimStatus::Inconsistent if !drifted => {
                *has_mismatch = true;
                warnings.push(VerifyWarning::new(
                    "tokenizer_claim_inconsistent",
                    "error",
                    format!(
                        "Observed tokenizer `{}` contradicts declared vendor `{}` (claim probability {:.1}%).",
                        tok.verdict.top.series,
                        claim.vendor,
                        claim.probability * 100.0
                    ),
                ));
            }
            ClaimStatus::Inconsistent => {
                *has_warning = true;
                warnings.push(VerifyWarning::new(
                    "tokenizer_claim_inconsistent_drifted",
                    "warn",
                    "Tokenizer appears inconsistent with declared model, but baseline drifted.",
                ));
            }
            ClaimStatus::Uncertain => {
                *has_warning = true;
                warnings.push(VerifyWarning::new(
                    "tokenizer_claim_uncertain",
                    "warn",
                    format!(
                        "Tokenizer claim consistency is uncertain (probability {:.1}%).",
                        claim.probability * 100.0
                    ),
                ));
            }
            ClaimStatus::Consistent => {}
        }
    }
}

fn evaluate_fingerprint(
    fp: &FingerprintReport,
    mimic_profile_active: bool,
    warnings: &mut Vec<VerifyWarning>,
    has_mismatch: &mut bool,
    has_warning: &mut bool,
) {
    if !fp.declared_in_bank {
        warnings.push(VerifyWarning::new(
            "declared_model_not_in_bank",
            "info",
            "Declared model is not in the closed-set behavioral reference bank; attribution shows closest known family.",
        ));
    }

    match fp.decision {
        Decision::NotModel => {
            *has_mismatch = true;
            warnings.push(VerifyWarning::new(
                "not_model_output",
                "error",
                "Sequence shape indicates scripted/PRNG output rather than a real LLM.",
            ));
        }
        Decision::Unscorable => {
            *has_warning = true;
            warnings.push(VerifyWarning::new(
                "fingerprint_unscorable",
                "warn",
                "Too few valid integer sequences were collected for behavioral attribution.",
            ));
        }
        Decision::Partial => {
            *has_warning = true;
            warnings.push(VerifyWarning::new(
                "fingerprint_partial",
                "warn",
                format!(
                    "Only {} of 3 challenges yielded full integer lists; attribution confidence is reduced.",
                    fp.used_outputs
                ),
            ));
            if fp.family_matches == Some(false) {
                warnings.push(VerifyWarning::new(
                    "fingerprint_family_mismatch_soft",
                    "warn",
                    format!(
                        "Partial behavioral fingerprint leans toward {} instead of declared family.",
                        fp.family_prediction_name
                    ),
                ));
            }
        }
        Decision::Scored => {
            if fp.family_matches == Some(false) {
                let high_conf = fp.top_family_probability.is_some_and(|p| p >= 0.85);
                if !mimic_profile_active && fp.declared_in_bank && high_conf {
                    *has_mismatch = true;
                    warnings.push(VerifyWarning::new(
                        "fingerprint_family_mismatch",
                        "error",
                        format!(
                            "Behavioral fingerprint attributed output to {} ({:.1}% family probability), mismatching declared family.",
                            fp.family_prediction_name,
                            fp.top_family_probability.unwrap_or(0.0) * 100.0
                        ),
                    ));
                } else {
                    *has_warning = true;
                    warnings.push(VerifyWarning::new(
                        "fingerprint_family_mismatch_soft",
                        "warn",
                        format!(
                            "Behavioral fingerprint favors {} (declared in bank: {}, mimic active: {}).",
                            fp.family_prediction_name, fp.declared_in_bank, mimic_profile_active
                        ),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_verify::fingerprint::{FamilyProbability, FingerprintCandidate};
    use crate::model_verify::tokenizer::{ClassScore, ClaimCheck, TokenizerVerdictKind};
    use lmfpd::ProbabilityStatus;

    fn sample_protocol_pass() -> ProtocolCheckResult {
        ProtocolCheckResult {
            model_name: ModelNameCheck::evaluate(
                "claude-sonnet-4-6",
                "claude-sonnet-4-6",
                Some("claude-sonnet-4-6".into()),
            ),
            tool_call: ToolCallCheck {
                status: CheckStatus::Pass,
                nonce: "okapi-1234".into(),
                tool_called: true,
                nonce_matched: true,
                arguments_valid_json: true,
                latency_ms: 420,
                detail: None,
            },
            signature: SignatureCheck {
                status: CheckStatus::Pass,
                applicable: true,
                has_thinking_block: true,
                has_signature: true,
                signature_len: Some(256),
                thinking_mode: Some("adaptive".into()),
                latency_ms: 800,
                detail: None,
            },
        }
    }

    fn sample_fingerprint(family: &str, declared_in_bank: bool, prob: f64) -> FingerprintReport {
        FingerprintReport {
            bank_built_at: "2026-11-01T00:00:00Z".into(),
            bank_model_count: 75,
            decision: Decision::Scored,
            prediction: format!("{family}-top"),
            prediction_name: format!("{family} Top"),
            family_prediction: family.into(),
            family_prediction_name: family.into(),
            declared_family: Some("claude".into()),
            family_matches: Some(family == "claude"),
            declared_in_bank,
            top_probability: Some(prob),
            top_family_probability: Some(prob),
            probability_status: ProbabilityStatus::ReferenceCalibrated,
            used_outputs: 3,
            model_like: Some(true),
            evidence_label: "Strong".into(),
            evidence_reason: "Top family dominates".into(),
            top_matches: vec![FingerprintCandidate {
                model: format!("{family}-top"),
                display_name: format!("{family} Top"),
                family: family.into(),
                family_name: family.into(),
                score: 0.9,
                probability: Some(prob),
            }],
            family_probabilities: vec![FamilyProbability {
                family: family.into(),
                family_name: family.into(),
                probability: prob,
            }],
            samples: vec![],
        }
    }

    #[test]
    fn clean_pass_synthesizes_verified() {
        let proto = sample_protocol_pass();
        let fp = sample_fingerprint("claude", true, 0.96);
        let (verdict, warnings) = synthesize_verdict(&VerdictInput {
            protocol: Some(&proto),
            tokenizer: None,
            fingerprint: Some(&fp),
            mimic_profile_active: false,
            errors: &[],
        });
        assert_eq!(verdict, OverallVerdict::Verified);
        assert!(warnings.is_empty());
    }

    #[test]
    fn high_confidence_fingerprint_mismatch_flags_mismatch_unless_mimic_active() {
        let proto = sample_protocol_pass();
        let fp = sample_fingerprint("deepseek", true, 0.95);
        let (verdict, warnings) = synthesize_verdict(&VerdictInput {
            protocol: Some(&proto),
            tokenizer: None,
            fingerprint: Some(&fp),
            mimic_profile_active: false,
            errors: &[],
        });
        assert_eq!(verdict, OverallVerdict::Mismatch);
        assert!(warnings.iter().any(|w| w.code == "fingerprint_family_mismatch"));

        let (mimic_verdict, mimic_warnings) = synthesize_verdict(&VerdictInput {
            protocol: Some(&proto),
            tokenizer: None,
            fingerprint: Some(&fp),
            mimic_profile_active: true,
            errors: &[],
        });
        assert_eq!(mimic_verdict, OverallVerdict::Warning);
        assert!(mimic_warnings.iter().any(|w| w.code == "mimic_profile_active"));
    }

    #[test]
    fn tokenizer_inconsistent_without_drift_flags_mismatch() {
        let tok = TokenizerReport {
            posterior: TokenizerPosterior {
                classes: vec![],
                labs: vec![],
                unknown: 0.001,
                alien: 0.001,
                answered: 6,
            },
            verdict: TokenizerVerdict {
                kind: TokenizerVerdictKind::Exact,
                top: ClassScore {
                    id: "deepseek".into(),
                    lab: "deepseek".into(),
                    lab_name: "DeepSeek".into(),
                    series: "DeepSeek V3".into(),
                    series_zh: "DeepSeek V3".into(),
                    exact: 0.999,
                    related: 0.0,
                    matched: 6,
                    compared: 6,
                    overhead: 12,
                },
                confidence: 0.999,
                claim: Some(ClaimCheck {
                    expected: vec![],
                    vendor: "Anthropic".into(),
                    first_party: true,
                    probability: 0.001,
                    status: ClaimStatus::Inconsistent,
                }),
            },
            probes_run: 6,
            baseline_tokens: 12,
            baseline_drift: Some(0),
            latency_ms: 900,
        };
        let (verdict, warnings) = synthesize_verdict(&VerdictInput {
            protocol: None,
            tokenizer: Some(&tok),
            fingerprint: None,
            mimic_profile_active: false,
            errors: &[],
        });
        assert_eq!(verdict, OverallVerdict::Mismatch);
        assert!(warnings.iter().any(|w| w.code == "tokenizer_claim_inconsistent"));
    }

    #[test]
    fn model_name_check_catches_cross_family_substitution() {
        let check = ModelNameCheck::evaluate(
            "claude-opus-4-6",
            "claude-opus-4-6",
            Some("gpt-4o-mini-2024-07-18".into()),
        );
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(check.declared_family.as_deref(), Some("claude"));
        assert_eq!(check.response_family.as_deref(), Some("gpt"));
    }
}
