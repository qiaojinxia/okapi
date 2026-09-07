//! Amazon Bedrock 上游（IMPLEMENTATION §11.35）：Anthropic 方言 + AWS 传输。
//!
//! 请求走 InvokeModel / InvokeModelWithResponseStream，请求体就是 Anthropic Messages
//! （去 `model` / `stream`、加 `anthropic_version`），所以协议转换全部复用 anthropic 方向；
//! 这里只管三件事：SigV4 或 Bearer 鉴权、模型 ID 进 URL（`:` 须编码）、把 event-stream 帧
//! 还原成 `AnthropicEvent`。非 Anthropic 模型（Converse 形状）不在此模块范围。

use crate::anthropic::{AnthropicEvent, MessagesResponse, MessagesStream, classify};
use crate::aws_eventstream::Decoder;
use crate::aws_sigv4::{self, AwsCredentials, SignParams};
use crate::error::UpstreamError;
use base64::Engine as _;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::Value;
use std::time::Duration;

const NON_STREAM_TIMEOUT: Duration = Duration::from_mins(2);
/// InvokeModel 的 Anthropic 请求体版本字段（Bedrock 固定值）。
pub const ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const SIGV4_SERVICE: &str = "bedrock";

#[derive(Clone)]
pub struct BedrockUpstream {
    http: crate::http::HttpPool,
}

/// 从 `bedrock-runtime.{region}.amazonaws.com` 这类主机名解析区域；VPC 端点等解析不出返回 None。
#[must_use]
pub fn region_from_host(host: &str) -> Option<&str> {
    let mut labels = host.split('.');
    let first = labels.next()?;
    if !first.starts_with("bedrock") {
        return None;
    }
    let region = labels.next()?;
    let valid = region.split('-').count() >= 3
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    valid.then_some(region)
}

/// 把 Anthropic Messages 请求体改成 InvokeModel 形状：去 `model` / `stream`（都在 URL 上）、
/// 加 `anthropic_version`（调用方已给的保留）。
pub fn invoke_body(body: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    obj.remove("model");
    obj.remove("stream");
    obj.entry("anthropic_version")
        .or_insert_with(|| Value::String(ANTHROPIC_VERSION.to_owned()));
    serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))
}

impl BedrockUpstream {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            http: crate::http::HttpPool::new()?,
        })
    }

    /// Anthropic Messages → InvokeModel。`api_base` = `https://bedrock-runtime.{region}.amazonaws.com`；
    /// `region` 为 None 时从主机名解析。`model_id` 是上游模型 ID（映射后的值）。
    // 与 anthropic::messages 同形的参数表再加 region / model_id 两项；收成结构体只会让唯一调用方更绕
    #[allow(clippy::too_many_arguments)]
    pub async fn messages(
        &self,
        api_base: &str,
        region: Option<&str>,
        credential: &str,
        model_id: &str,
        body: Bytes,
        stream: bool,
        outbound: &crate::http::Outbound,
    ) -> Result<MessagesResponse, UpstreamError> {
        let action = if stream {
            "invoke-with-response-stream"
        } else {
            "invoke"
        };
        let url = format!(
            "{}/model/{}/{action}",
            api_base.trim_end_matches('/'),
            aws_sigv4::uri_encode(model_id)
        );
        let payload = invoke_body(&body)?;
        let resp = self
            .send_signed(&url, region, credential, payload, stream, outbound)
            .await?;
        let status = resp.status().as_u16();
        let upstream_request_id = resp
            .headers()
            .get("x-amzn-requestid")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if !(200..300).contains(&status) {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            let body = resp.bytes().await.unwrap_or_default();
            return Err(UpstreamError::Status {
                status,
                body,
                retry_after_secs,
            });
        }
        if !stream {
            let body = resp.bytes().await.map_err(|e| classify(&e))?;
            return Ok(MessagesResponse::Json {
                status,
                upstream_request_id,
                body,
            });
        }
        // event-stream 帧 → Anthropic 事件：一帧一个 chunk，chunk.bytes 是 base64 的事件 JSON
        let events = futures::stream::unfold(
            (resp.bytes_stream(), Decoder::new(), false),
            |(mut bytes, mut decoder, mut failed)| async move {
                if failed {
                    return None;
                }
                let chunk = bytes.next().await?;
                let out: Vec<Result<AnthropicEvent, UpstreamError>> = match chunk {
                    Err(e) => {
                        failed = true;
                        vec![Err(UpstreamError::Stream(e.to_string()))]
                    }
                    Ok(bytes) => match decoder.push(&bytes) {
                        Ok(frames) => frames.iter().filter_map(frame_to_event).collect(),
                        Err(e) => {
                            failed = true;
                            vec![Err(e)]
                        }
                    },
                };
                Some((futures::stream::iter(out), (bytes, decoder, failed)))
            },
        )
        .flatten();
        Ok(MessagesResponse::Stream(MessagesStream {
            upstream_request_id,
            events: Box::pin(events),
        }))
    }

    /// 控制面 `ListFoundationModels`（测活 / 拉取模型用；只在 SigV4 凭证下可用——API key 只覆盖数据面）。
    /// 返回 `modelSummaries[].modelId`。
    pub async fn list_foundation_models(
        &self,
        api_base: &str,
        region: Option<&str>,
        credential: &str,
        outbound: &crate::http::Outbound,
    ) -> Result<Vec<String>, UpstreamError> {
        let runtime =
            reqwest::Url::parse(api_base).map_err(|e| UpstreamError::Build(e.to_string()))?;
        let region = region
            .map(str::to_owned)
            .or_else(|| region_from_host(runtime.host_str().unwrap_or_default()).map(str::to_owned))
            .ok_or_else(|| UpstreamError::Build("bedrock_region_missing".to_owned()))?;
        // 控制面主机固定形态；VPC 端点用户也走公网控制面（列模型不是热路径）
        let url = format!("https://bedrock.{region}.amazonaws.com/foundation-models");
        let creds = AwsCredentials::parse(credential)
            .ok_or_else(|| UpstreamError::Build("bedrock_sigv4_required".to_owned()))?;
        let parsed = reqwest::Url::parse(&url).map_err(|e| UpstreamError::Build(e.to_string()))?;
        let hash = aws_sigv4::payload_hash(b"");
        let signed = aws_sigv4::sign(
            &creds,
            &SignParams {
                method: "GET",
                url: &parsed,
                region: &region,
                service: SIGV4_SERVICE,
                headers: &[("x-amz-content-sha256", &hash)],
                payload_hash: &hash,
                timestamp: chrono::Utc::now(),
            },
        );
        let mut req = self
            .http
            .get(outbound, url)?
            .header("x-amz-content-sha256", hash.as_str())
            .timeout(NON_STREAM_TIMEOUT);
        for (k, v) in &signed {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await.map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|e| classify(&e))?;
        if !(200..300).contains(&status) {
            return Err(UpstreamError::Status {
                status,
                body,
                retry_after_secs: None,
            });
        }
        let parsed: Value =
            serde_json::from_slice(&body).map_err(|e| UpstreamError::Stream(e.to_string()))?;
        Ok(parsed
            .get("modelSummaries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| m.get("modelId").and_then(Value::as_str).map(str::to_owned))
            .collect())
    }

    /// 数据面 OpenAI 兼容模型列表（Bearer 形态凭证的测活用：只验 key 认不认）。
    pub async fn list_openai_models(
        &self,
        api_base: &str,
        credential: &str,
        outbound: &crate::http::Outbound,
    ) -> Result<u16, UpstreamError> {
        let url = format!("{}/openai/v1/models", api_base.trim_end_matches('/'));
        let resp = self
            .http
            .get(outbound, url)?
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
            .timeout(NON_STREAM_TIMEOUT)
            .send()
            .await
            .map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            Ok(status)
        } else {
            Err(UpstreamError::Status {
                status,
                body: resp.bytes().await.unwrap_or_default(),
                retry_after_secs: None,
            })
        }
    }

    /// 按凭证形态签名并发送：SigV4（access key）或 Bearer（Bedrock API key）。
    async fn send_signed(
        &self,
        url: &str,
        region: Option<&str>,
        credential: &str,
        payload: Vec<u8>,
        stream: bool,
        outbound: &crate::http::Outbound,
    ) -> Result<reqwest::Response, UpstreamError> {
        let parsed = reqwest::Url::parse(url).map_err(|e| UpstreamError::Build(e.to_string()))?;
        let accept = if stream {
            "application/vnd.amazon.eventstream"
        } else {
            "application/json"
        };
        let mut req = self
            .http
            .post(outbound, url)?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, accept);
        if let Some(creds) = AwsCredentials::parse(credential) {
            let region = region
                .map(str::to_owned)
                .or_else(|| {
                    region_from_host(parsed.host_str().unwrap_or_default()).map(str::to_owned)
                })
                .ok_or_else(|| UpstreamError::Build("bedrock_region_missing".to_owned()))?;
            let hash = aws_sigv4::payload_hash(&payload);
            let signed = aws_sigv4::sign(
                &creds,
                &SignParams {
                    method: "POST",
                    url: &parsed,
                    region: &region,
                    service: SIGV4_SERVICE,
                    headers: &[
                        ("content-type", "application/json"),
                        ("accept", accept),
                        ("x-amz-content-sha256", &hash),
                    ],
                    payload_hash: &hash,
                    timestamp: chrono::Utc::now(),
                },
            );
            req = req.header("x-amz-content-sha256", hash.as_str());
            for (k, v) in &signed {
                req = req.header(k.as_str(), v.as_str());
            }
        } else {
            req = req.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", credential.trim()),
            );
        }
        if !stream {
            req = req.timeout(NON_STREAM_TIMEOUT);
        }
        req.body(payload).send().await.map_err(|e| classify(&e))
    }
}

/// 一帧 → 一个 Anthropic 事件；非 chunk 的事件帧（无载荷）跳过，异常帧转成流错误。
fn frame_to_event(
    frame: &crate::aws_eventstream::Frame,
) -> Option<Result<AnthropicEvent, UpstreamError>> {
    let message_type = frame.header(":message-type").unwrap_or("event");
    if message_type != "event" {
        let detail = serde_json::from_slice::<Value>(&frame.payload)
            .ok()
            .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        let kind = frame
            .header(":exception-type")
            .or_else(|| frame.header(":error-code"))
            .unwrap_or("bedrock_stream_error");
        return Some(Err(UpstreamError::Stream(format!("{kind}: {detail}"))));
    }
    if frame.header(":event-type") != Some("chunk") {
        return None;
    }
    let envelope: Value = serde_json::from_slice(&frame.payload).ok()?;
    let b64 = envelope.get("bytes").and_then(Value::as_str)?;
    let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let data = String::from_utf8(raw).ok()?;
    let event = serde_json::from_str::<Value>(&data)
        .ok()
        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    Some(Ok(AnthropicEvent { event, data }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws_eventstream::{Frame, encode_frame};

    #[test]
    fn region_parsing() {
        assert_eq!(
            region_from_host("bedrock-runtime.us-east-1.amazonaws.com"),
            Some("us-east-1")
        );
        assert_eq!(
            region_from_host("bedrock-runtime.ap-southeast-2.amazonaws.com"),
            Some("ap-southeast-2")
        );
        assert_eq!(region_from_host("bedrock-runtime.vpce-123.example"), None);
        assert_eq!(region_from_host("api.openai.com"), None);
    }

    #[test]
    fn invoke_body_drops_routing_fields_and_sets_version() {
        let out =
            invoke_body(br#"{"model":"x","stream":true,"max_tokens":5,"messages":[]}"#).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert!(v.get("model").is_none());
        assert!(v.get("stream").is_none());
        assert_eq!(v["anthropic_version"], ANTHROPIC_VERSION);
        assert_eq!(v["max_tokens"], 5);
        // 已带版本字段的保留
        let out = invoke_body(br#"{"anthropic_version":"custom","messages":[]}"#).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["anthropic_version"], "custom");
    }

    #[test]
    fn chunk_frame_decodes_to_anthropic_event() {
        let inner =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#;
        let b64 = base64::engine::general_purpose::STANDARD.encode(inner);
        let payload = format!(r#"{{"bytes":"{b64}"}}"#);
        let frames = Decoder::new()
            .push(&encode_frame(
                &[(":message-type", "event"), (":event-type", "chunk")],
                payload.as_bytes(),
            ))
            .unwrap();
        let ev = frame_to_event(&frames[0]).unwrap().unwrap();
        assert_eq!(ev.event, "content_block_delta");
        assert_eq!(ev.data, inner);
    }

    #[test]
    fn exception_frame_becomes_stream_error() {
        let frame = Frame {
            headers: vec![
                (":message-type".to_owned(), "exception".to_owned()),
                (
                    ":exception-type".to_owned(),
                    "throttlingException".to_owned(),
                ),
            ],
            payload: br#"{"message":"Too many requests"}"#.to_vec(),
        };
        match frame_to_event(&frame) {
            Some(Err(UpstreamError::Stream(msg))) => {
                assert_eq!(msg, "throttlingException: Too many requests");
            }
            other => panic!("unexpected {other:?}"),
        }
        // 无载荷的非 chunk 事件帧（如 initial-response）跳过
        let frame = Frame {
            headers: vec![
                (":message-type".to_owned(), "event".to_owned()),
                (":event-type".to_owned(), "initial-response".to_owned()),
            ],
            payload: Vec::new(),
        };
        assert!(frame_to_event(&frame).is_none());
    }
}
