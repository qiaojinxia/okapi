//! 入站请求的校验与特征：工具丢失、图片 / 附件 / PDF / 音频的 token 估算。
use super::super::ingress::Ingress;
use super::{ProbeInfo, RequestBilling};
use crate::kernel::error::AppError;
use okapi_providers::convert::tool_loss::ForeignTools;

/// 探针之外的入参口径：负 temperature 和 max_tokens 负值一样在入口拒掉，不原样透传给上游；
/// Chat / Messages 的 `messages`、Gemini 的 `contents` 为空没有可答的内容，也在入口拒掉，
/// 不再选渠道、打上游后才失败。拒绝时 param 是出错的参数名。
pub(in crate::gateway) fn check_input(
    ingress: Ingress,
    v: &serde_json::Value,
) -> Result<(), AppError> {
    let temperature = if ingress == Ingress::Gemini {
        v.get("generationConfig")
            .or_else(|| v.get("generation_config"))
            .and_then(|c| c.get("temperature"))
    } else {
        v.get("temperature")
    };
    // 与 max_tokens 的强类型拒绝对称：不是数（如字符串 "-1"）或是负数都拒，null 视为没给
    if temperature.is_some_and(|t| !t.is_null() && t.as_f64().is_none_or(|t| t < 0.0)) {
        return Err(AppError::bad_request().with_param("temperature"));
    }
    let (field, required) = match ingress {
        Ingress::OpenAi | Ingress::Anthropic => ("messages", true),
        Ingress::Gemini => ("contents", true),
        _ => ("", false),
    };
    if required
        && v.get(field)
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        return Err(AppError::bad_request().with_param(field));
    }
    Ok(())
}

/// 解析入口探针并过 [`check_input`]。
pub(super) fn parse_checked<T: serde::de::DeserializeOwned>(
    ingress: Ingress,
    body: &[u8],
) -> Result<T, AppError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let probe = T::deserialize(&value).map_err(|_| AppError::bad_request())?;
    check_input(ingress, &value)?;
    Ok(probe)
}

/// 转换真正会丢的工具：原生工具准入（`server_tools`）会在转换后把它认得的工具原样装回，不算丢。
pub(super) fn lost_tools(info: &ProbeInfo, bill: &RequestBilling) -> ForeignTools {
    info.foreign_tools
        .without(&bill.server_tools.carried_types())
}

/// 请求特征探测（§3.8 能力感知路由）：tools 数组非空 / 消息含图像部件。
/// 请求特征：要不要工具 / 视觉能力（§3.8），以及转成别家方言会丢哪些工具。
#[derive(Default)]
pub(in crate::gateway) struct RequestFeatures {
    pub tools: bool,
    pub vision: bool,
    pub foreign_tools: ForeignTools,
}

/// 入参已由调用方解析成 `Value`（与 [`check_input`] 共用这一份，大请求体不再反复全量解析）。
pub(in crate::gateway) fn request_features(
    ingress: Ingress,
    v: &serde_json::Value,
) -> RequestFeatures {
    let foreign_tools = okapi_providers::convert::tool_loss::foreign_tools(
        super::super::execution_plan::format(ingress),
        v,
    );
    let (tools, vision) = if ingress == Ingress::Gemini {
        gemini_request_features(v)
    } else {
        message_request_features(ingress, v)
    };
    RequestFeatures {
        tools,
        vision,
        foreign_tools,
    }
}

pub(super) fn message_request_features(ingress: Ingress, v: &serde_json::Value) -> (bool, bool) {
    let needs_tools = v
        .get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty());
    let image_types: &[&str] = match ingress {
        Ingress::OpenAi => &["image_url"],
        Ingress::Anthropic => &["image"],
        Ingress::Responses | Ingress::ResponsesCompact => &["input_image"],
        Ingress::Gemini => &[],
    };
    let containers = match ingress {
        Ingress::Responses | Ingress::ResponsesCompact => v.get("input"),
        _ => v.get("messages"),
    };
    let needs_vision = containers
        .and_then(|m| m.as_array())
        .is_some_and(|messages| {
            messages.iter().any(|msg| {
                msg.get("content")
                    .and_then(|c| c.as_array())
                    .is_some_and(|parts| {
                        parts.iter().any(|p| {
                            p.get("type")
                                .and_then(|t| t.as_str())
                                .is_some_and(|t| image_types.contains(&t))
                        })
                    })
            })
        });
    (needs_tools, needs_vision)
}

/// 预扣按张估图片输入：各家按尺寸计 token，本地不解码图片，取主流单张上界
/// （Claude 封顶约 1600、GPT-4o high 约 1100、按 patch 计的 GPT-4.1/5 系约 2500）。
/// 只用于预扣与缺用量时的兜底，实际仍按上游用量结算。
pub(super) const IMAGE_INPUT_TOKENS: u32 = 2_560;

/// 请求里的图片输入张数，含工具结果里嵌套的图片。
pub(super) fn image_inputs(ingress: Ingress, v: &serde_json::Value) -> u32 {
    input_roots(ingress, v)
        .into_iter()
        .flatten()
        .map(count_images)
        .fold(0, u32::saturating_add)
}

/// 携带输入内容的顶层字段（消息 / input / contents 与系统指令）。
pub(super) fn input_roots(
    ingress: Ingress,
    v: &serde_json::Value,
) -> [Option<&serde_json::Value>; 2] {
    match ingress {
        Ingress::OpenAi => [v.get("messages"), None],
        Ingress::Anthropic => [v.get("messages"), v.get("system")],
        Ingress::Responses | Ingress::ResponsesCompact => [v.get("input"), None],
        Ingress::Gemini => [
            v.get("contents"),
            v.get("systemInstruction")
                .or_else(|| v.get("system_instruction")),
        ],
    }
}

/// PDF 每页上界：Anthropic 文档按页计文本 + 页面图像，约 1500–3000 token。
pub(super) const PDF_PAGE_TOKENS: u32 = 3_000;
/// 单个 PDF 计入的页数上限：上游单文件最多处理约 1000 页（Gemini 的文档上限，Anthropic /
/// OpenAI 更少）。页对象计数只是启发式，增量保存会重复写页对象，构造的文件可以任意堆
/// `/Type /Page`；截到这里预扣仍是上界，也不会被虚增到无界。
pub(super) const PDF_MAX_PAGES: u32 = 1_000;
/// 音频每 token 至少对应的字节数：码率不低于 32 kbps（4000 B/s），各家最高约 32 token/s（Gemini）。
pub(super) const AUDIO_BYTES_PER_TOKEN: usize = 125;

/// 内联文件与音频输入的 token 上界（只看请求里带着的数据；URL / file_id 本地取不到，不计）。
/// 只用于预扣与缺用量时的兜底，实际仍按上游用量结算。
pub(super) fn attachment_tokens(ingress: Ingress, v: &serde_json::Value) -> u32 {
    input_roots(ingress, v)
        .into_iter()
        .flatten()
        .map(count_attachments)
        .fold(0, u32::saturating_add)
}

pub(super) fn count_attachments(value: &serde_json::Value) -> u32 {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(count_attachments)
            .fold(0, u32::saturating_add),
        serde_json::Value::Object(obj) => {
            let own = match obj.get("type").and_then(serde_json::Value::as_str) {
                // Anthropic document：base64 PDF 或纯文本
                Some("document") => match str_at(value, "/source/type") {
                    Some("base64") => str_at(value, "/source/data").map_or(0, |data| {
                        media_tokens(str_at(value, "/source/media_type"), data)
                    }),
                    Some("text") => str_at(value, "/source/data")
                        .map_or(0, |text| saturating_u32(text.len() / 2)),
                    _ => 0,
                },
                // OpenAI chat file / Responses input_file：data URL
                Some("file") => str_at(value, "/file/file_data").map_or(0, data_url_tokens),
                Some("input_file") => str_at(value, "/file_data").map_or(0, data_url_tokens),
                Some("input_audio") => str_at(value, "/input_audio/data")
                    .map_or(0, |data| media_tokens(Some("audio/"), data)),
                _ => ["inlineData", "inline_data"]
                    .iter()
                    .filter_map(|key| obj.get(*key))
                    .map(|media| {
                        let mime =
                            str_at(media, "/mimeType").or_else(|| str_at(media, "/mime_type"));
                        str_at(media, "/data").map_or(0, |data| media_tokens(mime, data))
                    })
                    .fold(0, u32::saturating_add),
            };
            obj.values()
                .map(count_attachments)
                .fold(own, u32::saturating_add)
        }
        _ => 0,
    }
}

pub(super) fn str_at<'a>(value: &'a serde_json::Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(serde_json::Value::as_str)
}

pub(super) fn data_url_tokens(url: &str) -> u32 {
    url.strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .map_or(0, |(mime, data)| media_tokens(Some(mime), data))
}

/// 按媒体类型估 base64 数据的 token 上界；图片另按张计，其它类型无从估计。
pub(super) fn media_tokens(mime: Option<&str>, base64_data: &str) -> u32 {
    use base64::Engine as _;
    let mime = mime.unwrap_or_default();
    if mime.starts_with("audio/") {
        return saturating_u32(base64_data.len() / 4 * 3 / AUDIO_BYTES_PER_TOKEN);
    }
    if mime != "application/pdf" {
        return 0;
    }
    let pages = base64::prelude::BASE64_STANDARD
        .decode(base64_data.trim())
        .map_or(1, |pdf| pdf_pages(&pdf));
    pages.max(1).saturating_mul(PDF_PAGE_TOKENS)
}

/// 页对象数：`/Type /Page`（不含 `/Pages` 目录节点），空格可有可无；数到 [`PDF_MAX_PAGES`] 为止。
pub(super) fn pdf_pages(pdf: &[u8]) -> u32 {
    let mut pages = 0_u32;
    for (index, _) in pdf.windows(5).enumerate().filter(|(_, w)| *w == b"/Type") {
        let rest = &pdf[index + 5..];
        let rest = &rest[rest.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
        if rest.starts_with(b"/Page") && !rest.starts_with(b"/Pages") {
            pages += 1;
            if pages == PDF_MAX_PAGES {
                break;
            }
        }
    }
    pages
}

pub(super) fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

pub(super) fn count_images(value: &serde_json::Value) -> u32 {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().map(count_images).fold(0, u32::saturating_add)
        }
        serde_json::Value::Object(obj) => {
            let typed = matches!(
                obj.get("type").and_then(serde_json::Value::as_str),
                Some("image_url" | "image" | "input_image")
            );
            let gemini = ["inlineData", "inline_data", "fileData", "file_data"]
                .iter()
                .filter_map(|key| obj.get(*key))
                .any(|media| {
                    media
                        .get("mimeType")
                        .or_else(|| media.get("mime_type"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|mime| mime.starts_with("image/"))
                });
            obj.values()
                .map(count_images)
                .fold(u32::from(typed || gemini), u32::saturating_add)
        }
        _ => 0,
    }
}

/// Gemini 支持函数和原生工具；tools 数组非空即需要工具能力。
/// contents 部件带 inlineData / fileData（图像 mime）即需要视觉。
pub(super) fn gemini_request_features(v: &serde_json::Value) -> (bool, bool) {
    let needs_tools = v
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let is_image = |media: &serde_json::Value| {
        media
            .get("mimeType")
            .or_else(|| media.get("mime_type"))
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.starts_with("image/"))
    };
    let needs_vision = v
        .get("contents")
        .and_then(|c| c.as_array())
        .is_some_and(|contents| {
            contents.iter().any(|c| {
                c.get("parts")
                    .and_then(|p| p.as_array())
                    .is_some_and(|parts| {
                        parts.iter().any(|p| {
                            p.get("inlineData")
                                .or_else(|| p.get("inline_data"))
                                .or_else(|| p.get("fileData"))
                                .or_else(|| p.get("file_data"))
                                .is_some_and(is_image)
                        })
                    })
            })
        });
    (needs_tools, needs_vision)
}

#[cfg(test)]
mod check_input_tests {
    use super::{Ingress, check_input};
    use serde_json::json;

    fn param(ingress: Ingress, body: &serde_json::Value) -> Option<String> {
        check_input(ingress, body).err().and_then(|e| e.param)
    }

    #[test]
    fn temperature_must_be_a_non_negative_number_on_every_http_ingress() {
        let messages = json!([{"role": "user", "content": "hi"}]);
        for ingress in [Ingress::OpenAi, Ingress::Anthropic, Ingress::Responses] {
            for (temperature, refused) in [
                (json!(-0.1), true),
                (json!("-1"), true),
                (json!("0.5"), true),
                (json!(0), false),
                (json!(1.2), false),
                (json!(null), false),
            ] {
                let body = json!({"model": "m", "messages": messages, "temperature": temperature});
                assert_eq!(
                    param(ingress, &body).as_deref(),
                    refused.then_some("temperature"),
                    "{ingress:?} {temperature}"
                );
            }
        }
        let gemini = json!({"contents": [{"parts": [{"text": "hi"}]}], "generationConfig": {"temperature": -1}});
        assert_eq!(
            param(Ingress::Gemini, &gemini).as_deref(),
            Some("temperature")
        );
        let snake = json!({"contents": [{"parts": [{"text": "hi"}]}], "generation_config": {"temperature": "x"}});
        assert_eq!(
            param(Ingress::Gemini, &snake).as_deref(),
            Some("temperature")
        );
    }

    #[test]
    fn empty_conversations_are_refused_where_the_dialect_requires_one() {
        for ingress in [Ingress::OpenAi, Ingress::Anthropic] {
            assert_eq!(
                param(ingress, &json!({"model": "m", "messages": []})).as_deref(),
                Some("messages")
            );
            assert_eq!(
                param(ingress, &json!({"model": "m"})).as_deref(),
                Some("messages")
            );
            assert!(
                check_input(
                    ingress,
                    &json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]})
                )
                .is_ok()
            );
        }
        assert_eq!(
            param(Ingress::Gemini, &json!({"contents": []})).as_deref(),
            Some("contents")
        );
        // Responses 的输入可以只靠 previous_response_id 续上，这里不要求
        assert!(check_input(Ingress::Responses, &json!({"model": "m"})).is_ok());
    }
}
