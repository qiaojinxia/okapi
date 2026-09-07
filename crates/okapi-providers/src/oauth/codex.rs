//! ChatGPT 订阅经 Codex CLI 公开客户端的 OAuth（IMPLEMENTATION §11.38，实验性）。
//!
//! 后端只有 Responses 一种面（`https://chatgpt.com/backend-api/codex/responses`），请求头比官方
//! API 多两项：`chatgpt-account-id`（从 id_token 的 claim 取）与 `originator`。事件形状与官方
//! Responses 一致，传输直接复用 `responses::send_responses_at`。
//!
//! 该后端对请求体有一组硬要求（2026-09 对照 Sub2API 与 CLIProxyAPI 两家实现一致）：只有流式面、
//! `store` 必须 false、`instructions` 键必须存在、不接受 `role: system` 与一批官方 API 参数。
//! `prepare_body` 负责整形；客户端要非流式时由 `collect_json` 把 SSE 聚合回一个 Responses 对象。

use super::{Pkce, Tokens, form_encode, parse_tokens};
use crate::error::UpstreamError;
use crate::openai::{ChatResponse, StreamHandle, classify};
use crate::responses::usage_from_responses;
use crate::types::ChatEvent;
use base64::Engine as _;
use bytes::Bytes;
use futures::StreamExt as _;
use serde_json::{Value, json};
use std::time::Duration;

/// Codex CLI 公开客户端 id（OpenAI 私有，可能变更）。
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Codex CLI 的本地回调地址；我们不监听它，站长从浏览器地址栏把 `code` 贴回来。
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
pub const ORIGINATOR: &str = "codex_cli_rs";
pub const DEFAULT_API_BASE: &str = "https://chatgpt.com/backend-api/codex";
const TOKEN_TIMEOUT: Duration = Duration::from_secs(20);

/// 授权 URL：`state` 独立随机值（与 Anthropic 不同，这里不复用 verifier）。
#[must_use]
pub fn authorize_url(pkce: &Pkce, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?{}",
        form_encode(&[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", ORIGINATOR),
        ])
    )
}

/// 站长可能把整个回调 URL 贴回来：从中取 `code`（与 `state`）。
#[must_use]
pub fn split_pasted_code(pasted: &str) -> (String, Option<String>) {
    let pasted = pasted.trim();
    if let Ok(url) = reqwest::Url::parse(pasted) {
        let mut code = None;
        let mut state = None;
        for (k, v) in url.query_pairs() {
            match &*k {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                _ => {}
            }
        }
        if let Some(code) = code {
            return (code, state);
        }
    }
    (pasted.to_owned(), None)
}

/// `id_token`（JWT）里的 ChatGPT 账号 id：`https://api.openai.com/auth`.`chatgpt_account_id`。
/// 只解码不验签——它来自我们自己刚向 token 端点换来的响应。
#[must_use]
pub fn account_id_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 换码（表单体，与 Codex CLI 一致）。token 端点走不跟随重定向的探针 client：
/// 地址可被管理员覆写（`oauth_token_url`），SSRF 闸只看得到填进来的那个 URL。
pub async fn exchange(
    http: &crate::http::HttpPool,
    token_url: &str,
    code: &str,
    verifier: &str,
) -> Result<Tokens, UpstreamError> {
    let form = form_encode(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ]);
    let resp = http
        .probe(
            &crate::http::Outbound::default(),
            reqwest::Method::POST,
            token_url,
        )?
        .timeout(TOKEN_TIMEOUT)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form)
        .send()
        .await
        .map_err(|e| classify(&e))?;
    read_tokens(resp).await
}

/// 刷新（JSON 体，与 openai/codex 源码一致）。
pub async fn refresh(
    http: &crate::http::HttpPool,
    token_url: &str,
    refresh_token: &str,
) -> Result<Tokens, UpstreamError> {
    let resp = http
        .probe(
            &crate::http::Outbound::default(),
            reqwest::Method::POST,
            token_url,
        )?
        .timeout(TOKEN_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            json!({
                "client_id": CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            })
            .to_string(),
        )
        .send()
        .await
        .map_err(|e| classify(&e))?;
    read_tokens(resp).await
}

async fn read_tokens(resp: reqwest::Response) -> Result<Tokens, UpstreamError> {
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| classify(&e))?;
    if !(200..300).contains(&status) {
        return Err(UpstreamError::Status {
            status,
            body: bytes,
            retry_after_secs: None,
        });
    }
    parse_tokens(&bytes)
}

/// 该后端不接受的官方 Responses 参数（带上会 400）。`store=false` 让 `previous_response_id` 无意义。
const UNSUPPORTED_FIELDS: [&str; 15] = [
    "previous_response_id",
    "stream_options",
    "prompt_cache_retention",
    "safety_identifier",
    "max_output_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "frequency_penalty",
    "presence_penalty",
    "user",
    "metadata",
    "truncation",
    "stop_sequences",
    "chat_template_kwargs",
];

/// Responses 请求体改成 Codex 后端接受的形状：`store=false`、`stream=true`（只有流式面）、
/// `instructions` 键缺省补空串、`input[].role=system` 改 `developer`、剥掉不支持的参数。
pub fn prepare_body(body: &[u8]) -> Result<Vec<u8>, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    obj.insert("store".to_owned(), Value::Bool(false));
    obj.insert("stream".to_owned(), Value::Bool(true));
    if obj.get("instructions").is_none_or(|v| !v.is_string()) {
        obj.insert("instructions".to_owned(), Value::String(String::new()));
    }
    for field in UNSUPPORTED_FIELDS {
        obj.remove(field);
    }
    if let Some(items) = obj.get_mut("input").and_then(Value::as_array_mut) {
        for item in items.iter_mut() {
            if item.get("role").and_then(Value::as_str) == Some("system") {
                item["role"] = Value::String("developer".to_owned());
            }
        }
    }
    serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))
}

fn has_header(outbound: &crate::http::Outbound, name: &str) -> bool {
    outbound
        .extra_headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case(name))
}

/// 用订阅 access token 发一次 Responses。上游永远走流式；`stream=false` 时在此聚合回 JSON。
pub async fn responses(
    http: &crate::http::HttpPool,
    api_base: &str,
    access_token: &str,
    account_id: Option<&str>,
    body: Bytes,
    stream: bool,
    outbound: &crate::http::Outbound,
) -> Result<ChatResponse, UpstreamError> {
    let url = format!("{}/responses", api_base.trim_end_matches('/'));
    let body = Bytes::from(prepare_body(&body)?);
    let bearer = format!("Bearer {access_token}");
    let mut headers: Vec<(&str, &str)> = vec![
        ("authorization", bearer.as_str()),
        ("accept", "text/event-stream"),
    ];
    // 真实 Codex 客户端自带的 originator / OpenAI-Beta 已经透传在 extra_headers 里，不覆盖
    if !has_header(outbound, "originator") {
        headers.push(("originator", ORIGINATOR));
    }
    if !has_header(outbound, "openai-beta") {
        headers.push(("openai-beta", "responses=experimental"));
    }
    if let Some(id) = account_id {
        headers.push(("chatgpt-account-id", id));
    }
    let resp =
        crate::responses::send_responses_at(http, url, &headers, body, true, outbound).await?;
    match resp {
        ChatResponse::Stream(handle) if !stream => collect_json(handle).await,
        other => Ok(other),
    }
}

/// 把 Responses SSE 聚合成一个非流式 Responses 对象：取终态事件（`response.completed` /
/// `.incomplete` / `.failed`）的 `response`；其 `output` 为空时用 `response.output_item.done`
/// 按 `output_index` 逐项拼回（该后端的终态事件有时不带完整 output）。
async fn collect_json(mut handle: StreamHandle) -> Result<ChatResponse, UpstreamError> {
    let mut items: Vec<(u64, Value)> = Vec::new();
    let mut terminal: Option<Value> = None;
    while let Some(event) = handle.events.next().await {
        let ChatEvent::Data { raw, .. } = event? else {
            break;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        match parsed
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "response.output_item.done" => {
                if let Some(item) = parsed.get("item").cloned() {
                    let index = parsed.get("output_index").and_then(Value::as_u64);
                    items.push((index.unwrap_or(u64::MAX), item));
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                terminal = parsed.get("response").cloned();
                break;
            }
            "error" => {
                return Err(UpstreamError::Status {
                    status: 502,
                    body: Bytes::from(raw),
                    retry_after_secs: None,
                });
            }
            _ => {}
        }
    }
    let mut response =
        terminal.ok_or_else(|| UpstreamError::Stream("codex_no_terminal_event".to_owned()))?;
    let output_missing = response
        .get("output")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    if output_missing && !items.is_empty() {
        items.sort_by_key(|(index, _)| *index);
        response["output"] = Value::Array(items.into_iter().map(|(_, item)| item).collect());
    }
    let usage = usage_from_responses(response.get("usage"));
    let body = serde_json::to_vec(&response).map_err(|e| UpstreamError::Stream(e.to_string()))?;
    Ok(ChatResponse::Json {
        status: 200,
        upstream_request_id: handle.upstream_request_id,
        body: Bytes::from(body),
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::base64url;

    fn jwt_with_payload(payload: &str) -> String {
        format!(
            "{}.{}.sig",
            base64url(br#"{"alg":"RS256"}"#),
            base64url(payload.as_bytes())
        )
    }

    #[test]
    fn authorize_url_has_codex_specific_params() {
        let pkce = Pkce::from_bytes(&[2u8; 32]);
        let parsed = reqwest::Url::parse(&authorize_url(&pkce, "st-1")).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.host_str(), Some("auth.openai.com"));
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["code_challenge"], pkce.challenge);
        assert_eq!(q["state"], "st-1");
        assert_eq!(q["id_token_add_organizations"], "true");
        assert_eq!(q["codex_cli_simplified_flow"], "true");
        assert_eq!(q["originator"], ORIGINATOR);
        assert!(q["scope"].contains("offline_access"));
    }

    #[test]
    fn pasted_callback_url_yields_code_and_state() {
        let (code, state) = split_pasted_code(
            "http://localhost:1455/auth/callback?code=abc&state=xyz&scope=openid",
        );
        assert_eq!((code.as_str(), state.as_deref()), ("abc", Some("xyz")));
        let (code, state) = split_pasted_code("  rawcode  ");
        assert_eq!((code.as_str(), state), ("rawcode", None));
    }

    #[test]
    fn account_id_comes_from_openai_auth_claim() {
        let jwt = jwt_with_payload(
            r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-xyz","chatgpt_plan_type":"plus"}}"#,
        );
        assert_eq!(account_id_from_id_token(&jwt).as_deref(), Some("acct-xyz"));
        assert!(account_id_from_id_token(&jwt_with_payload(r#"{"sub":"u"}"#)).is_none());
        assert!(account_id_from_id_token("not-a-jwt").is_none());
        // 经 parse_tokens 整体解析也能带出来
        let body = format!(
            r#"{{"access_token":"a","refresh_token":"r","expires_in":600,"id_token":"{jwt}"}}"#
        );
        assert_eq!(
            parse_tokens(body.as_bytes()).unwrap().account_id.as_deref(),
            Some("acct-xyz")
        );
    }

    #[test]
    fn prepare_body_matches_codex_backend_requirements() {
        let out = prepare_body(
            br#"{"model":"gpt-5","input":[{"role":"system","content":"be terse"},{"role":"user","content":"hi"}],
            "store":true,"stream":false,"temperature":0.2,"max_output_tokens":10,"previous_response_id":"resp_0",
            "metadata":{"a":"b"},"reasoning":{"effort":"high"},"tools":[]}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["store"], false);
        assert_eq!(v["stream"], true, "该后端只有流式面");
        assert_eq!(v["instructions"], "", "instructions 键必须存在");
        assert_eq!(v["input"][0]["role"], "developer", "system 角色不被接受");
        assert_eq!(v["input"][1]["role"], "user");
        for field in UNSUPPORTED_FIELDS {
            assert!(v.get(field).is_none(), "{field} 应被剥掉");
        }
        assert_eq!(v["reasoning"]["effort"], "high", "原生字段保留");
        assert_eq!(v["model"], "gpt-5");

        let kept =
            prepare_body(br#"{"model":"m","input":"hi","instructions":"you are x"}"#).unwrap();
        let v: Value = serde_json::from_slice(&kept).unwrap();
        assert_eq!(
            v["instructions"], "you are x",
            "客户端给的 instructions 不动"
        );
    }

    // 测试流的元素类型就是 Result，这里统一包一层 Ok
    #[allow(clippy::unnecessary_wraps)]
    fn data(raw: &str) -> Result<ChatEvent, UpstreamError> {
        Ok(ChatEvent::Data {
            raw: raw.to_owned(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage: None,
        })
    }

    #[tokio::test]
    async fn collect_json_rebuilds_output_from_items_when_terminal_lacks_it() {
        let events = vec![
            data(r#"{"type":"response.created","response":{"id":"resp_1"}}"#),
            data(
                r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"message","id":"m2","content":[{"type":"output_text","text":"second"}]}}"#,
            ),
            data(
                r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"r1","summary":[]}}"#,
            ),
            data(
                r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[],
                "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
            ),
        ];
        let handle = StreamHandle {
            upstream_request_id: Some("req-1".to_owned()),
            events: Box::pin(futures::stream::iter(events)),
        };
        let ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } = collect_json(handle).await.unwrap()
        else {
            panic!("expected json");
        };
        assert_eq!(status, 200);
        assert_eq!(upstream_request_id.as_deref(), Some("req-1"));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], "resp_1");
        assert_eq!(v["output"][0]["id"], "r1", "按 output_index 排序");
        assert_eq!(v["output"][1]["content"][0]["text"], "second");
        assert_eq!(usage.unwrap().prompt_tokens, 10);
    }

    #[tokio::test]
    async fn collect_json_keeps_terminal_output_and_surfaces_stream_errors() {
        let handle = StreamHandle {
            upstream_request_id: None,
            events: Box::pin(futures::stream::iter(vec![
                data(
                    r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"stale"}}"#,
                ),
                data(
                    r#"{"type":"response.completed","response":{"id":"r","output":[{"id":"fresh"}],"usage":{"input_tokens":1,"output_tokens":1}}}"#,
                ),
            ])),
        };
        let ChatResponse::Json { body, .. } = collect_json(handle).await.unwrap() else {
            panic!("expected json");
        };
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["output"][0]["id"], "fresh", "终态自带 output 时以它为准");

        let failing = StreamHandle {
            upstream_request_id: None,
            events: Box::pin(futures::stream::iter(vec![data(
                r#"{"type":"error","code":"server_error","message":"boom"}"#,
            )])),
        };
        assert!(matches!(
            collect_json(failing).await,
            Err(UpstreamError::Status { status: 502, .. })
        ));

        let truncated = StreamHandle {
            upstream_request_id: None,
            events: Box::pin(futures::stream::iter(vec![data(
                r#"{"type":"response.created","response":{}}"#,
            )])),
        };
        assert!(matches!(
            collect_json(truncated).await,
            Err(UpstreamError::Stream(_))
        ));
    }
}
