//! 深度行为指纹归因（基于 MIT 协议 `lmfpd` / ModelTrace `shared-detector-v2`）。
//!
//! 复用 `lmfpd` 的 3 轮随机整数挑战、Hellinger + 有序块 + 位置头特征与序列防伪检查
//! （`Decision::NotModel`），参考库默认自动缓存到磁盘并内置官方站点与国内镜像回退，
//! 也支持通过 `OKAPI_MODEL_VERIFY_DIR` 指向离线目录。
#![allow(clippy::cast_precision_loss)]

pub use lmfpd::{Challenge, Decision, Output, ProbabilityStatus, parse_numbers};
use lmfpd::{Analysis, Detector, LoadOptions};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

static DETECTOR_CACHE: OnceLock<RwLock<Option<Arc<Detector>>>> = OnceLock::new();

fn detector_slot() -> &'static RwLock<Option<Arc<Detector>>> {
    DETECTOR_CACHE.get_or_init(|| RwLock::new(None))
}

/// 构造 `lmfpd::LoadOptions`：优先使用 `OKAPI_MODEL_VERIFY_DIR`（离线本地目录），
/// 否则使用默认双源（官方 + 国内 jsDelivr 镜像）并缓存到本地磁盘。
pub fn load_options(force_refresh: bool) -> LoadOptions {
    let mut opts = LoadOptions::default();
    if let Ok(dir) = std::env::var("OKAPI_MODEL_VERIFY_DIR")
        && !dir.trim().is_empty()
    {
        opts.sources = vec![dir];
    }
    if let Ok(cache) = std::env::var("OKAPI_MODEL_VERIFY_CACHE_PATH")
        && !cache.trim().is_empty()
    {
        opts.cache = PathBuf::from(cache);
    }
    if force_refresh {
        opts.ttl = 0.0;
    }
    opts
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankStatus {
    pub loaded: bool,
    pub cache_exists: bool,
    pub built_at: Option<String>,
    pub model_count: Option<usize>,
    pub tokenizer_classes: usize,
    pub tokenizer_probes: usize,
    pub sources: Vec<String>,
}

/// 查询当前参考库缓存状态（不触发网络下载）。
pub async fn bank_status() -> BankStatus {
    let opts = load_options(false);
    let cache_exists = opts.cache.exists()
        || opts
            .sources
            .first()
            .is_some_and(|s| !s.starts_with("http") && PathBuf::from(s).join("shared_detector.bin").exists());
    let (tokenizer_classes, tokenizer_probes) =
        super::tokenizer::bank_info().map_or((0, 0), |(c, p, _)| (c, p));
    let guard = detector_slot().read().await;
    let built_at = guard.as_ref().map(|d| d.built_at().to_owned());
    BankStatus {
        loaded: guard.is_some(),
        cache_exists,
        built_at,
        model_count: None,
        tokenizer_classes,
        tokenizer_probes,
        sources: opts.sources,
    }
}

/// 加载（或按需强制刷新）`lmfpd` 参考库与打分器。
pub async fn load_detector(force_refresh: bool) -> Result<Arc<Detector>, String> {
    if !force_refresh
        && let Some(existing) = detector_slot().read().await.as_ref().cloned()
    {
        return Ok(existing);
    }
    let opts = load_options(force_refresh);
    let loaded = tokio::task::spawn_blocking(move || {
        if force_refresh
            && opts.sources.first().is_some_and(|s| s.starts_with("http"))
            && opts.cache.exists()
        {
            let _ = std::fs::remove_file(&opts.cache);
        }
        Detector::load(&opts).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("detector load task failed: {e}"))??;
    let arc = Arc::new(loaded);
    *detector_slot().write().await = Some(Arc::clone(&arc));
    Ok(arc)
}

/// 解析并截断到 `expected_count` 个 1..=355 的整数，用于实时汇报每个挑战的采样状态。
#[must_use]
pub fn evaluate_challenge_text(text: &str, expected_count: u64) -> (usize, u64, bool) {
    let nums = parse_numbers(text);
    let min_numbers = ((expected_count * 7) / 10).max(1);
    let accepted = u64::try_from(nums.len()).unwrap_or(u64::MAX) >= min_numbers;
    (nums.len(), min_numbers, accepted)
}

/// 生成 3 轮 ModelTrace / `lmfpd` 随机整数挑战。
pub fn generate_challenges() -> Result<Vec<Challenge>, String> {
    lmfpd::generate_challenges(3).map_err(|e| e.to_string())
}

/// 从任意上游/规范模型名推断其规范家族 ID（与 `lmfpd` 参考库 family 对齐）。
#[must_use]
pub fn infer_model_family(model: &str) -> Option<&'static str> {
    let lower = model.trim().to_ascii_lowercase();
    let tail = lower.rsplit('/').next().unwrap_or(&lower);
    let base = tail.split(':').next().unwrap_or(tail);
    // 兼容 Bedrock 区域前缀（如 us.anthropic.claude-sonnet-4-6）
    let clean = base
        .strip_prefix("us.")
        .or_else(|| base.strip_prefix("eu."))
        .or_else(|| base.strip_prefix("ap."))
        .or_else(|| base.strip_prefix("global."))
        .unwrap_or(base);
    let clean = clean.strip_prefix("anthropic.").unwrap_or(clean);

    if clean.starts_with("claude") || clean.contains("claude-") {
        Some("claude")
    } else if clean.starts_with("gpt-")
        || clean.starts_with("chatgpt-")
        || clean.starts_with("o1")
        || clean.starts_with("o3")
        || clean.starts_with("o4")
        || clean.starts_with("codex")
    {
        Some("gpt")
    } else if clean.starts_with("gemini") {
        Some("gemini")
    } else if clean.starts_with("deepseek") {
        Some("deepseek")
    } else if clean.starts_with("qwen") || clean.starts_with("qwq") {
        Some("qwen")
    } else if clean.starts_with("glm-") || clean.starts_with("chatglm") {
        Some("glm")
    } else if clean.starts_with("kimi") || clean.starts_with("moonshot") {
        Some("kimi")
    } else if clean.starts_with("grok") {
        Some("grok")
    } else if clean.starts_with("minimax") {
        Some("minimax")
    } else if clean.starts_with("seed-") || clean.starts_with("doubao") {
        Some("seed")
    } else if clean.starts_with("mimo") {
        Some("mimo")
    } else if clean.starts_with("step-") {
        Some("step")
    } else if clean.starts_with("hunyuan") || clean.starts_with("hy4") {
        Some("hy")
    } else {
        None
    }
}

/// 标准化模型名以便与 `lmfpd` 参考库 ID 近似比对（如 `claude-sonnet-4-6` ↔ `claude-sonnet-4.6`）。
fn normalize_for_bank_match(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    let tail = lower.rsplit('/').next().unwrap_or(&lower);
    let base = tail.split(':').next().unwrap_or(tail);
    base.replace(['_', '.'], "-")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeSampleSummary {
    pub index: usize,
    pub expected_count: u64,
    pub parsed_numbers: usize,
    pub minimum_numbers: u64,
    pub accepted: bool,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintCandidate {
    pub model: String,
    pub display_name: String,
    pub family: String,
    pub family_name: String,
    pub score: f64,
    pub probability: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyProbability {
    pub family: String,
    pub family_name: String,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintReport {
    pub bank_built_at: String,
    pub bank_model_count: usize,
    pub decision: Decision,
    pub prediction: String,
    pub prediction_name: String,
    pub family_prediction: String,
    pub family_prediction_name: String,
    pub declared_family: Option<String>,
    pub family_matches: Option<bool>,
    pub declared_in_bank: bool,
    pub top_probability: Option<f64>,
    pub top_family_probability: Option<f64>,
    pub probability_status: ProbabilityStatus,
    pub used_outputs: usize,
    pub model_like: Option<bool>,
    pub evidence_label: String,
    pub evidence_reason: String,
    pub top_matches: Vec<FingerprintCandidate>,
    pub family_probabilities: Vec<FamilyProbability>,
    pub samples: Vec<ChallengeSampleSummary>,
}

/// 对已采集的 3 轮挑战回复执行 `lmfpd` 归因打分并生成结构化报告。
pub fn score_outputs(
    detector: &Detector,
    declared_model: &str,
    outputs: &[Output],
    samples: Vec<ChallengeSampleSummary>,
) -> Result<FingerprintReport, String> {
    let analysis: Analysis = detector
        .analyze(outputs, false)
        .map_err(|e| e.to_string())?;
    let declared_family = infer_model_family(declared_model).map(str::to_owned);
    let norm_declared = normalize_for_bank_match(declared_model);

    let declared_in_bank = analysis.results.iter().any(|r| {
        let norm_id = normalize_for_bank_match(&r.model);
        norm_declared == norm_id || norm_declared.starts_with(&format!("{norm_id}-"))
    });

    // 汇总每个家族的闭集概率（与 ModelTrace 家族概率定义一致）
    let mut fam_map: BTreeMap<String, (String, f64)> = BTreeMap::new();
    for r in &analysis.results {
        if let Some(p) = r.probability {
            let entry = fam_map
                .entry(r.family.clone())
                .or_insert_with(|| (r.family_name.clone(), 0.0));
            entry.1 += p;
        }
    }
    let mut family_probabilities: Vec<FamilyProbability> = fam_map
        .into_iter()
        .map(|(family, (family_name, probability))| FamilyProbability {
            family,
            family_name,
            probability,
        })
        .collect();
    family_probabilities.sort_by(|a, b| b.probability.total_cmp(&a.probability));

    let top_family_probability = family_probabilities
        .iter()
        .find(|f| f.family == analysis.family_prediction)
        .map(|f| f.probability);

    let family_matches = match (&declared_family, analysis.decision) {
        (Some(decl), Decision::Scored | Decision::Partial) => {
            Some(decl == &analysis.family_prediction)
        }
        _ => None,
    };

    let top_matches: Vec<FingerprintCandidate> = analysis
        .results
        .iter()
        .take(5)
        .map(|r| FingerprintCandidate {
            model: r.model.clone(),
            display_name: r.display_name.clone(),
            family: r.family.clone(),
            family_name: r.family_name.clone(),
            score: r.score,
            probability: r.probability,
        })
        .collect();

    Ok(FingerprintReport {
        bank_built_at: detector.built_at().to_owned(),
        bank_model_count: analysis.results.len(),
        decision: analysis.decision,
        prediction: analysis.prediction,
        prediction_name: analysis.prediction_name,
        family_prediction: analysis.family_prediction,
        family_prediction_name: analysis.family_prediction_name,
        declared_family,
        family_matches,
        declared_in_bank,
        top_probability: analysis.probability,
        top_family_probability,
        probability_status: analysis.probability_status,
        used_outputs: analysis.used_outputs,
        model_like: analysis.sequence.map(|s| s.model_like),
        evidence_label: analysis.evidence.label,
        evidence_reason: analysis.evidence.reason,
        top_matches,
        family_probabilities: family_probabilities.into_iter().take(6).collect(),
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_canonical_families_across_provider_prefixes() {
        assert_eq!(infer_model_family("claude-opus-4-6"), Some("claude"));
        assert_eq!(
            infer_model_family("us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
            Some("claude")
        );
        assert_eq!(infer_model_family("openai/gpt-5.4"), Some("gpt"));
        assert_eq!(infer_model_family("o3-mini"), Some("gpt"));
        assert_eq!(
            infer_model_family("google/gemini-3.5-flash"),
            Some("gemini")
        );
        assert_eq!(infer_model_family("deepseek-v4-pro"), Some("deepseek"));
        assert_eq!(infer_model_family("qwen3.8-max-0902"), Some("qwen"));
        assert_eq!(infer_model_family("glm-5.3"), Some("glm"));
        assert_eq!(infer_model_family("kimi-k3"), Some("kimi"));
        assert_eq!(infer_model_family("grok-4.6"), Some("grok"));
        assert_eq!(infer_model_family("custom-internal-model"), None);
    }

    #[test]
    fn generates_three_lmfpd_challenges() {
        let challenges = generate_challenges().expect("challenges");
        assert_eq!(challenges.len(), 3);
        assert!(challenges.iter().all(|c| c.expected_count >= 200 && !c.prompt.is_empty()));
    }
}
