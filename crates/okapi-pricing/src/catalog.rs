//! LiteLLM 社区价目表的内置快照：模型表单里的**建议价 / 规格 / 能力提示**。
//!
//! 它是输入辅助，不是价簿：建议价只在管理员点击后进入表单草稿，照常保存、发布，发布后的价簿快照才是
//! 计费依据（可回放）。目录没有的模型、目录写错的价格，都不影响计费。快照由
//! `scripts/refresh-pricing-catalog.py` 刷新，来源与许可见 `data/LICENSE-litellm.txt` 和
//! `docs/pricing-catalog.md`。
//!
//! 匹配只做"同一个名字"：大小写、`@` 与 `-`、`provider/` 前缀不计较，其余一概不猜——没有前缀推断，
//! 也没有"最接近的版本"。同名条目分散在多个厂商 / 云上时，取第一方（OpenAI、Anthropic、Google……）
//! 的那条，其余作为备选列出。
//!
//! 红线同本 crate 其余模块：全程整数十进制（[`Dec`]），无浮点；生产代码无 panic 类调用。

use okapi_domain::money::format_scaled_1e6;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

const SNAPSHOT: &str = include_str!("../data/litellm-catalog.json");

/// 备选条目最多列几个。
const MAX_ALTERNATIVES: usize = 5;

// ---- 十进制 ----

/// 小数位数：内部值 = 真实值 × 10^18。目录里最小的 token 单价在 1e-9 量级，
/// 换算到每百万 token 与做比值时都不丢位。
const SCALE: i32 = 18;
const ONE: i128 = 1_000_000_000_000_000_000;

/// 非负十进制数（USD 单价 / 每 token 或每次）。不经过浮点。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dec(i128);

impl Dec {
    /// 解析 `0.0000025`、`2.5e-06`、`0`；负数、空串、带符号 / 多余字符都拒绝。
    /// 超出 18 位小数的部分四舍五入。
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (mantissa, exponent) = match text.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
            None => (text, 0),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty() && fraction.is_empty() {
            return None;
        }
        if !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let digits: i128 = format!("{whole}{fraction}").parse().ok()?;
        let power = exponent
            .checked_sub(i32::try_from(fraction.len()).ok()?)?
            .checked_add(SCALE)?;
        let value = if power >= 0 {
            digits.checked_mul(10_i128.checked_pow(u32::try_from(power).ok()?)?)?
        } else {
            let divisor = 10_i128.checked_pow(u32::try_from(power.checked_neg()?).ok()?)?;
            digits.checked_add(divisor / 2)? / divisor
        };
        Some(Self(value))
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// USD / token → micro-USD / 百万 token（四舍五入）。
    fn micro_per_million(self) -> Option<i64> {
        i64::try_from(self.0.checked_add(500_000)? / 1_000_000).ok()
    }

    /// `self / base` 的 6 位小数定点（四舍五入）；`base` 为 0 时没有意义。
    fn ratio_scaled(self, base: Self) -> Option<i64> {
        if base.0 == 0 {
            return None;
        }
        let scaled = self.0.checked_mul(1_000_000)?.checked_add(base.0 / 2)? / base.0;
        i64::try_from(scaled).ok()
    }

    /// 精确的十进制文本（最多 18 位小数、去尾零）。
    fn text(self) -> String {
        let (whole, fraction) = (self.0 / ONE, self.0 % ONE);
        if fraction == 0 {
            return whole.to_string();
        }
        let fraction = format!("{fraction:018}");
        format!("{whole}.{}", fraction.trim_end_matches('0'))
    }

    fn per_million_text(self) -> Option<String> {
        self.micro_per_million().map(format_scaled_1e6)
    }

    fn ratio_text(self, base: Self) -> Option<String> {
        self.ratio_scaled(base).map(format_scaled_1e6)
    }
}

impl<'de> Deserialize<'de> for Dec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // serde_json 开了 arbitrary_precision：Number 保留文件里的原始数字文本
        let number = serde_json::Number::deserialize(deserializer)?;
        Self::parse(&number.to_string())
            .ok_or_else(|| serde::de::Error::custom("not a non-negative decimal"))
    }
}

// ---- 快照 ----

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub dataset: String,
    pub license: String,
    pub source: String,
    pub commit: Option<String>,
    pub committed_at: Option<String>,
    pub generated_at: String,
    pub entries: usize,
}

/// 与 `scripts/refresh-pricing-catalog.py` 的输出一一对应；多出来的字段会让解析失败，
/// 脚本与代码不会悄悄走样。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Entry {
    provider: String,
    mode: String,
    input: Option<Dec>,
    output: Option<Dec>,
    cache_read: Option<Dec>,
    cache_write: Option<Dec>,
    cache_write_1h: Option<Dec>,
    input_above_200k: Option<Dec>,
    output_above_200k: Option<Dec>,
    input_above_272k: Option<Dec>,
    output_above_272k: Option<Dec>,
    input_audio: Option<Dec>,
    output_audio: Option<Dec>,
    output_reasoning: Option<Dec>,
    per_image: Option<Dec>,
    per_query: Option<Dec>,
    input_second: Option<Dec>,
    output_second: Option<Dec>,
    batch: bool,
    flex: bool,
    priority: bool,
    context: Option<u64>,
    max_output: Option<u64>,
    vision: Option<bool>,
    tools: Option<bool>,
    parallel_tools: Option<bool>,
    reasoning: Option<bool>,
    prompt_cache: Option<bool>,
    audio_in: Option<bool>,
    audio_out: Option<bool>,
    video_in: Option<bool>,
    web_search: Option<bool>,
    schema: Option<bool>,
    computer_use: Option<bool>,
    system_prompt: Option<bool>,
    input_modalities: Vec<String>,
    output_modalities: Vec<String>,
    deprecated: Option<String>,
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    meta: Meta,
    models: BTreeMap<String, Entry>,
}

struct Row {
    key: String,
    /// `key` 里去掉 `provider/` 前缀之后的起点。
    bare_at: usize,
    /// 折叠后的 bare id（小写、`@` → `-`），搜索与匹配都用它。
    folded: String,
    entry: Entry,
}

impl Row {
    fn bare(&self) -> &str {
        self.key.get(self.bare_at..).unwrap_or(&self.key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("pricing catalog: {0}")]
pub struct CatalogError(String);

pub struct Catalog {
    meta: Meta,
    rows: Vec<Row>,
    by_key: HashMap<String, usize>,
    by_bare: HashMap<String, Vec<usize>>,
}

/// 内置快照（首次使用时解析一次）。解析失败只会发生在二进制本身带了坏数据的情况，
/// 测试里有一条会在构建期拦住它。
pub fn embedded() -> Result<&'static Catalog, &'static CatalogError> {
    static CATALOG: OnceLock<Result<Catalog, CatalogError>> = OnceLock::new();
    CATALOG.get_or_init(|| Catalog::parse(SNAPSHOT)).as_ref()
}

fn fold(text: &str) -> String {
    text.trim().to_lowercase().replace('@', "-")
}

/// `provider` 属于 `prefix` 这个厂商家族：`vertex_ai-anthropic_models` 属于 `vertex_ai`。
fn in_family(provider: &str, prefix: &str) -> bool {
    provider == prefix
        || provider
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
}

/// key 里厂商前缀之后的部分：`gemini/gemini-3.5-flash` → 起点指向 `gemini-3.5-flash`；
/// `low/1024-x-1024/gpt-image-1` 这种变体前缀不是厂商，原样保留。
fn bare_start(key: &str, provider: &str) -> usize {
    match key.split_once('/') {
        Some((prefix, _)) if in_family(provider, prefix) => prefix.len() + 1,
        _ => 0,
    }
}

/// 同名条目的取舍顺序：第一方 → 云厂商 → 其余；同级时不带路径前缀的、更短的优先。
const FIRST_PARTY: &[&str] = &[
    "openai",
    "anthropic",
    "gemini",
    "deepseek",
    "xai",
    "mistral",
    "moonshot",
    "dashscope",
    "volcengine",
    "zai",
    "minimax",
    "cohere",
    "cohere_chat",
    "perplexity",
    "groq",
];
const CLOUDS: &[&str] = &[
    "vertex_ai-language-models",
    "vertex_ai-anthropic_models",
    "azure",
    "azure_ai",
    "bedrock",
    "bedrock_converse",
];

fn provider_rank(provider: &str) -> usize {
    FIRST_PARTY
        .iter()
        .position(|candidate| *candidate == provider)
        .or_else(|| {
            CLOUDS
                .iter()
                .position(|candidate| *candidate == provider)
                .map(|index| FIRST_PARTY.len() + index)
        })
        .unwrap_or(FIRST_PARTY.len() + CLOUDS.len())
}

fn preference(row: &Row) -> (usize, bool, usize, &str) {
    (
        provider_rank(&row.entry.provider),
        row.key.contains('/'),
        row.key.len(),
        &row.key,
    )
}

impl Catalog {
    /// 解析 `scripts/refresh-pricing-catalog.py` 的输出。
    pub fn parse(json: &str) -> Result<Self, CatalogError> {
        let snapshot: Snapshot =
            serde_json::from_str(json).map_err(|error| CatalogError(error.to_string()))?;
        let mut rows = Vec::with_capacity(snapshot.models.len());
        let mut by_key = HashMap::new();
        let mut by_bare: HashMap<String, Vec<usize>> = HashMap::new();
        for (key, entry) in snapshot.models {
            let bare_at = bare_start(&key, &entry.provider);
            let folded = fold(key.get(bare_at..).unwrap_or(&key));
            let index = rows.len();
            by_key.insert(fold(&key), index);
            by_bare.entry(folded.clone()).or_default().push(index);
            rows.push(Row {
                key,
                bare_at,
                folded,
                entry,
            });
        }
        if rows.is_empty() {
            return Err(CatalogError("empty catalog".into()));
        }
        Ok(Self {
            meta: snapshot.meta,
            rows,
            by_key,
            by_bare,
        })
    }

    #[must_use]
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 以 `index` 这条为准，同一个 bare id 的其余条目按取舍顺序排成备选。
    fn found(&self, index: usize, how: Matched) -> Option<Found<'_>> {
        let row = self.rows.get(index)?;
        let mut others: Vec<&Row> = self
            .by_bare
            .get(&row.folded)
            .into_iter()
            .flatten()
            .filter(|other| **other != index)
            .filter_map(|other| self.rows.get(*other))
            .collect();
        others.sort_by_key(|&other| preference(other));
        Some(Found { row, how, others })
    }

    /// 在候选里按取舍顺序选一条。
    fn pick(&self, ids: &[usize], how: Matched) -> Option<Found<'_>> {
        let (best, _) = ids
            .iter()
            .filter_map(|index| self.rows.get(*index).map(|row| (*index, row)))
            .min_by_key(|&(_, row)| preference(row))?;
        self.found(best, how)
    }

    fn find(&self, name: &str) -> Option<Found<'_>> {
        let wanted = fold(name);
        if wanted.is_empty() {
            return None;
        }
        if let Some((prefix, rest)) = wanted.split_once('/') {
            // 写明了厂商 / 路径的名字：整串当 key 精确匹配……
            if let Some(&index) = self.by_key.get(&wanted) {
                return self.found(index, Matched::Exact);
            }
            // ……或者 `厂商/模型`，在该厂商家族里找
            let family: Vec<usize> = self
                .by_bare
                .get(rest)
                .into_iter()
                .flatten()
                .copied()
                .filter(|index| {
                    self.rows
                        .get(*index)
                        .is_some_and(|row| in_family(&row.entry.provider, prefix))
                })
                .collect();
            if !family.is_empty() {
                return self.pick(&family, Matched::Prefixed);
            }
        }
        let found = self.pick(self.by_bare.get(&wanted)?, Matched::Bare)?;
        let how = if fold(&found.row.key) == wanted {
            Matched::Exact
        } else {
            Matched::Bare
        };
        Some(Found { how, ..found })
    }

    /// 同名的建议；没有这个名字就是 `None`（不猜）。
    #[must_use]
    pub fn suggest(&self, name: &str) -> Option<Suggestion> {
        self.find(name).map(|found| Suggestion::build(&found))
    }

    /// 按名字联想：每个 bare id 一条（取第一方那条），完全匹配 → 前缀 → 词边界 → 包含。
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let terms: Vec<String> = query
            .split(|c: char| c.is_whitespace() || c == ',')
            .map(fold)
            .filter(|term| !term.is_empty())
            .collect();
        let Some(first) = terms.first() else {
            return Vec::new();
        };
        let mut best: HashMap<&str, &Row> = HashMap::new();
        let mut listings: HashMap<&str, HashSet<&str>> = HashMap::new();
        for row in &self.rows {
            if !terms.iter().all(|term| row.folded.contains(term.as_str())) {
                continue;
            }
            listings
                .entry(row.folded.as_str())
                .or_default()
                .insert(row.entry.provider.as_str());
            let slot = best.entry(row.folded.as_str()).or_insert(row);
            if preference(row) < preference(slot) {
                *slot = row;
            }
        }
        let mut hits: Vec<&Row> = best.into_values().collect();
        hits.sort_by_key(|&row| {
            (
                match_score(&row.folded, first),
                preference(row),
                row.folded.len(),
            )
        });
        hits.into_iter()
            .take(limit.clamp(1, 50))
            .map(|row| {
                let providers = listings.get(row.folded.as_str()).map_or(1, HashSet::len);
                Hit::build(row, providers)
            })
            .collect()
    }
}

/// 0 完全相同 · 1 前缀 · 2 词边界 · 3 包含。
fn match_score(folded: &str, term: &str) -> usize {
    if folded == term {
        return 0;
    }
    if folded.starts_with(term) {
        return 1;
    }
    let boundary = folded.match_indices(term).any(|(at, _)| {
        folded
            .get(..at)
            .and_then(|head| head.chars().next_back())
            .is_some_and(|c| matches!(c, '-' | '.' | '/' | ':' | '_'))
    });
    if boundary { 2 } else { 3 }
}

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

struct Found<'a> {
    row: &'a Row,
    how: Matched,
    others: Vec<&'a Row>,
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
    fn build(found: &Found<'_>) -> Self {
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
    fn build(row: &Row, listings: usize) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 手写的小目录：覆盖厂商前缀、云厂商同名条目、`@` 写法、各类单价。
    const FIXTURE: &str = r#"{
"meta":{"dataset":"litellm","license":"MIT","source":"test","commit":null,"committed_at":null,"generated_at":"2026-10-24","entries":14},
"models":{
"gpt-4o":{"provider":"openai","mode":"chat","input":0.0000025,"output":0.00001,"cache_read":0.00000125,"context":128000,"max_output":16384,"vision":true,"tools":true,"prompt_cache":true,"batch":true,"priority":true,"source":"https://example.test/openai"},
"azure/gpt-4o":{"provider":"azure","mode":"chat","input":0.0000025,"output":0.00001},
"openrouter/openai/gpt-4o":{"provider":"openrouter","mode":"chat","input":0.000003,"output":0.000012},
"claude-sonnet-4-5":{"provider":"anthropic","mode":"chat","input":0.000003,"output":0.000015,"cache_read":3e-7,"cache_write":0.00000375,"cache_write_1h":0.000006,"input_above_200k":0.000006,"output_above_200k":0.0000225,"deprecated":"2026-11-30","reasoning":true},
"claude-3-5-sonnet-20240620":{"provider":"anthropic","mode":"chat","input":0.000003,"output":0.000015},
"vertex_ai/claude-3-5-sonnet@20240620":{"provider":"vertex_ai-anthropic_models","mode":"chat","input":0.000003,"output":0.000015},
"anthropic.claude-3-5-sonnet-20240620-v1:0":{"provider":"bedrock","mode":"chat","input":0.000003,"output":0.000015},
"gemini-3.5-flash":{"provider":"vertex_ai-language-models","mode":"chat","input":0.0000015,"output":0.000009,"output_reasoning":0.000009,"input_audio":0.0000015},
"gemini/gemini-3.5-flash":{"provider":"gemini","mode":"chat","input":0.0000015,"output":0.000009,"audio_in":true,"audio_out":false,"input_modalities":["audio","image","text"],"output_modalities":["text"]},
"deepseek-chat":{"provider":"deepseek","mode":"chat","input":2.8e-7,"output":4.2e-7,"cache_read":2.8e-8},
"text-embedding-3-small":{"provider":"openai","mode":"embedding","input":0.00000002,"output":0,"context":8191},
"dall-e-3":{"provider":"openai","mode":"image_generation","per_image":0.04},
"whisper-1":{"provider":"openai","mode":"audio_transcription","input_second":0.0001},
"free-local":{"provider":"ollama","mode":"chat","input":0,"output":0}
}
}"#;

    fn catalog() -> Catalog {
        Catalog::parse(FIXTURE).unwrap()
    }

    #[test]
    fn decimals_parse_exactly_and_refuse_garbage() {
        let text = |input: &str| Dec::parse(input).map(Dec::text);
        assert_eq!(text("0.0000025").as_deref(), Some("0.0000025"));
        assert_eq!(text("2.5e-06").as_deref(), Some("0.0000025"));
        assert_eq!(text("2.5E-6").as_deref(), Some("0.0000025"));
        assert_eq!(text("1e-9").as_deref(), Some("0.000000001"));
        assert_eq!(text("12").as_deref(), Some("12"));
        assert_eq!(text("0").as_deref(), Some("0"));
        assert_eq!(text("0.0").as_deref(), Some("0"));
        for bad in ["", ".", "-1", "+1", "1.2.3", "abc", "1e", "1e9999", "0x10"] {
            assert_eq!(Dec::parse(bad), None, "{bad}");
        }
        // 超过 18 位小数的部分四舍五入，不报错
        assert_eq!(text("0.0000000000000000004").as_deref(), Some("0"));
        assert_eq!(
            text("0.0000000000000000005").as_deref(),
            Some("0.000000000000000001")
        );
    }

    #[test]
    fn per_million_and_ratio_use_six_places_without_floats() {
        let price = |input: &str| Dec::parse(input).unwrap();
        assert_eq!(
            price("0.0000025").per_million_text().as_deref(),
            Some("2.5")
        );
        assert_eq!(price("2.8e-7").per_million_text().as_deref(), Some("0.28"));
        assert_eq!(
            price("0.000000001").per_million_text().as_deref(),
            Some("0.001")
        );
        assert_eq!(price("0").per_million_text().as_deref(), Some("0"));
        // 每百万只留 6 位小数，再细的部分四舍五入：4.9e-13 USD/token = 0.00000049 USD/百万
        assert_eq!(
            price("0.00000000000049").per_million_text().as_deref(),
            Some("0")
        );
        assert_eq!(
            price("0.0000000000005").per_million_text().as_deref(),
            Some("0.000001")
        );
        let ratio = |value: &str, base: &str| price(value).ratio_text(price(base));
        assert_eq!(ratio("3e-7", "0.000003").as_deref(), Some("0.1"));
        assert_eq!(ratio("0.00000375", "0.000003").as_deref(), Some("1.25"));
        assert_eq!(ratio("0.000006", "0.000003").as_deref(), Some("2"));
        // 1/30 取到 6 位小数，与价簿的倍率精度一致
        assert_eq!(ratio("1", "30").as_deref(), Some("0.033333"));
        assert_eq!(ratio("1", "0"), None);
    }

    #[test]
    fn a_name_matches_its_own_entry_in_any_case_or_snapshot_spelling() {
        let catalog = catalog();
        let key = |name: &str| catalog.suggest(name).map(|s| s.key);
        assert_eq!(key("gpt-4o").as_deref(), Some("gpt-4o"));
        assert_eq!(key("  GPT-4O ").as_deref(), Some("gpt-4o"));
        // Vertex 写法 `@日期` 与 API 写法 `-日期` 是同一个快照
        assert_eq!(
            key("claude-3-5-sonnet@20240620").as_deref(),
            Some("claude-3-5-sonnet-20240620")
        );
        // 只有 `厂商/` 前缀这一层不计较；其余一概不猜
        assert_eq!(key("openai/gpt-4o").as_deref(), Some("gpt-4o"));
        assert_eq!(key("azure/gpt-4o").as_deref(), Some("azure/gpt-4o"));
        assert_eq!(key("gpt-4o-2024-08-06"), None);
        assert_eq!(key("gpt-4"), None);
        assert_eq!(key("claude-sonnet-4"), None);
        assert_eq!(key(""), None);
        assert_eq!(key("   "), None);
    }

    #[test]
    fn same_name_on_several_providers_prefers_the_first_party_and_lists_the_rest() {
        let catalog = catalog();
        let suggestion = catalog.suggest("gpt-4o").unwrap();
        assert_eq!(suggestion.provider, "openai");
        assert_eq!(suggestion.matched, Matched::Exact);
        let others: Vec<_> = suggestion
            .alternatives
            .iter()
            .map(|a| a.provider.as_str())
            .collect();
        assert_eq!(others, ["azure"]);
        assert_eq!(
            suggestion.alternatives[0].input_per_1m.as_deref(),
            Some("2.5")
        );

        // `openrouter/openai/gpt-4o` 是另一个 id（带路径的市场条目），不算 gpt-4o 的备选
        assert_eq!(
            catalog
                .suggest("openrouter/openai/gpt-4o")
                .unwrap()
                .provider,
            "openrouter"
        );

        // Gemini 的裸 key 是 Vertex 的条目，AI Studio 的挂在 `gemini/` 下：第一方（gemini）优先
        let flash = catalog.suggest("gemini-3.5-flash").unwrap();
        assert_eq!(flash.key, "gemini/gemini-3.5-flash");
        assert_eq!(flash.provider, "gemini");
        assert_eq!(flash.matched, Matched::Bare);
        assert_eq!(flash.alternatives.len(), 1);
        assert_eq!(flash.alternatives[0].provider, "vertex_ai-language-models");

        // 明确写了完整 key 就给那一条
        let vertex = catalog
            .suggest("vertex_ai/claude-3-5-sonnet@20240620")
            .unwrap();
        assert_eq!(vertex.provider, "vertex_ai-anthropic_models");
        assert_eq!(vertex.matched, Matched::Exact);
        assert_eq!(
            catalog
                .suggest("anthropic.claude-3-5-sonnet-20240620-v1:0")
                .unwrap()
                .provider,
            "bedrock"
        );
        // 厂商家族前缀：`vertex_ai/` 覆盖 `vertex_ai-anthropic_models`
        assert_eq!(
            catalog
                .suggest("vertex_ai/claude-3-5-sonnet-20240620")
                .unwrap()
                .provider,
            "vertex_ai-anthropic_models"
        );
    }

    #[test]
    fn token_prices_cache_lanes_and_notes_come_out_as_decimal_text() {
        let catalog = catalog();
        let sonnet = catalog.suggest("claude-sonnet-4-5").unwrap();
        assert_eq!(sonnet.billing, Billing::Ratio);
        assert_eq!(sonnet.kind, Some("chat"));
        assert_eq!(sonnet.input_per_1m.as_deref(), Some("3"));
        assert_eq!(sonnet.output_per_1m.as_deref(), Some("15"));
        assert_eq!(sonnet.cache_read_ratio.as_deref(), Some("0.1"));
        assert_eq!(sonnet.cache_write_ratio.as_deref(), Some("1.25"));
        assert_eq!(sonnet.cache_write_1h_ratio.as_deref(), Some("2"));
        assert_eq!(sonnet.deprecated.as_deref(), Some("2026-11-30"));
        assert_eq!(sonnet.capabilities.get("reasoning"), Some(&true));
        let long = sonnet
            .notes
            .iter()
            .find(|n| n.code == "long_context")
            .unwrap();
        assert_eq!(long.params["above_tokens"], "200000");
        assert_eq!(long.params["input_per_1m"], "6");
        assert_eq!(long.params["output_per_1m"], "22.5");

        let gpt = catalog.suggest("gpt-4o").unwrap();
        assert_eq!(gpt.cache_read_ratio.as_deref(), Some("0.5"));
        assert_eq!(gpt.cache_write_ratio, None);
        assert_eq!(gpt.context_window, Some(128_000));
        assert_eq!(gpt.max_output, Some(16_384));
        assert_eq!(
            gpt.source_url.as_deref(),
            Some("https://example.test/openai")
        );
        let tiers = gpt
            .notes
            .iter()
            .find(|n| n.code == "service_tiers")
            .unwrap();
        assert_eq!(tiers.params["tiers"], "batch,priority");
        assert_eq!(gpt.input_modalities, ["text", "image"]);
        assert_eq!(gpt.output_modalities, ["text"]);

        let deepseek = catalog.suggest("deepseek-chat").unwrap();
        assert_eq!(deepseek.input_per_1m.as_deref(), Some("0.28"));
        assert_eq!(deepseek.output_per_1m.as_deref(), Some("0.42"));
        assert_eq!(deepseek.cache_read_ratio.as_deref(), Some("0.1"));
    }

    #[test]
    fn capabilities_are_only_what_the_catalog_declares() {
        let catalog = catalog();
        let flash = catalog.suggest("gemini/gemini-3.5-flash").unwrap();
        assert_eq!(flash.capabilities.get("audio"), Some(&true));
        assert!(
            !flash.capabilities.contains_key("vision"),
            "没声明的不当成 false"
        );
        assert_eq!(flash.input_modalities, ["audio", "image", "text"]);
        assert_eq!(flash.output_modalities, ["text"]);
        // 只声明了 audio_out: false、没声明 audio_in 的，音频能力仍是"未知"
        let reasoning_only = catalog.suggest("claude-sonnet-4-5").unwrap();
        assert!(!reasoning_only.capabilities.contains_key("audio"));
        let embedding = catalog.suggest("text-embedding-3-small").unwrap();
        assert_eq!(embedding.capabilities.get("embedding"), Some(&true));
    }

    #[test]
    fn non_token_models_report_the_billing_they_can_and_cannot_express() {
        let catalog = catalog();
        let embedding = catalog.suggest("text-embedding-3-small").unwrap();
        assert_eq!(embedding.kind, Some("embedding"));
        assert_eq!(embedding.billing, Billing::Ratio);
        assert_eq!(embedding.input_per_1m.as_deref(), Some("0.02"));
        assert_eq!(embedding.output_per_1m, None, "嵌入没有输出价，不填 0");

        let image = catalog.suggest("dall-e-3").unwrap();
        assert_eq!(image.kind, Some("image_generation"));
        assert_eq!(image.billing, Billing::PerCall);
        assert_eq!(image.per_call_usd.as_deref(), Some("0.04"));
        assert_eq!(image.input_per_1m, None);
        assert_eq!(image.output_modalities, ["image"]);

        let speech = catalog.suggest("whisper-1").unwrap();
        assert_eq!(speech.kind, Some("speech_to_text"));
        assert_eq!(speech.billing, Billing::Unsupported);
        let per_second = speech
            .notes
            .iter()
            .find(|n| n.code == "per_second")
            .unwrap();
        assert_eq!(per_second.params["input_usd"], "0.0001");

        let free = catalog.suggest("free-local").unwrap();
        assert_eq!(free.billing, Billing::Ratio);
        assert_eq!(free.input_per_1m.as_deref(), Some("0"));
        assert!(free.notes.iter().any(|n| n.code == "free"));
    }

    #[test]
    fn search_ranks_exact_then_prefix_then_inner_matches_one_row_per_id() {
        let catalog = catalog();
        let ids = |query: &str| -> Vec<String> {
            catalog
                .search(query, 20)
                .into_iter()
                .map(|hit| hit.id)
                .collect()
        };
        // gpt-4o 同时挂在 openai / azure 下：只出现一次，用第一方那条；带路径的市场条目另算一个 id
        let hits = catalog.search("gpt-4o", 20);
        assert_eq!(hits.iter().filter(|hit| hit.id == "gpt-4o").count(), 1);
        assert_eq!(hits[0].id, "gpt-4o");
        assert_eq!(hits[0].provider, "openai");
        assert_eq!(hits[0].listings, 2);
        assert_eq!(hits[0].input_per_1m.as_deref(), Some("2.5"));
        assert_eq!(ids("gpt-4o"), ["gpt-4o", "openai/gpt-4o"]);
        // 前缀匹配先于词内包含；同级的短 id 在前（别名排在带日期的快照前面）
        assert_eq!(
            ids("claude"),
            [
                "claude-sonnet-4-5",
                "claude-3-5-sonnet-20240620",
                // 词边界上的包含（`anthropic.claude-…`）排在前缀匹配之后
                "anthropic.claude-3-5-sonnet-20240620-v1:0",
            ]
        );
        assert_eq!(ids("sonnet 4-5"), ["claude-sonnet-4-5"]);
        assert!(ids("zzz-nothing").is_empty(), "没有任何名字包含它");
        assert!(ids("   ").is_empty(), "空关键词不联想整本目录");
        assert_eq!(catalog.search("gpt", 1).len(), 1);
    }

    #[test]
    fn a_bad_snapshot_is_refused_instead_of_half_loaded() {
        assert!(Catalog::parse("{}").is_err());
        assert!(
            Catalog::parse(
                r#"{"meta":{"dataset":"x","license":"x","source":"x","generated_at":"x","entries":0},"models":{}}"#
            )
            .is_err()
        );
        // 目录里出现代码不认识的字段 → 脚本与代码走样，构建期就该发现
        let drift = FIXTURE.replace(
            r#""provider":"openai","mode":"chat","input":0.0000025"#,
            r#""provider":"openai","mode":"chat","surprise":1,"input":0.0000025"#,
        );
        assert!(Catalog::parse(&drift).is_err());
        // 负价格不是价格
        let negative = FIXTURE.replace(
            r#""input":0.0000025,"output":0.00001,"cache_read":0.00000125"#,
            r#""input":-0.0000025,"output":0.00001,"cache_read":0.00000125"#,
        );
        assert!(Catalog::parse(&negative).is_err());
    }

    /// 真快照：能解析、条数合理，几个有核对依据的模型与手工核对过的预设一致。
    #[test]
    fn the_embedded_snapshot_loads_and_matches_hand_checked_presets() {
        let catalog = embedded().unwrap();
        assert!(catalog.len() > 3000, "{}", catalog.len());
        assert_eq!(catalog.meta().dataset, "litellm");
        assert!(
            catalog
                .meta()
                .commit
                .as_deref()
                .is_some_and(|commit| commit.len() == 40)
        );

        let gpt4o = catalog.suggest("gpt-4o").unwrap();
        assert_eq!(
            (
                gpt4o.input_per_1m.as_deref(),
                gpt4o.output_per_1m.as_deref()
            ),
            (Some("2.5"), Some("10"))
        );
        assert_eq!(gpt4o.cache_read_ratio.as_deref(), Some("0.5"));

        // 前端 model-presets.ts 里 2026-10-06 核对过的 Claude：缓存读 0.05× / 写 1.25× / 1h 2×
        let opus = catalog.suggest("claude-opus-5-5").unwrap();
        assert_eq!(
            (opus.input_per_1m.as_deref(), opus.output_per_1m.as_deref()),
            (Some("4"), Some("20"))
        );
        assert_eq!(opus.cache_read_ratio.as_deref(), Some("0.05"));
        assert_eq!(opus.cache_write_ratio.as_deref(), Some("1.25"));
        assert_eq!(opus.cache_write_1h_ratio.as_deref(), Some("2"));
        let fable = catalog.suggest("claude-fable-5-1").unwrap();
        assert_eq!(fable.cache_read_ratio.as_deref(), Some("0.025"));

        // GPT-6：标准价 $2 / $10，超过 272k 上下文另有加价
        let sol = catalog.suggest("gpt-6-sol").unwrap();
        assert_eq!(
            (sol.input_per_1m.as_deref(), sol.output_per_1m.as_deref()),
            (Some("2"), Some("10"))
        );
        assert!(
            sol.notes
                .iter()
                .any(|n| n.code == "long_context" && n.params["above_tokens"] == "272000")
        );

        let embedding = catalog.suggest("text-embedding-3-small").unwrap();
        assert_eq!(embedding.kind, Some("embedding"));
        assert_eq!(embedding.output_per_1m, None);

        assert!(catalog.suggest("definitely-not-a-model-9000").is_none());
    }
}
