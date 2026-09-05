//! Token 估算：tiktoken BPE 精确计数，超长 prompt 按已测比例外推。
//!
//! 取代原先的 `chars/4` 启发式。那个式子对英文勉强成立，对中文能差三到四倍——
//! 405 个汉字被算成 108 tokens（实测）。预扣算少了不会直接漏钱（结算以上游
//! usage 为准），但会让 TPM 闸门失准、余额校验形同虚设：一个中文重负载用户
//! 能在余额不足的情况下反复过预扣。上游不返 usage 时它还会直接变成计费口径。
//!
//! **精度边界**：tiktoken 是 OpenAI 系的分词器。对 OpenAI 方言上游是准的；
//! 对 Anthropic / Gemini 只是同量级近似——它们的分词器未公开且不在本进程内。
//! 用它做预扣与兜底都远好过按字符数猜，但不该被当成这两家的权威计数。

use okapi_domain::TokenUsage;
use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

/// 单次请求最多精确分词的字节数，超出部分按已测 字节/token 比外推。
///
/// BPE 是线性但常数不小的 CPU 开销，热路径上不能让一个 10MB 的 prompt 把工作
/// 线程占住。取 128KB：覆盖绝大多数真实请求（远超常见 128k 上下文的字节数
/// 量级中位），又把最坏情况钉在毫秒级。外推段的误差不影响结算——结算走上游
/// usage；它只影响预扣占用量。
const EXACT_TOKENIZE_BUDGET: usize = 128 * 1024;

/// 每条消息的协议固定开销（role/分隔符；OpenAI 计费口径约 3-4）。
const PER_MESSAGE_OVERHEAD: usize = 4;
/// 整个请求的固定开销。
const REQUEST_OVERHEAD: usize = 3;

/// 兜底：拿不到分词器时退回旧启发式，宁可估不准也不能让请求挂掉。
const FALLBACK_CHARS_PER_TOKEN: usize = 4;

fn o200k() -> Option<&'static CoreBPE> {
    static BPE: OnceLock<Option<CoreBPE>> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::o200k_base().ok()).as_ref()
}

fn cl100k() -> Option<&'static CoreBPE> {
    static BPE: OnceLock<Option<CoreBPE>> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::cl100k_base().ok()).as_ref()
}

/// 按模型名选分词器。
///
/// o200k 是当代 OpenAI 系（gpt-4o / o 系列 / gpt-5）的编码，也是 Anthropic /
/// Gemini 这些"没有本地分词器"的模型的默认近似——它对中文的切分比 cl100k 更
/// 接近现代模型的实际行为。cl100k 只留给明确的老模型（gpt-4 / gpt-3.5 / embedding）。
fn encoding_for(model: &str) -> Option<&'static CoreBPE> {
    let m = model.to_ascii_lowercase();
    let legacy = m.starts_with("gpt-4-")
        || m == "gpt-4"
        || m.starts_with("gpt-3.5")
        || m.starts_with("text-embedding");
    if legacy { cl100k() } else { o200k() }
}

/// 一段文本的 token 数：预算内精确分词，超出部分按已测比例外推。
fn count_segment(bpe: &CoreBPE, text: &str) -> usize {
    if text.len() <= EXACT_TOKENIZE_BUDGET {
        return bpe.encode_ordinary(text).len();
    }
    // 截到字符边界，避免把多字节字符劈开喂给分词器
    let mut cut = EXACT_TOKENIZE_BUDGET;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &text[..cut];
    let measured = bpe.encode_ordinary(head).len();
    if measured == 0 || cut == 0 {
        return text.len() / FALLBACK_CHARS_PER_TOKEN;
    }
    // measured/cut 即本文本自己的 token/字节 密度，比任何全局常数都贴合
    let remaining = text.len() - cut;
    measured + remaining * measured / cut
}

/// 文本片段集合的 token 数（无分词器时退字符启发式）。
fn count_texts(model: &str, segments: &[&str]) -> usize {
    match encoding_for(model) {
        Some(bpe) => segments.iter().map(|s| count_segment(bpe, s)).sum(),
        None => segments
            .iter()
            .map(|s| s.chars().count() / FALLBACK_CHARS_PER_TOKEN)
            .sum(),
    }
}

/// 启动时把 BPE 表建起来。首次分词要建表（实测 ~2.5ms），不预热就由线上第一个
/// 请求承担；分词本身典型 37-66µs，超长 prompt 被 `EXACT_TOKENIZE_BUDGET` 钉在 ~3ms。
pub fn warm_up() {
    let _ = o200k();
    let _ = cl100k();
}

/// 预扣用的 prompt token 估算：正文精确分词 + 每消息协议开销。
#[must_use]
pub fn estimate_prompt_tokens(model: &str, segments: &[&str], message_count: usize) -> u32 {
    let body = count_texts(model, segments);
    let overhead = message_count
        .saturating_mul(PER_MESSAGE_OVERHEAD)
        .saturating_add(REQUEST_OVERHEAD);
    u32::try_from(body.saturating_add(overhead)).unwrap_or(u32::MAX)
}

/// 本次请求实测的 token/字符 密度（每 1000 字符的 token 数，定点整数）。
///
/// 补全侧只拿得到字符数（`ChatEvent` 不带正文，累积全文要为每条流常驻内存），
/// 所以用 prompt 侧**本次实测**的密度去折算——同一段对话语言相同，密度稳定，
/// 比任何全局常数都贴合。中文约 600、英文约 250，而旧式 `chars/4` 恒等于 250：
/// 中文场景差出两倍多，正是它把补全算少的原因。
///
/// 计费红线：全程整数，不引入浮点（`scripts/guard-no-float.sh`）。
#[must_use]
pub fn prompt_density(prompt_tokens: u32, prompt_chars: usize) -> u32 {
    if prompt_chars == 0 {
        return DEFAULT_DENSITY;
    }
    let d = u64::from(prompt_tokens)
        .saturating_mul(1000)
        .checked_div(prompt_chars as u64)
        .unwrap_or(u64::from(DEFAULT_DENSITY));
    // 夹在合理区间：极短 prompt 的密度噪声很大，不该被放大到补全侧
    u32::try_from(d.clamp(u64::from(MIN_DENSITY), u64::from(MAX_DENSITY)))
        .unwrap_or(DEFAULT_DENSITY)
}

/// 拿不到密度时的缺省（≈ 旧式 chars/4）。
pub const DEFAULT_DENSITY: u32 = 250;
/// 密度下限：再稀疏的文本也不会低于此（防极短 prompt 把补全算成 0）。
const MIN_DENSITY: u32 = 150;
/// 密度上限：CJK 稠密文本约 600-700，留出余量后封顶。
const MAX_DENSITY: u32 = 1200;

/// 无上游 usage 时按已透传字符数 × 本次密度估算补全 tokens。
///
/// 至少记 1：产出了内容却记 0 tokens 会让这笔账看起来像没发生。
#[must_use]
pub fn estimate_completion_tokens(content_chars: usize, density: u32) -> u32 {
    let n = (content_chars as u64).saturating_mul(u64::from(density)) / 1000;
    u32::try_from(n.max(1)).unwrap_or(u32::MAX)
}

/// 渠道 `trust_upstream_usage = false` 时的本地复核。
///
/// **取两者较大值**，而不是直接用本地数：本地 prompt 计数对 OpenAI 方言是权威的，
/// 对 Anthropic / Gemini 只是近似（见模块头），补全侧更是密度折算。若直接替换，
/// 一个诚实上报的 Anthropic 渠道反而会因为近似偏低而被少收——那不是"不信任上游"
/// 想要的结果。取 max 的语义是明确的：**上游可以报得比我算的多，但不能更少**，
/// 正好对上这个开关存在的理由（怕转售型上游少报）。
///
/// 只复核 prompt 与 completion 两轴：缓存命中/音频/图片这些拆分只有上游知道，
/// 本地无从复核，原样保留。
#[must_use]
pub fn recount_untrusted(
    reported: TokenUsage,
    local_prompt: u32,
    content_chars: usize,
    density: u32,
) -> TokenUsage {
    TokenUsage {
        prompt_tokens: reported.prompt_tokens.max(local_prompt),
        completion_tokens: reported
            .completion_tokens
            .max(estimate_completion_tokens(content_chars, density)),
        ..reported
    }
}

/// 上游 usage 缺失时的整份兜底 usage。
#[must_use]
pub fn fallback_usage(est_prompt: u32, content_chars: usize, density: u32) -> TokenUsage {
    TokenUsage {
        prompt_tokens: est_prompt,
        completion_tokens: estimate_completion_tokens(content_chars, density),
        ..TokenUsage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这条是本次改动的由来：旧式 chars/4 把 405 个汉字算成 108 tokens。
    #[test]
    fn chinese_prompt_is_no_longer_undercounted_fourfold() {
        let text = "中文提示词内容测试".repeat(45); // 405 个汉字
        let old_heuristic = u32::try_from(text.chars().count() / 4).unwrap();
        let n = estimate_prompt_tokens("gpt-4o", &[text.as_str()], 1);
        assert!(
            n > old_heuristic * 2,
            "中文分词结果 {n} 应远高于旧启发式 {old_heuristic}"
        );
        assert!(n < 900, "也不该离谱地高：{n}");
    }

    #[test]
    fn english_prompt_stays_in_the_right_ballpark() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(20);
        let n = estimate_prompt_tokens("gpt-4o", &[text.as_str()], 1);
        assert!((150..=300).contains(&n), "英文估算落在合理区间：{n}");
    }

    #[test]
    fn empty_prompt_only_costs_protocol_overhead() {
        assert_eq!(
            estimate_prompt_tokens("gpt-4o", &[], 0),
            u32::try_from(REQUEST_OVERHEAD).unwrap()
        );
    }

    #[test]
    fn per_message_overhead_accumulates() {
        let a = estimate_prompt_tokens("gpt-4o", &["hi"], 1);
        let b = estimate_prompt_tokens("gpt-4o", &["hi"], 3);
        assert_eq!(b - a, u32::try_from(PER_MESSAGE_OVERHEAD * 2).unwrap());
    }

    #[test]
    fn encoding_selector_covers_legacy_and_current() {
        for m in [
            "gpt-4",
            "gpt-3.5-turbo",
            "gpt-4o",
            "claude-opus-4",
            "gemini-2.5-pro",
        ] {
            assert!(encoding_for(m).is_some(), "{m} 应有可用编码");
        }
    }

    #[test]
    fn oversized_prompt_is_extrapolated_not_truncated() {
        // 超出精确预算的部分必须计入，否则超长 prompt 会被系统性少算
        let unit = "中文提示词内容测试";
        let big = unit.repeat(EXACT_TOKENIZE_BUDGET / unit.len() * 3);
        assert!(big.len() > EXACT_TOKENIZE_BUDGET * 2);
        let n = estimate_prompt_tokens("gpt-4o", &[big.as_str()], 1);
        let mut cut = EXACT_TOKENIZE_BUDGET;
        while !big.is_char_boundary(cut) {
            cut -= 1;
        }
        let head_only = estimate_prompt_tokens("gpt-4o", &[&big[..cut]], 1);
        assert!(
            n > head_only * 2,
            "外推后应约为头部的三倍，实得 {n} vs 头部 {head_only}"
        );
    }

    #[test]
    fn chinese_density_is_far_above_the_old_constant() {
        let text = "中文提示词内容测试".repeat(45);
        let tokens = estimate_prompt_tokens("gpt-4o", &[text.as_str()], 1);
        let d = prompt_density(tokens, text.chars().count());
        assert!(
            d > DEFAULT_DENSITY * 2,
            "中文密度 {d} 应远高于旧式常数 {DEFAULT_DENSITY}"
        );
    }

    #[test]
    fn english_density_lands_near_the_old_constant() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(20);
        let tokens = estimate_prompt_tokens("gpt-4o", &[text.as_str()], 1);
        let d = prompt_density(tokens, text.chars().count());
        assert!((150..=400).contains(&d), "英文密度 {d} 与旧常数同量级");
    }

    #[test]
    fn density_is_clamped_against_short_prompt_noise() {
        assert_eq!(prompt_density(0, 0), DEFAULT_DENSITY, "无 prompt 用缺省");
        assert!(prompt_density(9_999, 1) <= MAX_DENSITY, "极端值须封顶");
        assert!(prompt_density(0, 10_000) >= MIN_DENSITY, "极端值须托底");
    }

    #[test]
    fn completion_estimate_is_never_zero() {
        assert!(estimate_completion_tokens(0, DEFAULT_DENSITY) >= 1);
    }

    #[test]
    fn completion_estimate_tracks_density() {
        let en = estimate_completion_tokens(1000, 250);
        let zh = estimate_completion_tokens(1000, 600);
        assert_eq!(en, 250);
        assert_eq!(zh, 600, "同样字数的中文补全应算出更多 token");
    }

    #[test]
    fn recount_takes_the_larger_side() {
        let reported = TokenUsage {
            prompt_tokens: 5,         // 上游少报
            completion_tokens: 9_999, // 上游多报
            ..TokenUsage::default()
        };
        let out = recount_untrusted(reported, 400, 100, 600);
        assert_eq!(out.prompt_tokens, 400, "上游少报的一侧要被本地顶上去");
        assert_eq!(out.completion_tokens, 9_999, "上游多报的一侧原样保留");
    }

    #[test]
    fn recount_preserves_axes_it_cannot_verify() {
        let reported = TokenUsage {
            prompt_tokens: 100,
            cached_tokens: 40,
            audio_prompt_tokens: 7,
            reasoning_tokens: 11,
            ..TokenUsage::default()
        };
        let out = recount_untrusted(reported, 10, 10, DEFAULT_DENSITY);
        assert_eq!(out.cached_tokens, 40);
        assert_eq!(out.audio_prompt_tokens, 7);
        assert_eq!(out.reasoning_tokens, 11);
    }
}
