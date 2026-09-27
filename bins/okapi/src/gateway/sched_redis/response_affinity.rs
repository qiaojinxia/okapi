//! Responses 历史是上游账号资源；与可降级的 L2 cache 亲和分开。
use super::SchedulerRedis;
use crate::gateway::error::AppError;
use axum::http::StatusCode;
use fred::interfaces::{KeysInterface, LuaInterface};
use okapi_api::codes;
use okapi_store::ChannelCandidate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

const TTL_SECONDS: i64 = 30 * 24 * 3600;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

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
        Self {
            channel_id: candidate.channel_id,
            channel_key_id: candidate.channel_key_id,
            identity: hex::encode(Sha256::digest(namespace.to_string().as_bytes())),
        }
    }

    #[must_use]
    pub fn matches(&self, candidate: &ChannelCandidate) -> bool {
        candidate.responses_native && *self == Self::from_candidate(candidate)
    }
}

#[derive(Clone)]
pub(crate) struct ResponseParent {
    pub id: String,
    pub binding: ResponseBinding,
}

/// 不把未知、过期、他人或其他 API key 的 ID 命中情况透露给请求方。
pub(crate) async fn resolve_parent(
    sched: &SchedulerRedis,
    user: i64,
    key: i64,
    body: &[u8],
) -> Result<Option<ResponseParent>, AppError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
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

pub(crate) fn unavailable() -> AppError {
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
        let result = tokio::time::timeout(IO_TIMEOUT, self.client.eval::<i64, _, _, _>(
            "local old=redis.call('GET',KEYS[1]); if old then if old==ARGV[1] then return 1 else return 0 end end; redis.call('SET',KEYS[1],ARGV[1],'EX',ARGV[2]); return 1",
            vec![Self::response_key(user, key, response)], vec![raw, TTL_SECONDS.to_string()],
        )).await.map_err(|_| storage_error())?.map_err(|_| storage_error())?;
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
pub(crate) struct ResponseWriter {
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
        let Ok(value) = serde_json::from_slice::<Value>(raw) else {
            return Ok(());
        };
        let response = value.get("response").unwrap_or(&value);
        if response
            .get("object")
            .and_then(Value::as_str)
            .is_some_and(|object| object != "response")
        {
            return Ok(());
        }
        let Some(id) = response.get("id").and_then(Value::as_str) else {
            return Ok(());
        };
        if let Some(seen) = &self.seen {
            return if seen == id {
                Ok(())
            } else {
                Err(
                    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                        .with_param("response_id_changed"),
                )
            };
        }
        sched
            .response_binding_set(user, key, id, &self.binding)
            .await?;
        self.seen = Some(id.to_owned());
        Ok(())
    }
}
