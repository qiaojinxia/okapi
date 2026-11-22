//! Responses 历史是上游账号资源；与可降级的 L2 cache 亲和分开。
use super::SchedulerRedis;
use crate::error::AppError;
use axum::http::StatusCode;
use fred::interfaces::KeysInterface;
use okapi_api::codes;
use okapi_store::{CachedScript, ChannelCandidate};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

const TTL_SECONDS: i64 = 30 * 24 * 3600;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
static SET_BINDING_SCRIPT: CachedScript = CachedScript::new(
    "local old=redis.call('GET',KEYS[1]); if old then if old==ARGV[1] then return 1 else return 0 end end; redis.call('SET',KEYS[1],ARGV[1],'EX',ARGV[2]); return 1",
);

/// 不存凭证明文。OAuth 刷新保持 account_id 不变时仍指向同一历史空间。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseBinding {
    pub channel_id: i64,
    pub channel_key_id: i64,
    pub identity: String,
}

impl ResponseBinding {
    #[must_use]
    pub fn from_candidate(candidate: &ChannelCandidate) -> Self {
        Self {
            channel_id: candidate.channel_id,
            channel_key_id: candidate.channel_key_id,
            identity: hex::encode(upstream_digest(candidate)),
        }
    }

    /// 同一份上游身份摘要的前 8 字节（16 位十六进制），给回传客户端的不透明令牌打账号标签。
    #[must_use]
    pub fn account_tag(candidate: &ChannelCandidate) -> String {
        hex::encode(&upstream_digest(candidate)[..8])
    }

    #[must_use]
    pub fn matches(&self, candidate: &ChannelCandidate) -> bool {
        candidate.responses_native && *self == Self::from_candidate(candidate)
    }
}

fn upstream_digest(candidate: &ChannelCandidate) -> [u8; 32] {
    let account = if candidate.provider == "codex" {
        okapi_store::credential::OAuthCredential::parse(&candidate.credential)
            .and_then(|c| c.account_id)
            .filter(|id| !id.is_empty())
    } else {
        None
    };
    // base / provider / org-project 头也属于上游历史空间，配置变更不能沿用旧映射。
    let mut headers = candidate.extra_headers.clone();
    headers.sort();
    let namespace = serde_json::json!([
        candidate.provider,
        candidate.api_base,
        account.as_deref().unwrap_or(&candidate.credential),
        headers
    ]);
    Sha256::digest(namespace.to_string().as_bytes()).into()
}

#[derive(Clone)]
pub struct ResponseParent {
    pub id: String,
    pub binding: ResponseBinding,
}

/// 不把未知、过期、他人或其他 API key 的 ID 命中情况透露给请求方。
pub async fn resolve_parent(
    sched: &SchedulerRedis,
    user: i64,
    key: i64,
    body: &[u8],
) -> Result<Option<ResponseParent>, AppError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    resolve_parent_value(sched, user, key, &value).await
}

/// 同 [`resolve_parent`]，复用调用方已解析好的请求体 `Value`，免去一次全量 JSON 解码。
pub async fn resolve_parent_value(
    sched: &SchedulerRedis,
    user: i64,
    key: i64,
    value: &Value,
) -> Result<Option<ResponseParent>, AppError> {
    let Some(previous) = value.get("previous_response_id").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let id = previous
        .as_str()
        .filter(|s| valid_id(s))
        .ok_or_else(|| AppError::bad_request().with_param("previous_response_id"))?;
    if value.get("conversation").is_some_and(|v| !v.is_null()) {
        return Err(AppError::bad_request().with_param("previous_response_id"));
    }
    let binding = sched
        .response_binding_get(user, key, id)
        .await?
        .ok_or_else(|| {
            AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
                .with_param("previous_response_id")
        })?;
    Ok(Some(ResponseParent {
        id: id.to_owned(),
        binding,
    }))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 512
        && !id.chars().any(char::is_whitespace)
        && !id.chars().any(char::is_control)
}

pub fn unavailable() -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL)
        .with_param("previous_response_id")
}

fn storage_error() -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::INTERNAL_ERROR)
        .with_param("response_binding")
}

impl SchedulerRedis {
    fn response_key(user: i64, key: i64, response: &str) -> String {
        let digest = hex::encode(Sha256::digest(response.as_bytes()));
        format!("stick:resp:{{{user}}}:v2:{key}:{digest}")
    }

    pub async fn response_binding_get(
        &self,
        user: i64,
        key: i64,
        response: &str,
    ) -> Result<Option<ResponseBinding>, AppError> {
        let result = tokio::time::timeout(
            IO_TIMEOUT,
            self.client
                .get::<Option<String>, _>(Self::response_key(user, key, response)),
        )
        .await
        .map_err(|_| storage_error())?
        .map_err(|_| storage_error())?;
        result
            .map(|raw| serde_json::from_str(&raw).map_err(|_| storage_error()))
            .transpose()
    }

    /// 原子且不可改绑；同一 ID 被另一上游重复使用时拒绝覆盖。
    pub async fn response_binding_set(
        &self,
        user: i64,
        key: i64,
        response: &str,
        binding: &ResponseBinding,
    ) -> Result<(), AppError> {
        if !valid_id(response) {
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                    .with_param("response_id"),
            );
        }
        let raw = serde_json::to_string(binding).map_err(|_| storage_error())?;
        let result = tokio::time::timeout(
            IO_TIMEOUT,
            SET_BINDING_SCRIPT.eval::<i64, _, _>(
                &self.client,
                vec![Self::response_key(user, key, response)],
                vec![raw, TTL_SECONDS.to_string()],
            ),
        )
        .await
        .map_err(|_| storage_error())?
        .map_err(|_| storage_error())?;
        if result == 1 {
            Ok(())
        } else {
            Err(
                AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                    .with_param("response_id_conflict"),
            )
        }
    }
}

/// 在把原生响应 ID 暴露给客户端之前建立映射，SSE 只写一次。
pub struct ResponseWriter {
    binding: ResponseBinding,
    seen: Option<String>,
}

impl ResponseWriter {
    pub fn new(binding: ResponseBinding) -> Self {
        Self {
            binding,
            seen: None,
        }
    }

    pub async fn capture(
        &mut self,
        sched: &SchedulerRedis,
        user: i64,
        key: i64,
        raw: &[u8],
    ) -> Result<(), AppError> {
        let Some(id) = extract_response_id(raw) else {
            return Ok(());
        };
        if let Some(seen) = &self.seen {
            return if seen == id.as_ref() {
                Ok(())
            } else {
                Err(
                    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                        .with_param("response_id_changed"),
                )
            };
        }
        sched
            .response_binding_set(user, key, id.as_ref(), &self.binding)
            .await?;
        self.seen = Some(id.into_owned());
        Ok(())
    }
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[derive(Default)]
struct ResponseFields<'a> {
    id: Option<std::borrow::Cow<'a, str>>,
    object: Option<std::borrow::Cow<'a, str>>,
}

#[derive(Default)]
enum ResponseSlot<'a> {
    #[default]
    Absent,
    Object(ResponseFields<'a>),
    NonObject,
}

struct OptStrSeed;
impl<'de> serde::de::DeserializeSeed<'de> for OptStrSeed {
    type Value = Option<std::borrow::Cow<'de, str>>;
    fn deserialize<D: serde::de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        crate::json_probe::opt_str(deserializer)
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for ResponseSlot<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
        use std::borrow::Cow;

        struct SlotVisitor;
        impl<'de> Visitor<'de> for SlotVisitor {
            type Value = ResponseSlot<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = ResponseFields::default();
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "id" => fields.id = map.next_value_seed(OptStrSeed)?,
                        "object" => fields.object = map.next_value_seed(OptStrSeed)?,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(ResponseSlot::Object(fields))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ResponseSlot::NonObject)
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ResponseSlot::NonObject)
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ResponseSlot::NonObject)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ResponseSlot::NonObject)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ResponseSlot::NonObject)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ResponseSlot::NonObject)
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ResponseSlot::NonObject)
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ResponseSlot::NonObject)
            }
        }
        deserializer.deserialize_any(SlotVisitor)
    }
}

#[derive(Deserialize)]
struct ResponseIdProbe<'a> {
    #[serde(default, borrow, deserialize_with = "crate::json_probe::opt_str")]
    id: Option<std::borrow::Cow<'a, str>>,
    #[serde(default, borrow, deserialize_with = "crate::json_probe::opt_str")]
    object: Option<std::borrow::Cow<'a, str>>,
    #[serde(default, borrow)]
    response: ResponseSlot<'a>,
}

/// 零 DOM 提取 Responses 事件（或非流式响应）里的 `response.id`（或顶层 `id`）。
/// 普通增量帧只含 `item_id` 不含 `"id"`，字面量预检即可直接返回 `None` 避免解析 JSON。
fn extract_response_id(raw: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    if !contains_subslice(raw, b"\"id\"") && !contains_subslice(raw, b"\\u") {
        return None;
    }
    let probe = serde_json::from_slice::<ResponseIdProbe>(raw).ok()?;
    let (id, object) = match probe.response {
        ResponseSlot::Object(fields) => (fields.id, fields.object),
        ResponseSlot::Absent => (probe.id, probe.object),
        ResponseSlot::NonObject => return None,
    };
    if object.as_deref().is_some_and(|o| o != "response") {
        return None;
    }
    id
}

#[cfg(test)]
mod extract_id_tests {
    use super::extract_response_id;

    #[test]
    fn extracts_response_id_across_streaming_and_edge_shapes() {
        assert_eq!(
            extract_response_id(
                br#"{"type":"response.output_text.delta","item_id":"msg_1","delta":"hi"}"#
            ),
            None
        );
        assert_eq!(
            extract_response_id(
                br#"{"type":"response.created","response":{"id":"resp_1","object":"response"}}"#
            )
            .as_deref(),
            Some("resp_1")
        );
        assert_eq!(
            extract_response_id(br#"{"id":"resp_2","object":"response"}"#).as_deref(),
            Some("resp_2")
        );
        // 显式 response: null 不能退回到顶层 id
        assert_eq!(
            extract_response_id(br#"{"id":"resp_top","response":null}"#),
            None
        );
        // 非 response object 忽略
        assert_eq!(
            extract_response_id(br#"{"id":"msg_1","object":"chat.completion"}"#),
            None
        );
        // \uXXXX 逃逸键名保持等价
        assert_eq!(
            extract_response_id(br#"{"\u0069d":"resp_3","object":"response"}"#).as_deref(),
            Some("resp_3")
        );
    }
}
