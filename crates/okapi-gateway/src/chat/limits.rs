//! 预扣上限：请求级单价上限、补全上限的准入值，以及写进上游请求的缺省输出上限。
use super::super::ingress::Ingress;
use super::{DEFAULT_COMPLETION_CAP, MAX_COMPLETION_CAP};
use bytes::Bytes;

/// 报价单价是否越过请求声明的上限；越过则返回 "轴:实际单价" 供 param 回显。
///
/// 快照里的 `final_unit_price_input_per_1m_usd` 是**输入**侧最终单价；输出侧单价 =
/// 它 × completion_ratio（补全倍率就是"输出比输入贵多少倍"的定义）。全程定点整数：
/// micro-USD × 1e6 倍率与上限 × 1e6 在 i128 里比，不经浮点、不舍入。
pub(super) fn price_above_max(
    quote: &okapi_pricing::Quote,
    prefs: &super::super::routing_prefs::RoutingPrefs,
) -> Option<String> {
    use okapi_pricing::ratio::RATIO_SCALE;
    if prefs.max_price.prompt.is_none() && prefs.max_price.completion.is_none() {
        return None;
    }
    // 按次计价没有 token 单价可比。当作没超会让调用方的上限静默失效（一次按次价可能远高于
    // 按单价估的花费），所以给了上限就拒，param 带上这次的按次实付价，便于调用方判断。
    let Some(input) = quote
        .snapshot
        .final_unit_price_input_per_1m_usd
        .map(okapi_domain::Money::as_micros)
    else {
        return Some(format!("per_call:{}", quote.amount.to_usd_string()));
    };
    if let Some(cap) = prefs.max_price.prompt
        && input > cap
    {
        return Some(format!(
            "prompt:{}",
            okapi_domain::Money::from_micros(input).to_usd_string()
        ));
    }
    if let Some(cap) = prefs.max_price.completion {
        let ratio = quote
            .snapshot
            .completion_ratio
            .map_or(RATIO_SCALE, okapi_pricing::ratio::RatioFp::as_scaled);
        let output_scaled = i128::from(input) * i128::from(ratio);
        if output_scaled > i128::from(cap) * i128::from(RATIO_SCALE) {
            let output = i64::try_from(output_scaled / i128::from(RATIO_SCALE)).unwrap_or(i64::MAX);
            return Some(format!(
                "completion:{}",
                okapi_domain::Money::from_micros(output).to_usd_string()
            ));
        }
    }
    None
}

/// 预扣用的单条补全上界。显式上限照单全收（不超过模型 max_output）：上游按它生成、
/// 按实际计费，把它截到 `MAX_COMPLETION_CAP` 只会让预扣不再是扣费上界、余额可被透支。
/// 未声明时才取模型缺省并封顶，免得为没要求长输出的请求冻结大额余额。
/// compact 端点不接受输出上限参数、cap 根本写不进请求，缺省只能按模型真实 max_output
/// 全额预扣——否则预扣同样不是上界（封顶值到不了上游）。
pub(super) fn admitted_completion_cap(
    ingress: Ingress,
    requested: Option<u32>,
    max_output: Option<u32>,
) -> u32 {
    match requested {
        Some(requested) => max_output.map_or(requested, |max| requested.min(max)),
        None if ingress == Ingress::ResponsesCompact => max_output.unwrap_or(MAX_COMPLETION_CAP),
        None => max_output
            .unwrap_or(DEFAULT_COMPLETION_CAP)
            .min(MAX_COMPLETION_CAP),
    }
}

/// 未声明输出上限时，预扣只按封顶后的上限估；模型能输出得更多，就得把这个上限
/// 写进请求，否则上游照 max_output 生成、结算多退少补，余额被透支（2026-10-05 定案：
/// 补上限而不是全额预扣，宁可极少数超长输出截断，也不为普通请求冻结大额余额）。
/// max_output 不超过封顶的模型本身生成不到更多，不改请求。
pub(super) fn default_output_cap(requested: Option<u32>, max_output: Option<u32>) -> Option<u32> {
    (requested.is_none() && max_output.is_some_and(|max| max > MAX_COMPLETION_CAP))
        .then_some(MAX_COMPLETION_CAP)
}

/// Gemini 请求里生成配置对象的两种键名（写入时首选 camelCase）。
pub(super) const GEMINI_CONFIG_KEYS: [&str; 2] = ["generationConfig", "generation_config"];

/// 按入口方言写入缺省输出上限（转换路径各自翻译成上游字段）。已有上限的不动；
/// compact 端点不接受输出上限参数，跳过。作用在入口方言的原文上，必须先于出向转换
/// （见 `build_upstream_body`）：转换之后的体已经是上游方言，再按入口方言写字段名就错位了。
pub(super) fn bound_default_output(ingress: Ingress, cap: Option<u32>, body: Bytes) -> Bytes {
    let Some(cap) = cap else {
        return body;
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(obj) = value.as_object_mut() else {
        return body;
    };
    // `"max_tokens": null` 在 admission 侧与缺席同形（serde null → None，预扣照封顶档
    // 冻结），但"键存在即已声明"会让上限写不进请求——上游照模型 max_output 生成、
    // 结算多退少补没有下限，余额被透支。null 一律视同缺席，先剥掉再判定。
    for key in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
        if obj.get(key).is_some_and(serde_json::Value::is_null) {
            obj.remove(key);
        }
    }
    // Gemini 的配置对象有两种拼写（探针 `GeminiRequestProbe` 认 `generationConfig` 与 `generation_config`），
    // 里面的上限键也是 camel / snake 两种。对象本身是 null、或里面的上限是 null，都与缺席同形，先剥掉：
    // 否则下面 Gemini 臂在 `generationConfig: null` 上取不到对象就原样放行，上限写不进请求（同样透支）。
    for config_key in GEMINI_CONFIG_KEYS {
        if obj.get(config_key).is_some_and(serde_json::Value::is_null) {
            obj.remove(config_key);
        }
        if let Some(config) = obj
            .get_mut(config_key)
            .and_then(serde_json::Value::as_object_mut)
        {
            for name in ["maxOutputTokens", "max_output_tokens"] {
                if config.get(name).is_some_and(serde_json::Value::is_null) {
                    config.remove(name);
                }
            }
        }
    }
    let inserted = match ingress {
        Ingress::OpenAi => {
            // max_tokens 兼容面最广；推理模型由 model_parameters 改写成 max_completion_tokens
            if obj.contains_key("max_tokens") || obj.contains_key("max_completion_tokens") {
                return body;
            }
            obj.insert("max_tokens".into(), cap.into());
            true
        }
        Ingress::Anthropic => obj.insert("max_tokens".into(), cap.into()).is_none(),
        Ingress::Responses => obj.insert("max_output_tokens".into(), cap.into()).is_none(),
        Ingress::Gemini => {
            // 客户端用哪种拼写就写进哪个对象，别再并排造出第二个配置对象
            let config_key = GEMINI_CONFIG_KEYS
                .into_iter()
                .find(|key| obj.contains_key(*key))
                .unwrap_or(GEMINI_CONFIG_KEYS[0]);
            let Some(config) = obj
                .entry(config_key)
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
            else {
                return body;
            };
            config
                .insert("maxOutputTokens".into(), cap.into())
                .is_none()
        }
        Ingress::ResponsesCompact => false,
    };
    if !inserted {
        return body;
    }
    serde_json::to_vec(&value).map_or(body, Bytes::from)
}
