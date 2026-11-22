//! 贝叶斯开集 Tokenizer 指纹识别（移植自 MIT 协议项目 `Ikaleio/lm-detector`）。
//!
//! 原理：向目标渠道发送基线请求（仅包裹壳）与若干极短探测文本（`prefix + text + suffix`），
//! 从接口返回的 `prompt_tokens` / `input_tokens` 扣除固定系统开销后，与内置 62 类公开分词器
//! 的 72 条预计算探针计数做贝叶斯开集推断（区分已知类 `exact`、近亲变体 `related` 与未公开
//! 分词器 `alien`，如 Anthropic Claude / Google Gemini 等未开源分词器的官方模型）。
//!
//! 归属与致谢：算法与内置 `tokenizer_bank.json` 数据源自 `Ikaleio/lm-detector`（MIT License）。
#![allow(clippy::cast_precision_loss)]

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

const EMBEDDED_BANK_JSON: &str = include_str!("tokenizer_bank.json");

const PRIOR_KNOWN: f64 = 0.8;
const PRIOR_RELATIVE: f64 = 0.1;
const PRIOR_ALIEN: f64 = 0.1;
const KNOWN_RATES: &[(f64, f64)] = &[(0.02, 0.4), (0.06, 0.4), (0.15, 0.2)];
const RELATIVE_RATES: &[(f64, f64)] = &[(0.2, 0.3), (0.4, 0.4), (0.7, 0.3)];
const BASELINE_RATE: f64 = 0.02;
const NARROW_P: f64 = 0.02;
const WIDE_P: f64 = 0.85;
const FLOOR: f64 = 1e-6;
const OFFSET_WINDOW: i64 = 3;
const ALIEN_SPREAD: f64 = 0.15;
const PLAN_RATE_KNOWN: f64 = 0.06;
const PLAN_RATE_RELATIVE: f64 = 0.4;
const PLAN_WINDOW: i64 = 24;
const MINIMUM_PROBES: usize = 4;
pub const DEFAULT_MAX_PROBES: usize = 12;
const STOP_AT: f64 = 0.99;
const MINIMUM_GAIN: f64 = 1e-4;
const PLANNED_CLASSES: usize = 24;

#[derive(Debug, Clone, Deserialize)]
pub struct TokenizerProbe {
    pub id: String,
    pub category: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenizerClass {
    pub id: String,
    pub lab: String,
    pub lab_name: String,
    pub series: String,
    pub series_zh: String,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub counts: Vec<i64>,
    #[serde(default)]
    pub alternatives: Option<HashMap<String, Vec<i64>>>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenizerApiModel {
    pub pattern: String,
    pub vendor: String,
    pub class: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenizerWrapper {
    pub prefix: String,
    pub suffix: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenizerBank {
    pub schema: String,
    pub generated_at: String,
    pub wrapper: TokenizerWrapper,
    pub probes: Vec<TokenizerProbe>,
    pub classes: Vec<TokenizerClass>,
    pub api_models: Vec<TokenizerApiModel>,
}

struct PatternMatcher {
    positive: Regex,
    neg_lookahead: Option<Regex>,
}

impl PatternMatcher {
    fn compile(pattern: &str) -> Result<Self, String> {
        if let Some((before, rest)) = pattern.split_once("(?!")
            && let Some((neg, after)) = rest.split_once(')')
            && after.is_empty()
        {
            return Ok(Self {
                positive: compile_case_insensitive(before)?,
                neg_lookahead: Some(compile_case_insensitive(&format!("^(?:{neg})"))?),
            });
        }
        Ok(Self {
            positive: compile_case_insensitive(pattern)?,
            neg_lookahead: None,
        })
    }

    fn is_match(&self, text: &str) -> bool {
        let Some(m) = self.positive.find(text) else {
            return false;
        };
        match &self.neg_lookahead {
            None => true,
            Some(neg) => !neg.is_match(&text[m.end()..]),
        }
    }
}

struct CompiledBank {
    bank: TokenizerBank,
    probe_index: HashMap<String, usize>,
    /// `values[class_idx][probe_idx]` = 允许的 token 计数集合。
    values: Vec<Vec<Vec<i64>>>,
    alien_scale: Vec<f64>,
    api_patterns: Vec<PatternMatcher>,
    class_aliases: Vec<Vec<PatternMatcher>>,
}

static GLOBAL_BANK: OnceLock<Result<Arc<CompiledBank>, String>> = OnceLock::new();

fn compile_case_insensitive(pattern: &str) -> Result<Regex, String> {
    Regex::new(&format!("(?i){pattern}"))
        .map_err(|e| format!("invalid tokenizer bank regex `{pattern}`: {e}"))
}

fn compile_bank(bank: TokenizerBank) -> Result<CompiledBank, String> {
    if bank.schema != "tokenizer-bank-v1" {
        return Err(format!("unexpected tokenizer bank schema: {}", bank.schema));
    }
    if bank.probes.is_empty() || bank.classes.len() < 2 {
        return Err("tokenizer bank requires probes and >=2 classes".into());
    }
    let probe_index: HashMap<String, usize> = bank
        .probes
        .iter()
        .enumerate()
        .map(|(j, p)| (p.id.clone(), j))
        .collect();
    let mut values = Vec::with_capacity(bank.classes.len());
    let mut class_aliases = Vec::with_capacity(bank.classes.len());
    for item in &bank.classes {
        if item.counts.len() != bank.probes.len() {
            return Err(format!("class {} count length mismatch", item.id));
        }
        let mut class_vals = Vec::with_capacity(bank.probes.len());
        for (j, &cnt) in item.counts.iter().enumerate() {
            let key = j.to_string();
            let allowed = item
                .alternatives
                .as_ref()
                .and_then(|alt| alt.get(&key))
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| vec![cnt]);
            class_vals.push(allowed);
        }
        values.push(class_vals);
        let mut regexes = Vec::with_capacity(item.aliases.len());
        for alias in &item.aliases {
            regexes.push(PatternMatcher::compile(alias)?);
        }
        class_aliases.push(regexes);
    }
    let mut alien_scale = Vec::with_capacity(bank.probes.len());
    for j in 0..bank.probes.len() {
        let mut sorted: Vec<i64> = bank.classes.iter().map(|c| c.counts[j]).collect();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2] as f64;
        alien_scale.push((-1.0 / (ALIEN_SPREAD * median).max(1.5)).exp());
    }
    let mut api_patterns = Vec::with_capacity(bank.api_models.len());
    for entry in &bank.api_models {
        api_patterns.push(PatternMatcher::compile(&entry.pattern)?);
    }
    Ok(CompiledBank {
        bank,
        probe_index,
        values,
        alien_scale,
        api_patterns,
        class_aliases,
    })
}

fn load_compiled() -> Result<Arc<CompiledBank>, String> {
    GLOBAL_BANK
        .get_or_init(|| {
            let raw = std::env::var("OKAPI_MODEL_VERIFY_DIR")
                .ok()
                .map(|dir| std::path::PathBuf::from(dir).join("tokenizer_bank.json"))
                .and_then(|path| std::fs::read_to_string(path).ok())
                .unwrap_or_else(|| EMBEDDED_BANK_JSON.to_owned());
            let bank: TokenizerBank =
                serde_json::from_str(&raw).map_err(|e| format!("parse tokenizer_bank.json: {e}"))?;
            compile_bank(bank).map(Arc::new)
        })
        .clone()
}

/// 返回内置 Tokenizer 银行的元数据（类数、探针数、生成时间）。
pub fn bank_info() -> Result<(usize, usize, String), String> {
    let compiled = load_compiled()?;
    Ok((
        compiled.bank.classes.len(),
        compiled.bank.probes.len(),
        compiled.bank.generated_at.clone(),
    ))
}

/// 包装探针文本：`probe_id = None` 表示基线请求（空内容，只测固定开销）。
pub fn wrap_probe_text(probe_id: Option<&str>) -> Result<String, String> {
    let compiled = load_compiled()?;
    let inner = match probe_id {
        None => "",
        Some(id) => {
            let &j = compiled
                .probe_index
                .get(id)
                .ok_or_else(|| format!("unknown tokenizer probe `{id}`"))?;
            compiled.bank.probes[j].text.as_str()
        }
    };
    Ok(format!(
        "{}{inner}{}",
        compiled.bank.wrapper.prefix, compiled.bank.wrapper.suffix
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizerObservation {
    pub probe: Option<String>,
    pub tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassScore {
    pub id: String,
    pub lab: String,
    pub lab_name: String,
    pub series: String,
    pub series_zh: String,
    pub exact: f64,
    pub related: f64,
    pub matched: usize,
    pub compared: usize,
    pub overhead: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabScore {
    pub lab: String,
    pub lab_name: String,
    pub total: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizerPosterior {
    pub classes: Vec<ClassScore>,
    pub labs: Vec<LabScore>,
    pub unknown: f64,
    pub alien: f64,
    pub answered: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenizerVerdictKind {
    Exact,
    Related,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Consistent,
    Inconsistent,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimCheck {
    pub expected: Vec<String>,
    pub vendor: String,
    pub first_party: bool,
    pub probability: f64,
    pub status: ClaimStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizerVerdict {
    pub kind: TokenizerVerdictKind,
    pub top: ClassScore,
    pub confidence: f64,
    pub claim: Option<ClaimCheck>,
}

fn log_sum_exp(values: &[f64]) -> f64 {
    let top = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !top.is_finite() {
        return top;
    }
    let sum: f64 = values.iter().map(|&v| (v - top).exp()).sum();
    top + sum.ln()
}

fn log_geometric(distance: i64, p: f64) -> f64 {
    ((1.0 - p) / (1.0 + p)).ln() + (distance.unsigned_abs() as f64) * p.ln()
}

fn log_residual(distance: i64, rate: f64) -> f64 {
    log_sum_exp(&[
        (1.0 - rate).ln() + log_geometric(distance, NARROW_P),
        rate.ln() + log_geometric(distance, WIDE_P),
        FLOOR.ln(),
    ])
}

fn nearest(delta: i64, allowed: &[i64]) -> i64 {
    allowed
        .iter()
        .map(|&v| delta - v)
        .min_by_key(|d| d.unsigned_abs())
        .unwrap_or(delta)
}

struct Datum {
    probe_idx: Option<usize>,
    tokens: i64,
}

fn class_evidence(
    compiled: &CompiledBank,
    rows: &[Datum],
    class_idx: usize,
    grid: &[(f64, f64)],
    baseline: i64,
) -> (f64, i64) {
    let window_len = (2 * OFFSET_WINDOW + 1) as f64;
    let prior = -window_len.ln();
    let window_count = usize::try_from(2 * OFFSET_WINDOW + 1).unwrap_or(1);
    let mut terms = Vec::with_capacity(window_count * grid.len());
    let mut best_log = f64::NEG_INFINITY;
    let mut best_offset = baseline;
    for offset in (baseline - OFFSET_WINDOW)..=(baseline + OFFSET_WINDOW) {
        let residuals: Vec<i64> = rows
            .iter()
            .map(|row| match row.probe_idx {
                None => row.tokens - offset,
                Some(j) => nearest(row.tokens - offset, &compiled.values[class_idx][j]),
            })
            .collect();
        for &(rate, weight) in grid {
            let log: f64 = residuals
                .iter()
                .zip(rows)
                .map(|(&res, row)| {
                    log_residual(res, if row.probe_idx.is_none() { BASELINE_RATE } else { rate })
                })
                .sum();
            terms.push(prior + weight.ln() + log);
            if log > best_log {
                best_log = log;
                best_offset = offset;
            }
        }
    }
    (log_sum_exp(&terms), best_offset)
}

fn alien_evidence(compiled: &CompiledBank, rows: &[Datum], baseline: i64) -> f64 {
    let size = compiled.bank.classes.len() as f64;
    let window_len = (2 * OFFSET_WINDOW + 1) as f64;
    let prior = -window_len.ln();
    let terms: Vec<f64> = ((baseline - OFFSET_WINDOW)..=(baseline + OFFSET_WINDOW))
        .map(|offset| {
            let sum: f64 = rows
                .iter()
                .map(|row| match row.probe_idx {
                    None => log_residual(row.tokens - offset, BASELINE_RATE),
                    Some(j) => {
                        let scale = compiled.alien_scale[j];
                        let total: f64 = compiled
                            .values
                            .iter()
                            .map(|cls| {
                                log_geometric(nearest(row.tokens - offset, &cls[j]), scale).exp()
                            })
                            .sum();
                        ((total / size) + FLOOR).ln()
                    }
                })
                .sum();
            prior + sum
        })
        .collect();
    log_sum_exp(&terms)
}

/// 计算当前观测序列下的贝叶斯开集后验分布（至少需 1 条基线观测后才有结果）。
pub fn compute_posterior(
    observations: &[TokenizerObservation],
) -> Result<Option<TokenizerPosterior>, String> {
    let compiled = load_compiled()?;
    let mut rows = Vec::with_capacity(observations.len());
    for obs in observations {
        let probe_idx = match obs.probe.as_deref() {
            None => None,
            Some(id) => Some(
                *compiled
                    .probe_index
                    .get(id)
                    .ok_or_else(|| format!("unknown tokenizer probe `{id}`"))?,
            ),
        };
        rows.push(Datum {
            probe_idx,
            tokens: obs.tokens,
        });
    }
    let Some(first_baseline) = rows.iter().find(|r| r.probe_idx.is_none()) else {
        return Ok(None);
    };
    let baseline = first_baseline.tokens;
    let n_classes = compiled.bank.classes.len();
    let n_f = n_classes as f64;

    let mut known_logs = Vec::with_capacity(n_classes);
    let mut known_offsets = Vec::with_capacity(n_classes);
    let mut rel_logs = Vec::with_capacity(n_classes);
    for c in 0..n_classes {
        let (k_log, offset) = class_evidence(&compiled, &rows, c, KNOWN_RATES, baseline);
        let (r_log, _) = class_evidence(&compiled, &rows, c, RELATIVE_RATES, baseline);
        known_logs.push((PRIOR_KNOWN / n_f).ln() + k_log);
        known_offsets.push(offset);
        rel_logs.push((PRIOR_RELATIVE / n_f).ln() + r_log);
    }
    let alien_log = PRIOR_ALIEN.ln() + alien_evidence(&compiled, &rows, baseline);

    let mut all_logs = Vec::with_capacity(n_classes * 2 + 1);
    all_logs.extend_from_slice(&known_logs);
    all_logs.extend_from_slice(&rel_logs);
    all_logs.push(alien_log);
    let evidence = log_sum_exp(&all_logs);

    let probe_rows: Vec<&Datum> = rows.iter().filter(|r| r.probe_idx.is_some()).collect();
    let answered = probe_rows.len();

    let mut classes = Vec::with_capacity(n_classes);
    let mut lab_map: HashMap<String, LabScore> = HashMap::new();
    for (c, item) in compiled.bank.classes.iter().enumerate() {
        let exact = (known_logs[c] - evidence).exp();
        let related = (rel_logs[c] - evidence).exp();
        let offset = known_offsets[c];
        let matched = probe_rows
            .iter()
            .filter(|r| {
                r.probe_idx
                    .is_some_and(|j| nearest(r.tokens - offset, &compiled.values[c][j]) == 0)
            })
            .count();
        classes.push(ClassScore {
            id: item.id.clone(),
            lab: item.lab.clone(),
            lab_name: item.lab_name.clone(),
            series: item.series.clone(),
            series_zh: item.series_zh.clone(),
            exact,
            related,
            matched,
            compared: answered,
            overhead: offset,
        });
        let entry = lab_map.entry(item.lab.clone()).or_insert_with(|| LabScore {
            lab: item.lab.clone(),
            lab_name: item.lab_name.clone(),
            total: 0.0,
        });
        entry.total += exact + related;
    }
    classes.sort_by(|a, b| (b.exact + b.related).total_cmp(&(a.exact + a.related)));
    let mut labs: Vec<LabScore> = lab_map.into_values().collect();
    labs.sort_by(|a, b| b.total.total_cmp(&a.total));

    let alien = (alien_log - evidence).exp();
    let unknown = alien + classes.iter().map(|c| c.related).sum::<f64>();

    Ok(Some(TokenizerPosterior {
        classes,
        labs,
        unknown,
        alien,
        answered,
    }))
}

#[derive(Clone, Copy)]
enum AtomKind {
    Known,
    Relative,
    Alien,
}

struct Atom {
    group: String,
    weight: f64,
    kind: AtomKind,
    class_idx: usize,
}

fn atom_roots(compiled: &CompiledBank, atom: &Atom, j: usize, low: i64, size: usize) -> Vec<f64> {
    let mut raw = vec![0.0f64; size];
    let mut norm = 0.0f64;
    for (i, slot) in raw.iter_mut().enumerate() {
        let y = low + i64::try_from(i).unwrap_or(0);
        let density = match atom.kind {
            AtomKind::Known => {
                log_residual(y - compiled.values[atom.class_idx][j][0], PLAN_RATE_KNOWN).exp()
            }
            AtomKind::Relative => {
                log_residual(y - compiled.values[atom.class_idx][j][0], PLAN_RATE_RELATIVE).exp()
            }
            AtomKind::Alien => {
                let scale = compiled.alien_scale[j];
                compiled
                    .values
                    .iter()
                    .map(|cls| log_geometric(y - cls[j][0], scale).exp())
                    .sum()
            }
        };
        *slot = density;
        norm += density;
    }
    let norm = norm.max(1e-30);
    for slot in &mut raw {
        *slot = (*slot / norm).sqrt();
    }
    raw
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn build_plan_atoms(compiled: &CompiledBank, posterior: Option<&TokenizerPosterior>) -> Vec<Atom> {
    let class_pos: HashMap<&str, usize> = compiled
        .bank
        .classes
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let mut atoms = Vec::new();
    if let Some(post) = posterior {
        for score in post.classes.iter().take(PLANNED_CLASSES) {
            if let Some(&c) = class_pos.get(score.id.as_str()) {
                atoms.push(Atom {
                    group: score.id.clone(),
                    weight: score.exact,
                    kind: AtomKind::Known,
                    class_idx: c,
                });
                if score.related > 0.0 {
                    atoms.push(Atom {
                        group: "unknown".into(),
                        weight: score.related,
                        kind: AtomKind::Relative,
                        class_idx: c,
                    });
                }
            }
        }
        if post.alien > 0.0 {
            atoms.push(Atom {
                group: "unknown".into(),
                weight: post.alien,
                kind: AtomKind::Alien,
                class_idx: 0,
            });
        }
    } else {
        for (c, item) in compiled.bank.classes.iter().enumerate() {
            atoms.push(Atom {
                group: item.id.clone(),
                weight: 1.0,
                kind: AtomKind::Known,
                class_idx: c,
            });
        }
    }
    atoms
}

/// 基于成对 Bhattacharyya 代理目标的贪心批选探针：每次挑出信息增益最大的 `count` 条未测探针。
pub fn next_probes<S: std::hash::BuildHasher>(
    posterior: Option<&TokenizerPosterior>,
    asked: &HashSet<String, S>,
    count: usize,
) -> Result<Vec<String>, String> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let compiled = load_compiled()?;
    let atoms = build_plan_atoms(&compiled, posterior);

    let total: f64 = atoms.iter().map(|a| a.weight).sum::<f64>().max(1e-30);
    let mut pairs = Vec::new();
    for a in 0..atoms.len() {
        for b in (a + 1)..atoms.len() {
            if atoms[a].group != atoms[b].group {
                let w = (atoms[a].weight * atoms[b].weight).sqrt() / total;
                if w > 0.0 {
                    pairs.push((a, b, w));
                }
            }
        }
    }
    if pairs.is_empty() {
        return Ok(Vec::new());
    }

    let candidates: Vec<(String, usize)> = compiled
        .bank
        .probes
        .iter()
        .enumerate()
        .filter(|(_, p)| !asked.contains(&p.id))
        .map(|(j, p)| (p.id.clone(), j))
        .collect();

    let mut coeffs: Vec<(String, Vec<f64>)> = Vec::with_capacity(candidates.len());
    for (id, j) in &candidates {
        let min_v = compiled
            .values
            .iter()
            .map(|cls| cls[*j][0])
            .min()
            .unwrap_or(0);
        let max_v = compiled
            .values
            .iter()
            .map(|cls| cls[*j][0])
            .max()
            .unwrap_or(0);
        let low = min_v - PLAN_WINDOW;
        let size = usize::try_from((max_v + PLAN_WINDOW - low + 1).max(1)).unwrap_or(1);
        let roots: Vec<Vec<f64>> = atoms
            .iter()
            .map(|atom| atom_roots(&compiled, atom, *j, low, size))
            .collect();
        let bc: Vec<f64> = pairs
            .iter()
            .map(|&(a, b, _)| dot(&roots[a], &roots[b]))
            .collect();
        coeffs.push((id.clone(), bc));
    }

    let mut remaining = vec![1.0f64; pairs.len()];
    let mut chosen = Vec::with_capacity(count);
    while chosen.len() < count {
        let mut best: Option<(usize, f64)> = None;
        for (idx, (id, bc)) in coeffs.iter().enumerate() {
            if chosen.iter().any(|c| c == id) {
                continue;
            }
            let gain: f64 = pairs
                .iter()
                .enumerate()
                .map(|(t, &(_, _, w))| w * remaining[t] * (1.0 - bc[t]))
                .sum();
            if best.is_none_or(|(_, best_gain)| gain > best_gain) {
                best = Some((idx, gain));
            }
        }
        let Some((best_idx, gain)) = best else {
            break;
        };
        if gain < MINIMUM_GAIN {
            break;
        }
        chosen.push(coeffs[best_idx].0.clone());
        for (t, &bc_t) in coeffs[best_idx].1.iter().enumerate() {
            remaining[t] *= bc_t;
        }
    }
    Ok(chosen)
}

#[must_use]
pub fn probing_done(posterior: &TokenizerPosterior, max_probes: usize) -> bool {
    if posterior.answered >= max_probes {
        return true;
    }
    if posterior.answered < MINIMUM_PROBES {
        return false;
    }
    let top_exact = posterior.classes.first().map_or(0.0, |c| c.exact);
    top_exact.max(posterior.unknown) >= STOP_AT
}

fn normalize_model_id(model: &str) -> String {
    let trimmed = model.trim().to_ascii_lowercase();
    let after_slash = trimmed.rsplit('/').next().unwrap_or(&trimmed);
    after_slash
        .split(':')
        .next()
        .unwrap_or(after_slash)
        .to_owned()
}

/// 按后验分布和声明模型名给出 Tokenizer 判定（`exact`/`related`/`unknown` + 声明一致性校验）。
pub fn compute_verdict(
    posterior: &TokenizerPosterior,
    declared_model: &str,
) -> Result<TokenizerVerdict, String> {
    let compiled = load_compiled()?;
    let by_exact = posterior
        .classes
        .iter()
        .max_by(|a, b| a.exact.total_cmp(&b.exact))
        .ok_or_else(|| "empty tokenizer classes".to_owned())?;
    let by_related = posterior
        .classes
        .iter()
        .max_by(|a, b| a.related.total_cmp(&b.related))
        .ok_or_else(|| "empty tokenizer classes".to_owned())?;

    let (kind, top, confidence) = if by_exact.exact >= posterior.unknown {
        (TokenizerVerdictKind::Exact, by_exact.clone(), by_exact.exact)
    } else if by_related.related >= posterior.alien {
        (
            TokenizerVerdictKind::Related,
            by_related.clone(),
            by_related.related,
        )
    } else {
        (
            TokenizerVerdictKind::Unknown,
            posterior.classes[0].clone(),
            posterior.unknown,
        )
    };

    let norm = normalize_model_id(declared_model);
    let expected = if norm.is_empty() {
        None
    } else if let Some((idx, entry)) = compiled
        .bank
        .api_models
        .iter()
        .enumerate()
        .find(|(i, _)| compiled.api_patterns[*i].is_match(&norm))
    {
        let _ = idx;
        Some((
            entry.class.iter().cloned().collect::<Vec<_>>(),
            entry.vendor.clone(),
            true,
        ))
    } else {
        let matched_classes: Vec<&TokenizerClass> = compiled
            .bank
            .classes
            .iter()
            .enumerate()
            .filter(|(i, _)| compiled.class_aliases[*i].iter().any(|re| re.is_match(&norm)))
            .map(|(_, c)| c)
            .collect();
        (!matched_classes.is_empty()).then(|| {
            (
                matched_classes.iter().map(|c| c.id.clone()).collect(),
                matched_classes[0].lab_name.clone(),
                false,
            )
        })
    };

    let claim = expected.map(|(expected_ids, vendor, first_party)| {
        let probability = if expected_ids.is_empty() {
            // 厂商未公开分词器（如 Claude / Gemini）：期望落入 unknown（非已知开源分词器）
            posterior.unknown
        } else {
            posterior
                .classes
                .iter()
                .filter(|c| expected_ids.iter().any(|e| e == &c.id))
                .map(|c| c.exact + if first_party { 0.0 } else { c.related })
                .sum()
        };
        let status = if probability >= 0.9 {
            ClaimStatus::Consistent
        } else if probability <= 0.1 {
            ClaimStatus::Inconsistent
        } else {
            ClaimStatus::Uncertain
        };
        ClaimCheck {
            expected: expected_ids,
            vendor,
            first_party,
            probability,
            status,
        }
    });

    Ok(TokenizerVerdict {
        kind,
        top,
        confidence,
        claim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_bank_loads_and_plans_probes() {
        let (classes, probes, _) = bank_info().expect("embedded bank loads");
        assert!(classes >= 50, "expected >=50 tokenizer classes, got {classes}");
        assert!(probes >= 50, "expected >=50 probes, got {probes}");
        let asked = HashSet::new();
        let initial = next_probes(None, &asked, 4).expect("initial plan");
        assert_eq!(initial.len(), 4);
        assert!(wrap_probe_text(None).unwrap().contains("Reply with OK."));
        assert!(wrap_probe_text(Some(&initial[0])).unwrap().len() > 20);
    }

    #[test]
    fn identifies_o200k_and_flags_claude_spoof_as_inconsistent() {
        let compiled = load_compiled().unwrap();
        let o200k_idx = compiled
            .bank
            .classes
            .iter()
            .position(|c| c.id == "o200k")
            .expect("o200k class");
        let overhead = 23i64;
        let mut obs = vec![TokenizerObservation {
            probe: None,
            tokens: overhead,
            response_model: Some("gpt-5.4".into()),
        }];
        let mut asked = HashSet::new();
        for _ in 0..3 {
            let post = compute_posterior(&obs).unwrap();
            if post.as_ref().is_some_and(|p| probing_done(p, 12)) {
                break;
            }
            let batch = next_probes(post.as_ref(), &asked, 4).unwrap();
            for id in batch {
                let j = compiled.probe_index[&id];
                let cnt = compiled.bank.classes[o200k_idx].counts[j];
                obs.push(TokenizerObservation {
                    probe: Some(id.clone()),
                    tokens: overhead + cnt,
                    response_model: None,
                });
                asked.insert(id);
            }
        }
        let post = compute_posterior(&obs).unwrap().expect("posterior");
        let gpt = compute_verdict(&post, "gpt-5.4").unwrap();
        assert_eq!(gpt.kind, TokenizerVerdictKind::Exact);
        assert_eq!(gpt.top.id, "o200k");
        assert_eq!(gpt.claim.as_ref().unwrap().status, ClaimStatus::Consistent);

        // 拿 o200k 分词器冒充 Claude 必须判为不一致
        let spoofed_claude = compute_verdict(&post, "claude-opus-4-6").unwrap();
        assert_eq!(
            spoofed_claude.claim.as_ref().unwrap().status,
            ClaimStatus::Inconsistent
        );
    }
}
