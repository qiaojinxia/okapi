//! Gemini usageMetadata: totals include cached input and thinking output.
use okapi_api::{CompletionTokensDetails, ModalTokensDetails, PromptTokensDetails, UsageProbe};
use okapi_domain::ModalitiesReported;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    prompt_token_count: Option<u32>,
    /// 内置工具（Google Search Grounding / Code Execution / URL Context）注入的输入 token，
    /// 与 `promptTokenCount` 同属计费输入且计入 `totalTokenCount`。
    tool_use_prompt_token_count: Option<u32>,
    #[serde(default)]
    candidates_token_count: Option<u32>,
    thoughts_token_count: Option<u32>,
    cached_content_token_count: Option<u32>,
    total_token_count: Option<u64>,
    prompt_tokens_details: Option<Vec<Modality>>,
    cache_tokens_details: Option<Vec<Modality>>,
    candidates_tokens_details: Option<Vec<Modality>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Modality {
    modality: String,
    token_count: u32,
}

fn modalities(
    rows: Option<&[Modality]>,
    total: u32,
    complete: bool,
) -> Option<(u32, u32, ModalitiesReported)> {
    let mut audio = 0;
    let mut image = 0;
    let mut counted = 0_u64;
    let mut seen = HashSet::new();
    for row in rows.into_iter().flatten() {
        if !seen.insert(row.modality.as_str()) {
            return None;
        }
        counted += u64::from(row.token_count);
        match row.modality.as_str() {
            "AUDIO" => audio = row.token_count,
            "IMAGE" => image = row.token_count,
            // Text and other modalities retain the provider's base input/output price.
            _ => {}
        }
    }
    if counted > u64::from(total) || (complete && counted != u64::from(total)) {
        return None;
    }
    let covered = rows.is_some() && counted == u64::from(total);
    Some((
        audio,
        image,
        ModalitiesReported {
            audio: seen.contains("AUDIO") || covered,
            image: seen.contains("IMAGE") || covered,
        },
    ))
}

fn normalize(meta: &Metadata) -> Option<UsageProbe> {
    let prompt = match (meta.prompt_token_count, meta.tool_use_prompt_token_count) {
        (Some(p), Some(t)) => Some(p.checked_add(t)?),
        (Some(p), None) => Some(p),
        (None, Some(t)) => Some(t),
        (None, None) => None,
    };
    let thoughts = meta.thoughts_token_count.unwrap_or(0);
    let completion = match (meta.candidates_token_count, meta.thoughts_token_count) {
        (Some(c), _) => Some(c.checked_add(thoughts)?),
        // 思考模型将 maxOutputTokens 全部耗尽于 thoughts 时可能省略 candidatesTokenCount；
        // 若 totalTokenCount 显示还包含额外正文输出，交由 UsageProbe 按 total - prompt 推算，否则直接记为 thoughts。
        (None, Some(t)) => {
            let inferred_larger = meta
                .total_token_count
                .zip(prompt)
                .is_some_and(|(total, p)| total > u64::from(p) + u64::from(t));
            if inferred_larger { None } else { Some(t) }
        }
        (None, None) => None,
    };
    let mut probe: UsageProbe = serde_json::from_value(serde_json::json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": meta.total_token_count,
    }))
    .ok()?;
    let (audio, image, input_reported) = modalities(
        meta.prompt_tokens_details.as_deref(),
        if probe.missing_prompt {
            u32::MAX
        } else {
            probe.prompt_tokens
        },
        false,
    )?;
    let (audio_out, image_out, output_reported) = modalities(
        meta.candidates_tokens_details.as_deref(),
        if probe.missing_completion {
            u32::MAX
        } else {
            probe.completion_tokens.checked_sub(thoughts)?
        },
        false,
    )?;
    let cached_details = if let Some(rows) = meta.cache_tokens_details.as_deref() {
        let total = meta.cached_content_token_count?;
        let (audio, image, _) = modalities(Some(rows), total, true)?;
        Some(ModalTokensDetails {
            text_tokens: Some(total.checked_sub(audio)?.checked_sub(image)?),
            audio_tokens: Some(audio),
            image_tokens: Some(image),
        })
    } else {
        None
    };
    probe.prompt_tokens_details = PromptTokensDetails {
        cached_tokens: meta.cached_content_token_count.unwrap_or(0),
        cache_read_reported: meta.cached_content_token_count.is_some(),
        audio_tokens: audio,
        image_tokens: image,
        cached_tokens_details: cached_details,
        modalities_reported: input_reported,
        ..PromptTokensDetails::default()
    };
    probe.completion_tokens_details = CompletionTokensDetails {
        reasoning_tokens: thoughts,
        reasoning_reported: meta.thoughts_token_count.is_some(),
        modalities_reported: output_reported,
        audio_tokens: audio_out,
        image_tokens: image_out,
    };
    Some(probe)
}

pub(super) fn parse(meta: Option<&Value>) -> Option<UsageProbe> {
    let meta = meta.filter(|v| !v.is_null())?;
    let gemini_axes = [
        "promptTokenCount",
        "toolUsePromptTokenCount",
        "candidatesTokenCount",
        "totalTokenCount",
    ]
    .iter()
    .any(|key| meta.get(key).is_some_and(|v| !v.is_null()));
    if okapi_api::has_bridged_usage_fields(meta) && !gemini_axes {
        return Some(
            serde_json::from_value(meta.clone()).unwrap_or_else(|_| UsageProbe::invalid()),
        );
    }
    let probe = serde_json::from_value::<Metadata>(meta.clone())
        .ok()
        .as_ref()
        .and_then(normalize)
        .unwrap_or_else(UsageProbe::invalid);
    if !okapi_api::has_bridged_usage_fields(meta) {
        return Some(probe);
    }
    if [
        "inputTokens",
        "outputTokens",
        "totalTokens",
        "total_input_tokens",
        "total_output_tokens",
        "total_thought_tokens",
        "input_tokens_by_modality",
        "output_tokens_by_modality",
        "total_tool_use_tokens",
    ]
    .iter()
    .any(|key| meta.get(key).is_some_and(|v| !v.is_null()))
    {
        return Some(UsageProbe::invalid());
    }
    let mut canonical = probe.chat_json();
    if !canonical.is_object() {
        return Some(UsageProbe::invalid());
    }
    for key in [
        "cacheReadInputTokens",
        "cacheWriteInputTokens",
        "cacheDetails",
        "total_cached_tokens",
        "cached_tokens_by_modality",
    ] {
        if let Some(value) = meta.get(key) {
            canonical[key] = value.clone();
        }
    }
    Some(serde_json::from_value(canonical).unwrap_or_else(|_| UsageProbe::invalid()))
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::json;

    #[test]
    fn tool_use_prompt_tokens_are_counted_in_prompt_and_match_total() {
        let meta = json!({
            "promptTokenCount": 100,
            "toolUsePromptTokenCount": 500,
            "cachedContentTokenCount": 60,
            "candidatesTokenCount": 80,
            "thoughtsTokenCount": 20,
            "totalTokenCount": 700,
        });
        let usage = parse(Some(&meta)).unwrap().to_token_usage().unwrap();
        assert_eq!(usage.prompt_tokens, 600);
        assert_eq!(usage.cached_tokens, 60);
        assert_eq!(usage.prompt_uncached(), 540);
        assert_eq!(usage.completion_tokens, 100);
        assert_eq!(usage.reasoning_tokens, 20);
        assert!(usage.cache_read_reported);
    }

    #[test]
    fn pure_thinking_without_candidates_token_count_retains_thoughts_and_validates_total() {
        // 无 totalTokenCount 的纯思考帧：不再丢失 thoughtsTokenCount
        let without_total = json!({
            "promptTokenCount": 100,
            "thoughtsTokenCount": 500,
        });
        let u1 = parse(Some(&without_total))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(u1.prompt_tokens, 100);
        assert_eq!(u1.completion_tokens, 500);
        assert_eq!(u1.reasoning_tokens, 500);

        // 带一致 totalTokenCount 的纯思考终态
        let with_exact_total = json!({
            "promptTokenCount": 100,
            "thoughtsTokenCount": 500,
            "totalTokenCount": 600,
        });
        let u2 = parse(Some(&with_exact_total))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(u2.completion_tokens, 500);
        assert_eq!(u2.reasoning_tokens, 500);

        // totalTokenCount 高于 prompt + thoughts 时仍可按 total - prompt 还原完整补全量
        let with_extra_candidates = json!({
            "promptTokenCount": 100,
            "thoughtsTokenCount": 500,
            "totalTokenCount": 650,
        });
        let u3 = parse(Some(&with_extra_candidates))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(u3.completion_tokens, 550);
        assert_eq!(u3.reasoning_tokens, 500);

        // totalTokenCount 小于 prompt + thoughts 仍保持 fail-closed 判非法
        let broken = json!({
            "promptTokenCount": 100,
            "thoughtsTokenCount": 500,
            "totalTokenCount": 550,
        });
        assert!(parse(Some(&broken)).unwrap().to_token_usage().is_err());
    }
}
