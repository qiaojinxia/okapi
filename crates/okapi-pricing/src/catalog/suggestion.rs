use super::{Dec, Entry, MAX_ALTERNATIVES, Row};
use serde::Serialize;
use std::collections::BTreeMap;

// ---- 建议 ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Matched {
    /// 名字与目录的 key 完全一致。
    Exact,
    /// `厂商/模型` 写法，在该厂商家族里找到。
    Prefixed,
    /// 目录里这个名字挂在某个厂商前缀下（`gemini/gemini-3.5-flash`）。
    Bare,
}

pub(super) struct Found<'a> {
    pub(super) row: &'a Row,
    pub(super) how: Matched,
    pub(super) others: Vec<&'a Row>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    /// 按 token：输入 / 输出（含缓存倍率）。
    Ratio,
    /// 按次：每张图 / 每次查询。
    PerCall,
    /// 目录是按秒等 Okapi 现有计价方式表达不了的单位：只给提示，不给可填的价。
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
pub struct Note {
    pub code: &'static str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<&'static str, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Alternative {
    pub key: String,
    pub provider: String,
    pub input_per_1m: Option<String>,
    pub output_per_1m: Option<String>,
}

/// 一条目录建议。价格是 USD / 百万 token 的十进制文本，倍率是相对输入价的 6 位小数。
#[derive(Clone, Debug, Serialize)]
pub struct Suggestion {
    pub key: String,
    pub provider: String,
    pub matched: Matched,
    /// LiteLLM 的 mode 原文。
    pub mode: String,
    /// 对应的 Okapi 模型类型（`MODEL_KINDS` 之一）。
    pub kind: Option<&'static str>,
    pub billing: Billing,
    pub input_per_1m: Option<String>,
    pub output_per_1m: Option<String>,
    pub cache_read_ratio: Option<String>,
    pub cache_write_ratio: Option<String>,
    pub cache_write_1h_ratio: Option<String>,
    /// 按次价（USD）：每张图 / 每次查询。
    pub per_call_usd: Option<String>,
    /// 目录的最大输入 token（不一定等于厂商宣传的"上下文窗口"）。
    pub context_window: Option<u64>,
    pub max_output: Option<u64>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    /// Okapi 能力键 → 目录明确声明的值（没声明的不出现，不当成 false）。
    pub capabilities: BTreeMap<&'static str, bool>,
    pub notes: Vec<Note>,
    pub deprecated: Option<String>,
    pub source_url: Option<String>,
    pub alternatives: Vec<Alternative>,
}

fn kind_of(mode: &str) -> Option<&'static str> {
    Some(match mode {
        "chat" | "responses" => "chat",
        "completion" => "completion",
        "embedding" => "embedding",
        "rerank" => "rerank",
        "image_generation" | "image_edit" => "image_generation",
        "audio_transcription" => "speech_to_text",
        "audio_speech" => "text_to_speech",
        "realtime" => "realtime",
        "video_generation" => "video_generation",
        "moderation" => "moderation",
        "search" => "search",
        _ => return None,
    })
}

fn billing_of(entry: &Entry) -> Billing {
    let priced = |price: Option<Dec>| price.is_some_and(|price| !price.is_zero());
    if priced(entry.input) || priced(entry.output) {
        Billing::Ratio
    } else if entry.per_image.is_some() || entry.per_query.is_some() {
        Billing::PerCall
    } else if entry.input.is_some() || entry.output.is_some() {
        // 目录明确写了 0：免费模型
        Billing::Ratio
    } else {
        Billing::Unsupported
    }
}

fn per_million(price: Option<Dec>) -> Option<String> {
    price.and_then(Dec::per_million_text)
}

fn note(code: &'static str, params: &[(&'static str, Option<String>)]) -> Note {
    Note {
        code,
        params: params
            .iter()
            .filter_map(|(name, value)| value.clone().map(|value| (*name, value)))
            .collect(),
    }
}

/// 价格之外值得管理员看一眼的事：长上下文加价、服务档位、推理 / 音频另价、按秒计价、免费、已弃用。
fn notes_of(entry: &Entry, billing: Billing) -> Vec<Note> {
    let mut notes = Vec::new();
    for (threshold, input, output) in [
        ("200000", entry.input_above_200k, entry.output_above_200k),
        ("272000", entry.input_above_272k, entry.output_above_272k),
    ] {
        if input.is_some() || output.is_some() {
            notes.push(note(
                "long_context",
                &[
                    ("above_tokens", Some(threshold.to_owned())),
                    ("input_per_1m", per_million(input)),
                    ("output_per_1m", per_million(output)),
                ],
            ));
        }
    }
    let tiers: Vec<&str> = [
        ("batch", entry.batch),
        ("flex", entry.flex),
        ("priority", entry.priority),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect();
    if !tiers.is_empty() {
        notes.push(note("service_tiers", &[("tiers", Some(tiers.join(",")))]));
    }
    if entry.output_reasoning.is_some() && entry.output_reasoning != entry.output {
        notes.push(note(
            "reasoning_price",
            &[("output_per_1m", per_million(entry.output_reasoning))],
        ));
    }
    if entry.input_audio.is_some() || entry.output_audio.is_some() {
        notes.push(note(
            "audio_price",
            &[
                ("input_per_1m", per_million(entry.input_audio)),
                ("output_per_1m", per_million(entry.output_audio)),
            ],
        ));
    }
    if entry.input_second.is_some() || entry.output_second.is_some() {
        notes.push(note(
            "per_second",
            &[
                ("input_usd", entry.input_second.map(Dec::text)),
                ("output_usd", entry.output_second.map(Dec::text)),
            ],
        ));
    }
    if billing == Billing::Ratio
        && entry.input.is_some_and(Dec::is_zero)
        && entry.output.is_none_or(Dec::is_zero)
    {
        notes.push(note("free", &[]));
    }
    if billing == Billing::Unsupported && notes.is_empty() {
        notes.push(note("no_price", &[]));
    }
    notes
}

fn modalities_of(entry: &Entry) -> (Vec<String>, Vec<String>) {
    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }
    if !entry.input_modalities.is_empty() || !entry.output_modalities.is_empty() {
        return (
            entry.input_modalities.clone(),
            entry.output_modalities.clone(),
        );
    }
    match entry.mode.as_str() {
        "image_generation" | "image_edit" => (words(&["text"]), words(&["image"])),
        "audio_transcription" => (words(&["audio"]), words(&["text"])),
        "audio_speech" => (words(&["text"]), words(&["audio"])),
        "chat" | "responses" if entry.vision == Some(true) => {
            (words(&["text", "image"]), words(&["text"]))
        }
        _ => (Vec::new(), Vec::new()),
    }
}

fn capabilities_of(entry: &Entry) -> BTreeMap<&'static str, bool> {
    let mut out = BTreeMap::new();
    for (name, value) in [
        ("vision", entry.vision),
        ("tools", entry.tools),
        ("parallel_tools", entry.parallel_tools),
        ("structured_output", entry.schema),
        ("reasoning", entry.reasoning),
        ("prompt_cache", entry.prompt_cache),
        ("video", entry.video_in),
        ("web_search", entry.web_search),
        ("computer_use", entry.computer_use),
        ("system_prompt", entry.system_prompt),
    ] {
        if let Some(value) = value {
            out.insert(name, value);
        }
    }
    match (entry.audio_in, entry.audio_out) {
        (Some(true), _) | (_, Some(true)) => {
            out.insert("audio", true);
        }
        (Some(false), Some(false)) => {
            out.insert("audio", false);
        }
        _ => {}
    }
    match entry.mode.as_str() {
        "embedding" => {
            out.insert("embedding", true);
        }
        "realtime" => {
            out.insert("realtime", true);
        }
        _ => {}
    }
    out
}

impl Suggestion {
    pub(super) fn build(found: &Found<'_>) -> Self {
        let entry = &found.row.entry;
        let ratio = |price: Option<Dec>| price?.ratio_text(entry.input?);
        let billing = billing_of(entry);
        // 嵌入 / 重排 / 审核没有输出价：0 当作"没有"，免得表单把输出价填成 0
        let output_is_none_by_nature =
            matches!(entry.mode.as_str(), "embedding" | "rerank" | "moderation")
                && entry.output.is_some_and(Dec::is_zero);
        let (input_modalities, output_modalities) = modalities_of(entry);
        Self {
            key: found.row.key.clone(),
            provider: entry.provider.clone(),
            matched: found.how,
            mode: entry.mode.clone(),
            kind: kind_of(&entry.mode),
            billing,
            input_per_1m: per_million(entry.input),
            output_per_1m: if output_is_none_by_nature {
                None
            } else {
                per_million(entry.output)
            },
            cache_read_ratio: ratio(entry.cache_read),
            cache_write_ratio: ratio(entry.cache_write),
            cache_write_1h_ratio: ratio(entry.cache_write_1h),
            per_call_usd: entry.per_image.or(entry.per_query).map(Dec::text),
            context_window: entry.context,
            max_output: entry.max_output,
            input_modalities,
            output_modalities,
            capabilities: capabilities_of(entry),
            notes: notes_of(entry, billing),
            deprecated: entry.deprecated.clone(),
            source_url: entry.source.clone(),
            alternatives: found
                .others
                .iter()
                .take(MAX_ALTERNATIVES)
                .map(|row| Alternative {
                    key: row.key.clone(),
                    provider: row.entry.provider.clone(),
                    input_per_1m: per_million(row.entry.input),
                    output_per_1m: per_million(row.entry.output),
                })
                .collect(),
        }
    }
}

/// 联想结果的一行。
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    /// 要填进"模型名"的 id（目录 key 去掉厂商前缀）。
    pub id: String,
    pub provider: String,
    pub mode: String,
    pub kind: Option<&'static str>,
    pub input_per_1m: Option<String>,
    pub output_per_1m: Option<String>,
    /// 目录里有几个厂商挂着同一个 id。
    pub listings: usize,
}

impl Hit {
    pub(super) fn build(row: &Row, listings: usize) -> Self {
        let entry = &row.entry;
        Self {
            id: row.bare().to_owned(),
            provider: entry.provider.clone(),
            mode: entry.mode.clone(),
            kind: kind_of(&entry.mode),
            input_per_1m: per_million(entry.input),
            output_per_1m: per_million(entry.output),
            listings,
        }
    }
}
