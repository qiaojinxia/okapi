//! 渠道 key 的探测（控制台「测试」按钮与 worker 的后台探测共用）：按协议拼最小请求、带上网关真实请求同款的协议头、
//! 判 2xx 的体是不是 JSON，失败原样交回上游错误，由调用方按 key 状态机的同一张表归类。
//!
//! 能验的是静态 key 的直连渠道。订阅 OAuth（anthropic_max / codex）与云厂商（bedrock / vertex）要走各自的凭证流程，
//! `custom_pass` 是任意路径透传、没有通用的最小补全，都不在这里。
use super::state::AppState;
use okapi_kernel::error::AppError;
use okapi_providers::UpstreamError;
use okapi_providers::custom_pass::{PassRequest, PassResponse};
use serde_json::{Value, json};

/// 探测响应体上限：模型列表和补全都是几百字节到几 KB 的 JSON，8 MiB 已经很宽。
const PROBE_BODY_MAX_BYTES: usize = 8 * 1024 * 1024;

/// 后台探测能验的协议（静态 key 的直连渠道）。
pub const PROBEABLE_PROVIDERS: &[&str] =
    &["openai", "openai_compat", "azure", "anthropic", "gemini"];

/// 探测补全的输出上限。取 16 而非 1：实测有上游把 `max_tokens=1` 直接 400
/// （`max_tokens must be greater than 2`），好模型会被误报成不可用；16 仍然便宜到可忽略，
/// 兼容性却好得多（推理模型的最小输出预算也常常大于 1）。
pub const PROBE_MAX_TOKENS: u32 = 16;

/// Azure 部署列表端点用的 api-version：`deployments` 是数据面早期版本里的资源，
/// 新版本 spec 已不再列出但端点仍可用（one-api / new-api 同此用法）。
/// 站长配的 `settings.api_version` 是给补全端点的，两者不混用。
pub const AZURE_DEPLOYMENTS_API_VERSION: &str = "2023-03-15-preview";

/// Azure 部署列表 URL（凭证探测与模型发现共用）。
pub fn azure_deployments_url(base: &str) -> String {
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
pub fn probe_request(
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
pub fn with_protocol_headers(
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
pub async fn probe_body_problem(
    provider: &str,
    stream: impl futures::Stream<Item = Result<bytes::Bytes, okapi_providers::UpstreamError>> + Unpin,
) -> Option<(&'static str, String)> {
    if provider == "custom_pass" {
        return None;
    }
    let Ok(body) = okapi_providers::limits::collect(stream, PROBE_BODY_MAX_BYTES).await else {
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

/// 凭证头：与网关真实请求同款（Anthropic `x-api-key`、Gemini `x-goog-api-key`、Azure `api-key`，其余 Bearer）。
#[must_use]
pub fn credential_header(provider: &str, credential: String) -> (String, String) {
    match provider {
        "anthropic" => ("x-api-key".to_owned(), credential),
        "gemini" => ("x-goog-api-key".to_owned(), credential),
        "azure" => ("api-key".to_owned(), credential),
        _ => ("authorization".to_owned(), format!("Bearer {credential}")),
    }
}

/// 一次模型级探测要用到的全部输入。
pub struct ProbeTarget<'a> {
    pub provider: &'a str,
    /// 渠道地址，不带结尾的 `/`。
    pub api_base: &'a str,
    /// 上游名（已过 `model_mapping`；azure 下即部署名）。
    pub model: &'a str,
    pub api_version: Option<&'a str>,
    pub settings: &'a Value,
    pub credential: String,
    /// 该 key 解析出的出口代理；出口不可用的由调用方在这之前处理，探测绝不退回直连。
    pub proxy_url: Option<String>,
}

/// 一次探测的结果。`failure` 是和真实请求同形状的上游错误，可以直接交给 [`failure_kind`] 归类。
pub struct ProbeOutcome {
    pub latency_ms: i64,
    pub http_status: Option<u16>,
    pub failure: Option<UpstreamError>,
}

/// 发一次 16 token 的最小补全，验「**这个模型**调得通吗」。只探 `/models` 会对一个实际返回 403 的模型报通
/// （聚合型上游按套餐给模型授权，凭证本身全站有效），所以后台探测一律走模型级。
///
/// 2xx 的体不是 JSON（站点的网页前端对未知路径回 `200 text/html`）按失败返回，种类放在 `Stream` 的原因里。
///
/// # Errors
/// 协议不支持最小补全（`custom_pass`）。
pub async fn probe_model(
    state: &AppState,
    target: ProbeTarget<'_>,
) -> Result<ProbeOutcome, AppError> {
    let (method, url, body) = probe_request(
        target.provider,
        target.api_base,
        Some(target.model),
        target.api_version,
    )?;
    let (auth_header, auth_value) = credential_header(target.provider, target.credential);
    let outbound = okapi_providers::Outbound::from_settings(target.settings, target.proxy_url);
    let started = std::time::Instant::now();
    let outcome = state
        .upstreams
        .pass
        .probe(PassRequest {
            method,
            url,
            auth_header,
            auth_value,
            content_type: Some("application/json".to_owned()),
            body,
            proxy_url: outbound.proxy_url,
            extra_headers: with_protocol_headers(target.provider, outbound.extra_headers),
        })
        .await;
    let latency_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let (http_status, failure) = match outcome {
        Ok(PassResponse::Ok { status, stream, .. }) => {
            match probe_body_problem(target.provider, stream).await {
                None => (Some(status), None),
                Some((kind, _head)) => (Some(status), Some(UpstreamError::Stream(kind.to_owned()))),
            }
        }
        Ok(PassResponse::ErrStatus { status, body }) => (
            Some(status),
            Some(UpstreamError::Status {
                status,
                body,
                retry_after_secs: None,
            }),
        ),
        Err(error) => (None, Some(error)),
    };
    Ok(ProbeOutcome {
        latency_ms,
        http_status,
        failure,
    })
}

/// 探测失败该记到 key 状态机的哪一类——与真实请求同一张表（[`super::upstream_failure`]）。
#[must_use]
pub fn failure_kind(error: &UpstreamError) -> okapi_store::channels::KeyFailure {
    super::upstream_failure::failure_kind_of(error)
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

#[cfg(test)]
mod credential_header_tests {
    use super::credential_header;

    #[test]
    fn the_credential_goes_out_in_the_header_the_gateway_uses_for_real_requests() {
        let key = || "k-1".to_owned();
        assert_eq!(
            credential_header("anthropic", key()),
            ("x-api-key".into(), "k-1".into())
        );
        assert_eq!(
            credential_header("gemini", key()),
            ("x-goog-api-key".into(), "k-1".into())
        );
        assert_eq!(
            credential_header("azure", key()),
            ("api-key".into(), "k-1".into())
        );
        for provider in ["openai", "openai_compat"] {
            assert_eq!(
                credential_header(provider, key()),
                ("authorization".into(), "Bearer k-1".into()),
                "{provider}"
            );
        }
    }
}
