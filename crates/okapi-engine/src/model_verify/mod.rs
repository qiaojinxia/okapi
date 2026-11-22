//! 模型验真引擎：整合 L1 协议/能力探针、贝叶斯开集 Tokenizer 指纹与 L2 行为指纹归因。
//!
//! 第三方开源致谢（MIT License）：
//! - Tokenizer 开集贝叶斯算法与 `tokenizer_bank.json` 源自 `Ikaleio/lm-detector`
//! - 行为指纹参考库与打分器复用 `lmfpd` crate（ModelTrace `shared-detector-v2` 生态）

pub mod fingerprint;
pub mod tokenizer;
pub mod verdict;

pub use fingerprint::{
    BankStatus, Challenge, ChallengeSampleSummary, Decision, FamilyProbability,
    FingerprintCandidate, FingerprintReport, Output, ProbabilityStatus, bank_status,
    evaluate_challenge_text, generate_challenges, infer_model_family, load_detector, parse_numbers,
    score_outputs,
};
pub use tokenizer::{
    ClaimCheck, ClaimStatus, ClassScore, DEFAULT_MAX_PROBES, LabScore, TokenizerObservation,
    TokenizerPosterior, TokenizerVerdict, TokenizerVerdictKind, bank_info as tokenizer_bank_info,
    compute_posterior, compute_verdict, next_probes, probing_done, wrap_probe_text,
};
pub use verdict::{
    CheckStatus, ModelNameCheck, OverallVerdict, ProtocolCheckResult, SignatureCheck,
    TokenizerReport, ToolCallCheck, VerdictInput, VerifyWarning, synthesize_verdict,
};

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct VerifyCheckSelection {
    #[serde(default = "default_true")]
    pub protocol: bool,
    #[serde(default = "default_true")]
    pub tokenizer: bool,
    #[serde(default = "default_true")]
    pub fingerprint: bool,
}

impl Default for VerifyCheckSelection {
    fn default() -> Self {
        Self {
            protocol: true,
            tokenizer: true,
            fingerprint: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub channel_id: i64,
    pub provider: String,
    pub model: String,
    pub upstream_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<i64>,
    pub mimic_profile_active: bool,
    pub verdict: OverallVerdict,
    pub warnings: Vec<VerifyWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ProtocolCheckResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<TokenizerReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<FingerprintReport>,
    pub elapsed_ms: u64,
}

/// 推送给控制台前端的 SSE 事件负载。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VerifyEvent {
    Init {
        channel_id: i64,
        provider: String,
        model: String,
        upstream_model: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        key_id: Option<i64>,
        mimic_profile_active: bool,
        checks: VerifyCheckSelection,
        bank: BankStatus,
    },
    Stage {
        stage: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<usize>,
    },
    ProtocolResult {
        result: ProtocolCheckResult,
    },
    TokenizerProgress {
        probes_run: usize,
        max_probes: usize,
        posterior: TokenizerPosterior,
    },
    TokenizerResult {
        result: TokenizerReport,
    },
    FingerprintSample {
        sample: ChallengeSampleSummary,
    },
    FingerprintResult {
        result: FingerprintReport,
    },
    Done {
        report: Box<VerifyReport>,
    },
    Error {
        stage: String,
        message: String,
    },
}
