//! 渠道测活与上游模型发现（保存前的草稿渠道也能拉模型）。
use super::{audit, ensure_channel_owner, guard_scoped, validate_channel_settings};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use crate::kernel::extract::Path;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::permissions;
use serde::Deserialize;
use serde_json::{Value, json};

/// 渠道测活：按渠道协议发轻量探测（models 列表/根路径），返回可达性与延迟。
/// 语义：2xx = ok；401/403 = 可达但凭证问题（http_status 供管理员判断）；
/// 网络失败 = 不可达。own 范围管理员只能测自己名下渠道。
#[derive(Deserialize, Default)]
pub struct TestChannelReq {
    /// 给了就真发一次 1 token 的最小补全验这个模型；不给只验凭证与连通性。
    #[serde(default)]
    pub model: Option<String>,
}

pub async fn test_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
    // Option<Json>：老调用方不带 body（也就没有 content-type），必须仍然能测
    body: Option<Json<TestChannelReq>>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let result = probe_channel(&state, channel_id, model).await?;
    audit(
        &state,
        &actor,
        "channel.test",
        &channel_id.to_string(),
        result.clone(),
    )
    .await;
    Ok(Json(result))
}

/// 探测补全的输出上限。取 16 而非 1：实测有上游把 `max_tokens=1` 直接 400
/// （`max_tokens must be greater than 2`），好模型会被误报成不可用；16 仍然便宜到可忽略，
/// 兼容性却好得多（推理模型的最小输出预算也常常大于 1）。
pub(super) const PROBE_MAX_TOKENS: u32 = 16;

/// Azure 部署列表端点用的 api-version：`deployments` 是数据面早期版本里的资源，
/// 新版本 spec 已不再列出但端点仍可用（one-api / new-api 同此用法）。
/// 站长配的 `settings.api_version` 是给补全端点的，两者不混用。
pub(super) const AZURE_DEPLOYMENTS_API_VERSION: &str = "2023-03-15-preview";

/// Azure 部署列表 URL（凭证探测与模型发现共用）。
pub(super) fn azure_deployments_url(base: &str) -> String {
    format!(
        "{}/openai/deployments?api-version={AZURE_DEPLOYMENTS_API_VERSION}",
        okapi_providers::azure::normalize_endpoint(base)
    )
}

/// 按协议与探测范围拼探测请求：`(method, url, body)`。
/// 无 model = GET 协议各自的模型列表端点；有 model = POST 一次最小补全。
/// `model` 已是上游名（azure 下即部署名）；`api_version` 只对 azure 有意义。
///
/// 地址的约定与真实请求一致：`api_base` 已含版本段（`anthropic` 缺省 `https://api.anthropic.com/v1`、
/// OpenAI 兼容缺省 `…/v1`），探测只在后面接各协议自己的路径——探测走的路必须和流量是同一条，
/// 否则「测试通过却发不出去」或「地址对了测试反而红」都会误导运维。
pub(super) fn probe_request(
    provider: &str,
    base: &str,
    model: Option<&str>,
    api_version: Option<&str>,
) -> Result<(axum::http::Method, String, bytes::Bytes), AppError> {
    let Some(m) = model else {
        let url = match provider {
            "custom_pass" => base.to_owned(),
            "azure" => azure_deployments_url(base),
            _ => format!("{base}/models"),
        };
        return Ok((axum::http::Method::GET, url, bytes::Bytes::new()));
    };
    let (url, payload) = match provider {
        "anthropic" => (
            format!("{base}/messages"),
            json!({"model": m, "max_tokens": PROBE_MAX_TOKENS,
                   "messages": [{"role": "user", "content": "ping"}]}),
        ),
        "gemini" => (
            format!(
                "{base}/models/{}:generateContent",
                okapi_providers::gemini::path_segment(m)
            ),
            json!({"contents": [{"parts": [{"text": "ping"}]}],
                   "generationConfig": {"maxOutputTokens": PROBE_MAX_TOKENS}}),
        ),
        "azure" => (
            okapi_providers::azure::deployment_url(
                base,
                m,
                "/chat/completions",
                api_version.unwrap_or(okapi_providers::azure::DEFAULT_API_VERSION),
            ),
            json!({"model": m, "max_tokens": PROBE_MAX_TOKENS,
                   "messages": [{"role": "user", "content": "ping"}]}),
        ),
        // custom_pass 是任意路径透传，没有「一次最小补全」的通用形状
        "custom_pass" => {
            return Err(AppError::bad_request().with_param("model_probe_unsupported"));
        }
        _ => (
            format!("{base}/chat/completions"),
            json!({"model": m, "max_tokens": PROBE_MAX_TOKENS,
                   "messages": [{"role": "user", "content": "ping"}]}),
        ),
    };
    Ok((
        axum::http::Method::POST,
        url,
        bytes::Bytes::from(serde_json::to_vec(&payload).unwrap_or_default()),
    ))
}

/// 探测要带上网关真实请求同款的协议头：`anthropic` 直连渠道的 `anthropic-version`（官方端点缺它直接 400）；
/// 站长在设置里额外配了同名头就用站长的。
pub(super) fn with_protocol_headers(
    provider: &str,
    mut headers: Vec<(String, String)>,
) -> Vec<(String, String)> {
    if provider == "anthropic"
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
    {
        headers.push((
            "anthropic-version".to_owned(),
            okapi_providers::anthropic::ANTHROPIC_VERSION.to_owned(),
        ));
    }
    headers
}

/// 2xx 不等于通：很多站点的网页前端对任何未知路径都回 `200 text/html`，地址缺了版本段（`/v1`）时探测会被它接住，
/// 看上去一切正常、真实请求却打不通。模型列表与补全都该是 JSON；不是就返回原因。
/// `custom_pass` 是任意路径透传，体不一定是 JSON，不在此列。
pub(super) async fn probe_body_problem(
    provider: &str,
    stream: impl futures::Stream<Item = Result<bytes::Bytes, okapi_providers::UpstreamError>> + Unpin,
) -> Option<(&'static str, String)> {
    if provider == "custom_pass" {
        return None;
    }
    let Ok(body) = super::super::outbound_body::collect(stream, FETCH_MODELS_MAX_BYTES).await
    else {
        return Some(("upstream_body_unreadable", String::new()));
    };
    if serde_json::from_slice::<Value>(&body).is_ok() {
        return None;
    }
    let head: String = String::from_utf8_lossy(&body)
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .take(160)
        .collect();
    Some(("upstream_not_json", head))
}

/// 渠道探测核心（REST 与 MCP channel_test 共用）。
///
/// `model = None`：只打 `/models`，验的是「凭证认不认、网络通不通」。
/// `model = Some(m)`：真发一次 1 token 的最小补全，验的是「**这个模型**调得通吗」。
///
/// 为什么要分两种：聚合型上游的凭证是全站有效的，但模型按套餐授权——只探 `/models`
/// 会对一个实际返回 403 的模型报 `ok: true`，把运维直接引到错误结论上（实测复现过）。
/// 模型探测会在上游真实产生一次调用（`max_tokens=1`），是管理员显式动作，不计站内账。
// 协议 × 探测范围的分派与结果留痕放同一视野
#[allow(clippy::too_many_lines)]
pub(crate) async fn probe_channel(
    state: &AppState,
    channel_id: i64,
    model: Option<&str>,
) -> Result<Value, AppError> {
    // 先取启用的 key：停用 / 失效的 key 测出来的 401 会把运维引到「渠道坏了」，其实别的 key 好好的
    let row = sqlx::query!(
        r#"
        SELECT c.provider, c.api_base, c.model_mapping, c.settings,
               NULLIF(c.settings ->> 'api_version', '') AS api_version,
               ck.id AS channel_key_id, ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        WHERE c.id = $1 AND c.deleted_at IS NULL
        ORDER BY (ck.status = 1) DESC, ck.id
        LIMIT 1
        "#,
        channel_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            okapi_api::codes::NO_AVAILABLE_CHANNEL,
        )
    })?;
    let credential =
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
    let base = row.api_base.unwrap_or_default();
    let base = base.trim_end_matches('/');
    // 探的是网关真正会发出去的名字：过一遍 model_mapping（azure 下映射值就是部署名，
    // 不映射直接打规范名必 404，其它协议映射改名的场景同理）
    let upstream_model = model.map(|m| {
        row.model_mapping
            .get(m)
            .and_then(Value::as_str)
            .unwrap_or(m)
            .to_owned()
    });

    // bedrock / vertex 要签名 / 换 token，走各自客户端（§11.35）
    if super::super::cloud_probe::is_cloud(&row.provider) {
        let proxy_url = super::super::egress::key_proxy(state, row.channel_key_id).await?;
        let result = super::super::cloud_probe::probe(
            state,
            &row.provider,
            base,
            &row.settings,
            &credential,
            row.channel_key_id,
            upstream_model.as_deref(),
            proxy_url,
        )
        .await;
        state.sched.channel_test_record(channel_id, &result).await;
        return Ok(result);
    }

    // 按协议选探测端点与凭证头
    let (auth_header, auth_value) = match row.provider.as_str() {
        "anthropic" => ("x-api-key".to_owned(), credential),
        "gemini" => ("x-goog-api-key".to_owned(), credential),
        "azure" => ("api-key".to_owned(), credential),
        _ => ("authorization".to_owned(), format!("Bearer {credential}")),
    };
    let (method, url, body) = probe_request(
        &row.provider,
        base,
        upstream_model.as_deref(),
        row.api_version.as_deref(),
    )?;
    let content_type = (!body.is_empty()).then(|| "application/json".to_owned());
    let outbound = okapi_providers::Outbound::from_settings(
        &row.settings,
        super::super::egress::key_proxy(state, row.channel_key_id).await?,
    );

    let started = std::time::Instant::now();
    let outcome = state
        .pass
        .probe(okapi_providers::custom_pass::PassRequest {
            method,
            url,
            auth_header,
            auth_value,
            content_type,
            body,
            proxy_url: outbound.proxy_url,
            extra_headers: with_protocol_headers(&row.provider, outbound.extra_headers),
        })
        .await;
    let latency_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let at = chrono::Utc::now().to_rfc3339();
    // scope 让调用方一眼知道这个 ok 是什么意思：credential=只验了凭证与连通性，
    // model=这个模型真的调得通。少了它，两种探测的 `ok:true` 长得一模一样。
    let scope = if model.is_some() {
        "model"
    } else {
        "credential"
    };
    let result = match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { status, stream, .. }) => {
            match probe_body_problem(&row.provider, stream).await {
                None => {
                    json!({"ok": true, "http_status": status, "latency_ms": latency_ms, "at": at,
                               "scope": scope, "model": model})
                }
                // 不带 http_status：列表的「最近测试」列有它就只显示「HTTP 200」，看不出为什么算失败
                Some((code, head)) => {
                    json!({"ok": false, "error_code": code, "latency_ms": latency_ms,
                                             "at": at, "scope": scope, "model": model,
                                             "upstream_body": head})
                }
            }
        }
        Ok(okapi_providers::custom_pass::PassResponse::ErrStatus { status, body }) => {
            // 失败带上游原文（截断）：403 到底是凭证问题还是模型没开通，全在这句话里
            let detail: String = String::from_utf8_lossy(&body).chars().take(300).collect();
            json!({"ok": false, "http_status": status, "latency_ms": latency_ms, "at": at,
                   "scope": scope, "model": model, "upstream_body": detail})
        }
        Err(err) => {
            json!({"ok": false, "error_code": err.error_code(), "latency_ms": latency_ms,
                   "at": at, "scope": scope, "model": model})
        }
    };
    // 留痕供列表页"最近测试"列回填（new-api 的 response_time/test_time 语义）
    state.sched.channel_test_record(channel_id, &result).await;
    Ok(result)
}

/// 上游模型列表响应大小上限（Sub2API 0.1.180 吸收，防超大响应耗尽内存）。
pub(super) const FETCH_MODELS_MAX_BYTES: usize = 8 * 1024 * 1024;

/// 渠道上游模型发现（§11.3，new-api #6184 吸收）：拉上游 /models 返回 id 列表，
/// 由管理员确认后经既有渠道更新通道写入（不自动改配置）。
pub async fn fetch_channel_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    let row = sqlx::query!(
        r#"
        SELECT c.provider, c.api_base, c.settings, ck.id AS channel_key_id,
               ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        WHERE c.id = $1 AND c.deleted_at IS NULL
        ORDER BY (ck.status = 1) DESC, ck.id
        LIMIT 1
        "#,
        channel_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            okapi_api::codes::NO_AVAILABLE_CHANNEL,
        )
    })?;
    let credential =
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
    let base = row.api_base.unwrap_or_default();
    let proxy_url = super::super::egress::key_proxy(&state, row.channel_key_id).await?;
    let models = discover_models(
        &state,
        DiscoverTarget {
            provider: &row.provider,
            base: base.trim_end_matches('/'),
            settings: &row.settings,
            credential,
            channel_key_id: row.channel_key_id,
            proxy_url,
        },
    )
    .await?;
    Ok(Json(json!({ "channel_id": channel_id, "models": models })))
}

#[derive(Deserialize)]
pub struct DraftModelsReq {
    pub provider: String,
    #[serde(default)]
    pub api_base: Option<String>,
    pub credential: String,
    #[serde(default)]
    pub settings: Option<Value>,
    #[serde(default)]
    pub egress: Option<okapi_store::egress::Binding>,
}

/// 新建渠道时的上游模型发现：渠道还没落库，用表单里刚填的地址 / 凭证 / 设置 / 出口。
/// 校验与建渠道同一套（地址过 SSRF 闸、设置合法、出口对操作者可见）；凭证只用于这一次请求，
/// 不落库。订阅账号（OAuth）的凭证是登录流程换出来的令牌，没有可拿来试的草稿凭证，要先建渠道。
pub async fn fetch_draft_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<DraftModelsReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let provider = req.provider.trim();
    let descriptor = okapi_providers::registry::lookup(provider)
        .ok_or_else(|| AppError::bad_request().with_param("provider"))?;
    if matches!(
        descriptor.credential,
        okapi_providers::registry::CredentialKind::OAuth(_)
    ) {
        return Err(AppError::bad_request().with_param("fetch_models_requires_saved_channel"));
    }
    let credential = req.credential.trim();
    if credential.is_empty() {
        return Err(AppError::bad_request().with_param("credential"));
    }
    validate_channel_settings(&state, provider, req.settings.as_ref()).await?;
    let binding = req.egress.unwrap_or(okapi_store::egress::Binding::Inherit);
    super::super::egress::validate_binding(&state, &binding, &actor, scope).await?;
    let base =
        super::super::channel_creation::endpoint(&state, provider, req.api_base.as_deref()).await?;
    let (resolved, _) =
        okapi_store::egress::pick_for_new_key(&state.pg, &binding, state.master_key.as_deref())
            .await?;
    let settings = req.settings.unwrap_or_else(|| json!({}));
    let models = discover_models(
        &state,
        DiscoverTarget {
            provider,
            base: base.trim_end_matches('/'),
            settings: &settings,
            credential: credential.to_owned(),
            channel_key_id: 0,
            proxy_url: resolved.proxy_url()?,
        },
    )
    .await?;
    Ok(Json(json!({ "models": models })))
}

/// 一次上游模型发现要用到的全部输入。`channel_key_id` 在草稿（还没落库）时是 0，只有订阅账号才用得上。
pub(super) struct DiscoverTarget<'a> {
    provider: &'a str,
    base: &'a str,
    settings: &'a Value,
    credential: String,
    channel_key_id: i64,
    proxy_url: Option<String>,
}

/// 云厂商走各自的签名 / 令牌流程，其余协议拉 `{base}/models`（Azure 拉部署列表），返回去重排序的 id。
pub(super) async fn discover_models(
    state: &AppState,
    target: DiscoverTarget<'_>,
) -> Result<Vec<String>, AppError> {
    let DiscoverTarget {
        provider,
        base,
        settings,
        credential,
        channel_key_id,
        proxy_url,
    } = target;
    if super::super::cloud_probe::is_cloud(provider) {
        return super::super::cloud_probe::fetch_models(
            state,
            provider,
            base,
            settings,
            &credential,
            channel_key_id,
            proxy_url,
        )
        .await;
    }
    let (url, auth_header, auth_value) = models_probe_target(provider, base, credential);
    let outbound = okapi_providers::Outbound::from_settings(settings, proxy_url);
    let outcome = state
        .pass
        .probe(okapi_providers::custom_pass::PassRequest {
            method: axum::http::Method::GET,
            url,
            auth_header,
            auth_value,
            content_type: None,
            body: bytes::Bytes::new(),
            proxy_url: outbound.proxy_url,
            extra_headers: with_protocol_headers(provider, outbound.extra_headers),
        })
        .await;
    let body = match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { stream, .. }) => {
            super::super::outbound_body::collect(stream, FETCH_MODELS_MAX_BYTES).await?
        }
        Ok(okapi_providers::custom_pass::PassResponse::ErrStatus { status, .. }) => {
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                    .with_param(format!("status_{status}")),
            );
        }
        Err(err) => {
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                    .with_param(err.error_code()),
            );
        }
    };
    let parsed: Value = serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request().with_param("upstream_models_not_json"))?;
    Ok(parse_upstream_model_ids(&parsed))
}

pub(super) fn models_probe_target(
    provider: &str,
    base: &str,
    credential: String,
) -> (String, String, String) {
    match provider {
        "anthropic" => (format!("{base}/models"), "x-api-key".to_owned(), credential),
        "gemini" => (
            format!("{base}/models"),
            "x-goog-api-key".to_owned(),
            credential,
        ),
        // Azure 部署名才是可路由的模型名，不能换成资源模型目录。
        "azure" => (
            azure_deployments_url(base),
            "api-key".to_owned(),
            credential,
        ),
        _ => (
            format!("{base}/models"),
            "authorization".to_owned(),
            format!("Bearer {credential}"),
        ),
    }
}

/// 上游模型列表 → 去重排序的 id 列表。
/// openai/anthropic/azure 部署: `{data:[{id}]}`；gemini: `{models:[{name: "models/x"}]}`。
pub(super) fn parse_upstream_model_ids(parsed: &Value) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    for item in parsed
        .get("data")
        .or_else(|| parsed.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            models.push(id.to_owned());
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            models.push(name.strip_prefix("models/").unwrap_or(name).to_owned());
        }
    }
    models.sort();
    models.dedup();
    models
}

#[cfg(test)]
mod probe_request_tests {
    use super::*;

    fn url(provider: &str, base: &str, model: Option<&str>) -> String {
        probe_request(provider, base, model, None).unwrap().1
    }

    /// 探测与真实请求同一条路：地址已含版本段，探测只在后面接各协议自己的路径。
    #[test]
    fn probes_follow_the_traffic_path_for_a_base_that_carries_the_version_segment() {
        let anthropic = "https://api.anthropic.com/v1";
        assert_eq!(
            url("anthropic", anthropic, Some("m")),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            url("anthropic", anthropic, None),
            "https://api.anthropic.com/v1/models"
        );
        let openai = "https://api.openai.com/v1";
        assert_eq!(
            url("openai", openai, Some("m")),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            url("openai", openai, None),
            "https://api.openai.com/v1/models"
        );
    }

    #[test]
    fn anthropic_probes_carry_the_version_header_unless_the_operator_set_one() {
        let version = okapi_providers::anthropic::ANTHROPIC_VERSION;
        let added = with_protocol_headers("anthropic", vec![("x-extra".into(), "1".into())]);
        assert!(added.contains(&("anthropic-version".to_owned(), version.to_owned())));
        let own = with_protocol_headers(
            "anthropic",
            vec![("Anthropic-Version".into(), "2099-01-01".into())],
        );
        assert_eq!(own.len(), 1, "站长配的同名头不重复添加：{own:?}");
        for provider in ["openai", "gemini", "azure", "custom_pass"] {
            assert!(
                with_protocol_headers(provider, Vec::new()).is_empty(),
                "{provider}"
            );
        }
    }

    #[tokio::test]
    async fn a_probe_body_must_be_json_unless_the_channel_is_a_passthrough() {
        let stream = |text: &'static str| {
            futures::stream::iter(vec![Ok::<_, okapi_providers::UpstreamError>(
                bytes::Bytes::from_static(text.as_bytes()),
            )])
        };
        assert!(
            probe_body_problem("anthropic", stream(r#"{"data":[]}"#))
                .await
                .is_none()
        );
        let (code, head) =
            probe_body_problem("anthropic", stream("<!doctype html>\n<html lang=\"en\">"))
                .await
                .unwrap();
        assert_eq!(code, "upstream_not_json");
        assert!(head.starts_with("<!doctype html>"), "{head}");
        assert!(!head.contains('\n'), "控制字符不进结果：{head:?}");
        assert!(
            probe_body_problem("custom_pass", stream("<html>"))
                .await
                .is_none()
        );
    }
}
