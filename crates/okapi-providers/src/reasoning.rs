//! reasoning 意图的归一与注入（IMPLEMENTATION §4.4 / §11.25 / §11.26）。
//!
//! **两条来源，一个内部形状，三向注入**——这是 OpenRouter / LiteLLM 这类网关的通行结构，
//! 网关的价值恰恰就在中间那次归一：客户端只会写自己那一种方言，上游只认自己那一种。
//!
//! 来源一，**模型名**（§4.4 / §11.25）：`-high/-medium/-low`、`-thinking[-<N>]`，
//! 以及泛化后的 `base@effort:high`（见 [`crate::modifiers`]）。解析顺序由 gateway 保证：
//! 模型全名（含别名）直命中优先，未命中才剥后缀重试，因此真实存在的带后缀模型
//! （如上游确有 `o3-high`）不受影响。名字这条路**同时决定计费名**。
//!
//! 来源二，**请求体参数**（§11.26，[`parse_request`]）：`reasoning_effort`（OpenAI chat）、
//! `reasoning: {effort, max_tokens, enabled}`（OpenRouter 统一形状，也是 Responses 原生形状）、
//! `thinking: {type, budget_tokens}`（Anthropic）。参数**不影响计费名**——这与 OpenRouter
//! 一致：只有 slug 上的变体改计价，请求参数不改。
//!
//! 两条来源合并时**参数优先**（[`RequestReasoning::merge`]）：参数是本次请求的显式意图，
//! 名字后缀是模型名里带的默认值。
//!
//! 三向注入：
//! - OpenAI：`reasoning_effort`（预算档按 [`ReasoningDirective::effective_effort`] 折成档位）；
//! - Anthropic：`thinking: {type: enabled, budget_tokens}`，并保证 max_tokens > 预算；
//! - Gemini：`generationConfig.thinkingConfig.thinkingBudget`（含 includeThoughts）。
//!
//! 三个注入口都**不覆盖已存在的同名字段**：同方言透传时客户端写的就是上游要的，
//! 轮不到我们改。

use crate::error::UpstreamError;
use bytes::Bytes;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Effort {
    High,
    Medium,
    Low,
    /// OpenAI gpt-5 起的最低档（几乎不思考）。
    Minimal,
}

impl Effort {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Minimal => "minimal",
        }
    }

    /// 档位字面量 → `Effort`。**模型名后缀与请求体参数共用这一张表**，
    /// 否则 `@effort:minimal` 与 `reasoning_effort:"minimal"` 会一个认一个不认。
    ///
    /// 不认识的档（OpenRouter 的 `max`/`xhigh` 之类）返回 None——不装懂：
    /// 同方言透传时它本来就会原样送到上游，由上游判对错才是对的。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "minimal" => Some(Self::Minimal),
            _ => None,
        }
    }

    /// effort 档 → thinking 预算（anthropic/gemini 无 effort 参数时的映射）。
    #[must_use]
    pub fn budget_tokens(self) -> u32 {
        match self {
            Self::High => 16_000,
            Self::Medium => 8_000,
            Self::Low => 2_048,
            // anthropic 的 budget_tokens 下限就是 1024，再低没有意义
            Self::Minimal => 1_024,
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

    /// 生效档位（openai 注入用）：只给了预算时按 [`Effort::budget_tokens`] 的逆映射折过去。
    ///
    /// 有这个逆映射，"客户端只会说预算、上游只会说档位"（anthropic 方言客户端 →
    /// openai 方言上游，或 `-thinking-4096` 打到 openai 渠道）才不会静默丢失。
    /// 此前这条路是直接 return 原体的：用户要了思考、没要到、钱照收。
    #[must_use]
    pub fn effective_effort(&self) -> Effort {
        if let Some(e) = self.effort {
            return e;
        }
        // 阈值取 budget_tokens 各档之间的中点
        match self.budget_tokens.unwrap_or(DEFAULT_THINKING_BUDGET) {
            0..=1_024 => Effort::Minimal,
            1_025..=5_000 => Effort::Low,
            5_001..=12_000 => Effort::Medium,
            _ => Effort::High,
        }
    }
}

// ---- 来源二：请求体参数（§11.26）----

/// okapi 接受的统一 reasoning 对象键（形状对齐 OpenRouter，也是 Responses 的原生键）。
/// OpenAI chat 方言与 Anthropic 方言都不认识它，故同方言转发前要按 [`strip_unified`] 处理。
pub const UNIFIED_FIELD: &str = "reasoning";

/// 客户端在请求体里表达的 reasoning 意图。
///
/// 三态——"没提"和"明确关掉"必须分开：前者该由模型名后缀接管，后者要压过后缀
/// （`model=x@effort:high` + `reasoning:{enabled:false}` 得是不思考）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestReasoning {
    /// 请求体没提，或写的东西我们翻译不了。
    Unspecified,
    /// 明确关闭。
    Disabled,
    Enabled(ReasoningDirective),
}

impl RequestReasoning {
    /// 与模型名后缀合并。**参数优先**：参数是本次请求的显式意图，
    /// 后缀是模型名里带的默认值，显式的该压过默认的。
    #[must_use]
    pub fn merge(self, from_name: Option<ReasoningDirective>) -> Option<ReasoningDirective> {
        match self {
            Self::Unspecified => from_name,
            Self::Disabled => None,
            Self::Enabled(d) => Some(d),
        }
    }
}

/// 请求体里是否可能出现 reasoning 相关键（省掉绝大多数请求的整体反序列化）。
/// `"reasoning` 不带尾引号，一次覆盖 `"reasoning"` 与 `"reasoning_effort"`。
fn mentions_reasoning(body: &[u8]) -> bool {
    [b"\"reasoning".as_slice(), b"\"thinking\"".as_slice()]
        .iter()
        .any(|needle| body.windows(needle.len()).any(|w| w == *needle))
}

/// 从请求原文解析 reasoning 意图，与入口方言无关（三个入口共用一份）。
///
/// 认三种写法，同时出现时优先级由高到低：
/// 1. `reasoning: {effort, max_tokens, enabled}`——最表达得清楚的一种；
/// 2. `reasoning_effort: "minimal|low|medium|high"`（OpenAI chat 原生）；
/// 3. `thinking: {type: "enabled"|"disabled", budget_tokens}`（Anthropic 原生）。
///
/// **翻译不了就说翻译不了**（返回 `Unspecified`），绝不拒请求：同方言透传时这个字段
/// 本来就会原样送到上游，由上游判对错才对；我们只在真能翻译时才翻译。
#[must_use]
pub fn parse_request(body: &Bytes) -> RequestReasoning {
    if !mentions_reasoning(body) {
        return RequestReasoning::Unspecified;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return RequestReasoning::Unspecified;
    };
    let Some(obj) = value.as_object() else {
        return RequestReasoning::Unspecified;
    };

    if let Some(r) = obj.get(UNIFIED_FIELD).and_then(Value::as_object) {
        let effort_str = r
            .get("effort")
            .and_then(Value::as_str)
            .filter(|s| consumed_effort(s));
        if r.get("enabled") == Some(&Value::Bool(false)) || effort_str == Some("none") {
            return RequestReasoning::Disabled;
        }
        let effort = effort_str.and_then(Effort::parse);
        let budget_tokens = positive_u32(r.get("max_tokens"));
        if effort.is_some() || budget_tokens.is_some() {
            return RequestReasoning::Enabled(ReasoningDirective {
                effort,
                budget_tokens,
            });
        }
        // `{enabled: true}` 单独出现 = 开启、用缺省档
        if r.get("enabled") == Some(&Value::Bool(true)) {
            return RequestReasoning::Enabled(ReasoningDirective {
                effort: None,
                budget_tokens: None,
            });
        }
        return RequestReasoning::Unspecified;
    }

    if let Some(s) = obj.get("reasoning_effort").and_then(Value::as_str) {
        if s == "none" {
            return RequestReasoning::Disabled;
        }
        return Effort::parse(s).map_or(RequestReasoning::Unspecified, |effort| {
            RequestReasoning::Enabled(ReasoningDirective {
                effort: Some(effort),
                budget_tokens: None,
            })
        });
    }

    if let Some(t) = obj.get("thinking").and_then(Value::as_object) {
        if t.get("type").and_then(Value::as_str) == Some("disabled") {
            return RequestReasoning::Disabled;
        }
        return RequestReasoning::Enabled(ReasoningDirective {
            effort: None,
            budget_tokens: positive_u32(t.get("budget_tokens")),
        });
    }

    RequestReasoning::Unspecified
}

fn positive_u32(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// 这个 effort 字面量我们是否真的看懂了（`none` 算看懂：它就是"关掉"）。
fn consumed_effort(v: &str) -> bool {
    v == "none" || Effort::parse(v).is_some()
}

/// 统一对象里这一项是否已被 [`parse_request`] 消化掉。
fn consumed_key(key: &str, value: &Value) -> bool {
    match key {
        "effort" => value.as_str().is_some_and(consumed_effort),
        "max_tokens" => positive_u32(Some(value)).is_some(),
        "enabled" => value.is_boolean(),
        _ => false,
    }
}

/// 转发前处理统一 `reasoning` 对象：**只摘走我们已经消化的键**，
/// 对象被摘空了就整个删掉。无改动时返回 None（零拷贝）。
///
/// 为什么不整个删：消化过的键（effort/max_tokens/enabled）已翻译进各方言的原生字段，
/// 留着会让不认识它的上游 400；但**没消化的键必须原样送出去**——
/// `exclude` 我们没实现、`effort:"xhigh"` 我们没看懂，上游（比如另一个 OpenRouter 兼容网关）
/// 很可能认得。一把梭全删，就是把"我不懂"当成"它不存在"，正是本模块要修的那类静默丢失。
///
/// 原生的 `reasoning_effort` / `thinking` 一概不动：那是上游自己的字段。
#[must_use]
pub fn strip_unified(body: &Bytes) -> Option<Bytes> {
    if !mentions_reasoning(body) {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let root = value.as_object_mut()?;
    let obj = root.get_mut(UNIFIED_FIELD)?.as_object_mut()?;
    let before = obj.len();
    obj.retain(|k, v| !consumed_key(k, v));
    if obj.len() == before {
        return None; // 一个都没消化：原样透传，别动它
    }
    if obj.is_empty() {
        root.remove(UNIFIED_FIELD);
    }
    Some(Bytes::from(serde_json::to_vec(&value).ok()?))
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
    // 只给了预算（anthropic 方言客户端 / `-thinking-N`）时折成档位：
    // OpenAI 没有预算参数，但有档位，硬丢掉等于收了钱不干活
    let effort = directive.effective_effort();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> RequestReasoning {
        parse_request(&Bytes::from(json.to_owned()))
    }

    #[test]
    fn silence_is_not_an_opinion() {
        assert_eq!(parse(r#"{"model":"m"}"#), RequestReasoning::Unspecified);
        assert_eq!(parse("not json"), RequestReasoning::Unspecified);
        // 翻译不了的档位不装懂：同方言下它本来就会原样透传到上游
        assert_eq!(
            parse(r#"{"reasoning_effort":"xhigh"}"#),
            RequestReasoning::Unspecified
        );
    }

    #[test]
    fn all_three_dialects_normalize_to_one_shape() {
        let high = RequestReasoning::Enabled(ReasoningDirective {
            effort: Some(Effort::High),
            budget_tokens: None,
        });
        assert_eq!(parse(r#"{"reasoning_effort":"high"}"#), high, "OpenAI chat");
        assert_eq!(
            parse(r#"{"reasoning":{"effort":"high"}}"#),
            high,
            "统一形状"
        );
        assert_eq!(
            parse(r#"{"thinking":{"type":"enabled","budget_tokens":4096}}"#),
            RequestReasoning::Enabled(ReasoningDirective {
                effort: None,
                budget_tokens: Some(4096),
            }),
            "Anthropic"
        );
        assert_eq!(
            parse(r#"{"reasoning":{"max_tokens":2048}}"#),
            RequestReasoning::Enabled(ReasoningDirective {
                effort: None,
                budget_tokens: Some(2048),
            })
        );
        // `{enabled:true}` 单独出现 = 开启、用缺省档
        assert_eq!(
            parse(r#"{"reasoning":{"enabled":true}}"#),
            RequestReasoning::Enabled(ReasoningDirective {
                effort: None,
                budget_tokens: None,
            })
        );
    }

    #[test]
    fn disabled_is_distinct_from_unspecified() {
        // 这两态要是并成一个，`model=x@effort:high` + `enabled:false` 就关不掉了
        for body in [
            r#"{"reasoning":{"enabled":false}}"#,
            r#"{"reasoning":{"effort":"none"}}"#,
            r#"{"reasoning_effort":"none"}"#,
            r#"{"thinking":{"type":"disabled"}}"#,
        ] {
            assert_eq!(parse(body), RequestReasoning::Disabled, "{body}");
        }
    }

    #[test]
    fn parameter_beats_the_name_suffix() {
        let from_name = Some(ReasoningDirective {
            effort: Some(Effort::Low),
            budget_tokens: None,
        });
        let param = ReasoningDirective {
            effort: Some(Effort::High),
            budget_tokens: None,
        };
        assert_eq!(
            RequestReasoning::Enabled(param).merge(from_name),
            Some(param),
            "显式参数压过模型名里带的默认值"
        );
        assert_eq!(
            RequestReasoning::Unspecified.merge(from_name),
            from_name,
            "没提 → 后缀接管"
        );
        assert_eq!(
            RequestReasoning::Disabled.merge(from_name),
            None,
            "明确关掉 → 压过后缀"
        );
    }

    #[test]
    fn budget_only_still_yields_an_effort_for_openai() {
        // 此前这条路直接原样返回：用户要了思考、上游没收到、钱照收
        let d = |n: u32| ReasoningDirective {
            effort: None,
            budget_tokens: Some(n),
        };
        assert_eq!(d(1_024).effective_effort(), Effort::Minimal);
        assert_eq!(d(2_048).effective_effort(), Effort::Low);
        assert_eq!(d(8_000).effective_effort(), Effort::Medium);
        assert_eq!(d(32_000).effective_effort(), Effort::High);
        // 显式档位优先于逆映射
        assert_eq!(
            ReasoningDirective {
                effort: Some(Effort::Low),
                budget_tokens: Some(32_000),
            }
            .effective_effort(),
            Effort::Low
        );

        let body = Bytes::from(r#"{"model":"m"}"#.to_owned());
        let out = apply_openai(&body, d(2_048)).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["reasoning_effort"], "low");
    }

    #[test]
    fn strip_keeps_what_it_did_not_understand() {
        // 没实现的 exclude / 没看懂的 xhigh 必须原样送出去——
        // 上游（另一个 OpenRouter 兼容网关）很可能认得
        let body =
            Bytes::from(r#"{"model":"m","reasoning":{"effort":"high","exclude":true}}"#.to_owned());
        let v: Value = serde_json::from_slice(&strip_unified(&body).unwrap()).unwrap();
        assert_eq!(v["reasoning"]["exclude"], true, "没消化的键要留下");
        assert!(v["reasoning"].get("effort").is_none(), "消化过的键要摘走");

        // 一个都没消化 → 零拷贝、原样透传
        assert!(
            strip_unified(&Bytes::from(
                r#"{"reasoning":{"effort":"xhigh"}}"#.to_owned()
            ))
            .is_none()
        );
        assert!(
            strip_unified(&Bytes::from(r#"{"reasoning":{"exclude":true}}"#.to_owned())).is_none()
        );
    }

    #[test]
    fn strip_removes_only_the_unified_object() {
        let body = Bytes::from(
            r#"{"model":"m","reasoning":{"effort":"high"},"reasoning_effort":"high"}"#.to_owned(),
        );
        let out = strip_unified(&body).expect("有统一对象应返回新体");
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert!(v.get("reasoning").is_none(), "上游不认识它，会 400");
        assert_eq!(v["reasoning_effort"], "high", "原生字段是上游自己的，不动");
        // 只有原生字段时零拷贝
        assert!(strip_unified(&Bytes::from(r#"{"reasoning_effort":"high"}"#.to_owned())).is_none());
        assert!(strip_unified(&Bytes::from(r#"{"model":"m"}"#.to_owned())).is_none());
    }
}
