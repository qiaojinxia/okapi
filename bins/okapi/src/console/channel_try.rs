//! 渠道账号测试输出（POST /admin/channels/{id}/try）：钉住这条渠道（可选某个账号）发一次
//! 真实的流式补全，把上游输出原样流给控制台。
//!
//! 与「测试」按钮（`probe_channel`）的分工：探测只验凭证 / 发 16 token，不走网关链路，订阅账号
//! 测不了；这里在进程内直接调用数据面处理器（同 Playground 中继），OAuth 刷新、客户端模拟、
//! 出口代理、协议转换都与真实请求一致。按管理员自己的令牌计费、记日志——零收入的测试记录
//! 会把渠道毛利拉成负数，触发负毛利熔断误伤同组用户，所以不做免费。

use super::admin::{audit, ensure_channel_owner, guard_scoped};
use crate::gateway::channel_test::ChannelPin;
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use okapi_api::{codes, permissions};
use serde::Deserialize;
use serde_json::{Value, json};

/// 测试输出上限：够看出模型在正常说话，又不至于一次测试花掉可观的钱。
const MAX_OUTPUT_TOKENS: u32 = 512;
const MAX_PROMPT_CHARS: usize = 4000;

#[derive(Debug, Deserialize)]
pub struct TryReq {
    pub model: String,
    #[serde(default)]
    pub prompt: Option<String>,
    /// 只测这个账号（channel_keys.id）；缺省按调度顺序取渠道内可用账号。
    #[serde(default)]
    pub key_id: Option<i64>,
}

pub async fn try_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
    ExtractJson(req): ExtractJson<TryReq>,
) -> Response {
    match prepare(&state, &headers, channel_id, req).await {
        Ok((pin, body, responses)) => {
            let mut headers = headers;
            // 试用台的选 key 头不适用：测试一律用登录的这把令牌
            headers.remove(super::playground::SELECTED_KEY_HEADER);
            let trace = crate::gateway::diagnostics::Trace::new(&headers);
            let call = async move {
                if responses {
                    crate::gateway::chat::responses(State(state), headers, body).await
                } else {
                    crate::gateway::chat::chat_completions(State(state), headers, body).await
                }
            };
            trace
                .scope(crate::gateway::channel_test::scope(pin, Box::pin(call)))
                .await
        }
        Err(error) => error.into_response_with(None),
    }
}

async fn prepare(
    state: &AppState,
    headers: &HeaderMap,
    channel_id: i64,
    req: TryReq,
) -> Result<(ChannelPin, Bytes, bool), AppError> {
    let (actor, scope) = guard_scoped(state, headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(state, channel_id, &actor, scope).await?;
    let model = req.model.trim();
    let prompt = req.prompt.as_deref().map(str::trim).unwrap_or_default();
    let prompt = if prompt.is_empty() { "Hi" } else { prompt };
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(AppError::bad_request().with_param("prompt"));
    }
    let row: Option<(String, Value, Vec<String>, bool)> = sqlx::query_as(
        r"SELECT c.provider, c.models,
                 COALESCE(array_agg(pc.pool_code::text) FILTER (WHERE pc.pool_code IS NOT NULL), '{}'),
                 $2::bigint IS NULL OR EXISTS (
                     SELECT 1 FROM channel_keys ck WHERE ck.channel_id = c.id AND ck.id = $2)
          FROM channels c
          LEFT JOIN pool_channels pc ON pc.channel_id = c.id
          WHERE c.id = $1 AND c.deleted_at IS NULL
          GROUP BY c.id",
    )
    .bind(channel_id)
    .bind(req.key_id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let Some((provider, models, pools, key_ok)) = row else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    if !key_ok {
        return Err(AppError::bad_request().with_param("key_id"));
    }
    if !models
        .as_array()
        .is_some_and(|list| list.iter().any(|m| m.as_str() == Some(model)))
    {
        return Err(AppError::bad_request().with_param("model"));
    }
    // 不在任何池的渠道对谁都不可达，测试同样打不到；直接说明白，别回「无可用渠道」
    if pools.is_empty() {
        return Err(AppError::bad_request().with_param("channel_orphan"));
    }
    // Codex 订阅只服务 Responses 入口；其余协议走 chat，由网关转换
    let responses = provider == "codex";
    let body = if responses {
        json!({"model": model, "stream": true, "store": false,
               "max_output_tokens": MAX_OUTPUT_TOKENS,
               "input": [{"role": "user", "content": prompt}]})
    } else {
        json!({"model": model, "stream": true, "max_tokens": MAX_OUTPUT_TOKENS,
               "stream_options": {"include_usage": true},
               "messages": [{"role": "user", "content": prompt}]})
    };
    audit(
        state,
        &actor,
        "channel.try",
        &channel_id.to_string(),
        json!({"model": model, "key_id": req.key_id}),
    )
    .await;
    let body = serde_json::to_vec(&body).map_err(|_| AppError::bad_request())?;
    Ok((
        ChannelPin {
            channel_id,
            key_id: req.key_id,
            pools,
        },
        Bytes::from(body),
        responses,
    ))
}
