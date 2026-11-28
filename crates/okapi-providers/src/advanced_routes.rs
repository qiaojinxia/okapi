//! 声明式 HTTP 路由映射（`channels.settings.advanced_routes`）。
//!
//! 允许仅通过渠道 JSON 配置对接非标准上游路径、自定义鉴权头/查询参数、按模型通配分流以及
//! 按名称引用 [`crate::convert::registry`] 转换器，无需修改 Rust 代码重新编译发版。

use crate::convert::registry::{self as conv_registry, ConverterSpec};
use crate::error::UpstreamError;
use crate::http::{HttpPool, Outbound};
use crate::inference::Response as InferenceResponse;
use crate::registry::{self as provider_registry, ApiFormat, CredentialKind};
use bytes::Bytes;
use okapi_api::contracts::{AdvancedRouteAuthDto, AdvancedRouteDto};
use serde_json::Value;

const MAX_ROUTES: usize = 32;
const MAX_ROUTES_BYTES: usize = 16 * 1024;
const MAX_MODELS_PER_ROUTE: usize = 64;
const MAX_PATH_LEN: usize = 512;
const MAX_HEADER_LEN: usize = 64;

const FORBIDDEN_AUTH_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
];

/// 单条命中的声明式路由及其已解析的转换器规范。
#[derive(Clone, Debug)]
pub struct MatchedRoute {
    pub index: usize,
    pub route: AdvancedRouteDto,
    pub converter: &'static ConverterSpec,
}

/// 控制台写入校验：`channels.settings.advanced_routes`。
pub fn validate_routes_value(provider: &str, value: &Value) -> Result<(), &'static str> {
    if value.is_null() {
        return Ok(());
    }
    let Some(arr) = value.as_array() else {
        return Err("advanced_routes");
    };
    if arr.len() > MAX_ROUTES {
        return Err("advanced_routes_too_many");
    }
    if serde_json::to_vec(value)
        .ok()
        .is_none_or(|bytes| bytes.len() > MAX_ROUTES_BYTES)
    {
        return Err("advanced_routes_too_large");
    }
    if !arr.is_empty() {
        let static_key = provider_registry::lookup(provider)
            .is_some_and(|desc| desc.credential == CredentialKind::StaticKey);
        if !static_key {
            return Err("advanced_routes_provider_unsupported");
        }
    }
    let routes: Vec<AdvancedRouteDto> =
        serde_json::from_value(value.clone()).map_err(|_| "advanced_routes")?;
    for route in &routes {
        validate_single_route(route)?;
    }
    Ok(())
}

fn validate_single_route(route: &AdvancedRouteDto) -> Result<(), &'static str> {
    let incoming = route.incoming_path.trim();
    if incoming.is_empty() || incoming.len() > MAX_PATH_LEN {
        return Err("advanced_routes_incoming_path");
    }
    let pinned_ingress = parse_incoming_path_format(incoming)?;
    if !safe_upstream_path(&route.upstream_path) {
        return Err("advanced_routes_upstream_path");
    }
    if let Some(stream_path) = route.stream_upstream_path.as_deref()
        && !safe_upstream_path(stream_path)
    {
        return Err("advanced_routes_stream_upstream_path");
    }
    if route.models.len() > MAX_MODELS_PER_ROUTE
        || route
            .models
            .iter()
            .any(|m| m.trim().is_empty() || m.len() > 128)
    {
        return Err("advanced_routes_models");
    }
    validate_converter_choice(pinned_ingress, route)?;
    if let Some(auth) = route.auth.as_ref() {
        validate_auth(auth)?;
    }
    Ok(())
}

fn validate_converter_choice(
    pinned_ingress: Option<ApiFormat>,
    route: &AdvancedRouteDto,
) -> Result<(), &'static str> {
    let raw = route
        .converter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(name) = raw else {
        return Ok(());
    };
    if matches!(name, "auto" | "passthrough") {
        return Ok(());
    }
    let Some(spec) = conv_registry::lookup_by_name(name) else {
        return Err("advanced_routes_converter");
    };
    if route.pass_through_body && !spec.is_native() {
        return Err("advanced_routes_pass_through_conflict");
    }
    if let Some(expected_from) = pinned_ingress
        && spec.from != expected_from
    {
        return Err("advanced_routes_converter_mismatch");
    }
    Ok(())
}

fn validate_auth(auth: &AdvancedRouteAuthDto) -> Result<(), &'static str> {
    let has_header = auth
        .header
        .as_deref()
        .is_some_and(|h| !h.trim().is_empty());
    let has_query = auth
        .query_param
        .as_deref()
        .is_some_and(|q| !q.trim().is_empty());
    if has_header && has_query {
        return Err("advanced_routes_auth");
    }
    if let Some(header) = auth.header.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let lower = header.to_ascii_lowercase();
        if header.len() > MAX_HEADER_LEN
            || FORBIDDEN_AUTH_HEADERS.contains(&lower.as_str())
            || !header
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err("advanced_routes_auth_header");
        }
    }
    if let Some(prefix) = auth.prefix.as_deref()
        && (prefix.len() > MAX_HEADER_LEN
            || prefix
                .bytes()
                .any(|b| b < 0x20 || b == 0x7f || matches!(b, b'\r' | b'\n')))
    {
        return Err("advanced_routes_auth_prefix");
    }
    if let Some(qp) = auth
        .query_param
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && (qp.len() > MAX_HEADER_LEN
            || !qp
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
    {
        return Err("advanced_routes_auth_query_param");
    }
    Ok(())
}

/// 严格校验上游相对路径，杜绝跨主机跳转、路径遍历与头注入。
#[must_use]
pub fn safe_upstream_path(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_PATH_LEN
        || !trimmed.starts_with('/')
        || trimmed.starts_with("//")
    {
        return false;
    }
    if trimmed.contains("..")
        || trimmed.contains('\\')
        || trimmed.contains("://")
        || trimmed.contains('@')
        || trimmed
            .bytes()
            .any(|b| b < 0x20 || b == 0x7f || matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    {
        return false;
    }
    true
}

/// 识别 `incoming_path` 对应的入口格式：`None` 表示 `"*"` 通配（匹配任意非 compact 入口）。
fn parse_incoming_path_format(raw: &str) -> Result<Option<ApiFormat>, &'static str> {
    let path = raw.trim().split('?').next().unwrap_or("");
    match path {
        "*" => Ok(None),
        "/v1/chat/completions" => Ok(Some(ApiFormat::ChatCompletions)),
        "/v1/messages" => Ok(Some(ApiFormat::Messages)),
        "/v1/responses" => Ok(Some(ApiFormat::Responses)),
        "/v1/responses/compact" => Ok(Some(ApiFormat::ResponsesCompact)),
        other if parse_gemini_incoming_model(other).is_some() => Ok(Some(ApiFormat::Gemini)),
        _ => Err("advanced_routes_incoming_path"),
    }
}

/// 匹配 Gemini 入口路径（支持 `{model}` 占位符以及 `:generateContent` ↔ `:streamGenerateContent` 等价）。
fn parse_gemini_incoming_model(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/v1beta/models/")?;
    let model = rest
        .strip_suffix(":generateContent")
        .or_else(|| rest.strip_suffix(":streamGenerateContent"))?;
    (!model.is_empty() && !model.contains('/')).then_some(model)
}

/// 按声明顺序匹配首条命中的 `advanced_routes` 规则；无配置时零分配直接返回 `None`。
#[must_use]
pub fn match_route(
    raw: &Value,
    ingress: ApiFormat,
    requested_model: &str,
    upstream_model: &str,
) -> Option<MatchedRoute> {
    let arr = raw.as_array()?;
    if arr.is_empty() {
        return None;
    }
    for (index, item) in arr.iter().enumerate() {
        let Ok(route) = serde_json::from_value::<AdvancedRouteDto>(item.clone()) else {
            continue;
        };
        if !safe_upstream_path(&route.upstream_path) {
            continue;
        }
        if let Some(stream_path) = route.stream_upstream_path.as_deref()
            && !safe_upstream_path(stream_path)
        {
            continue;
        }
        if !matches_incoming(&route.incoming_path, ingress, requested_model, upstream_model) {
            continue;
        }
        if !matches_models(&route.models, requested_model, upstream_model) {
            continue;
        }
        let Some(converter) = resolve_converter(&route, ingress) else {
            continue;
        };
        return Some(MatchedRoute {
            index,
            route,
            converter,
        });
    }
    None
}

/// 按下标取回已匹配的路由定义（供热路径执行期使用）。
#[must_use]
pub fn route_at(raw: &Value, index: usize) -> Option<AdvancedRouteDto> {
    let item = raw.as_array()?.get(index)?;
    serde_json::from_value::<AdvancedRouteDto>(item.clone()).ok()
}

fn matches_incoming(
    pattern: &str,
    ingress: ApiFormat,
    requested_model: &str,
    upstream_model: &str,
) -> bool {
    let path = pattern.trim().split('?').next().unwrap_or("");
    if path == "*" {
        return ingress != ApiFormat::ResponsesCompact;
    }
    match ingress {
        ApiFormat::ChatCompletions => path == "/v1/chat/completions",
        ApiFormat::Messages => path == "/v1/messages",
        ApiFormat::Responses => path == "/v1/responses",
        ApiFormat::ResponsesCompact => path == "/v1/responses/compact",
        ApiFormat::Gemini => parse_gemini_incoming_model(path).is_some_and(|model_pat| {
            model_pat == "{model}"
                || wildcard_match(model_pat, requested_model)
                || wildcard_match(model_pat, upstream_model)
        }),
    }
}

fn matches_models(patterns: &[String], requested_model: &str, upstream_model: &str) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|pat| {
        let pat = pat.trim();
        !pat.is_empty()
            && (wildcard_match(pat, requested_model) || wildcard_match(pat, upstream_model))
    })
}

fn resolve_converter(route: &AdvancedRouteDto, ingress: ApiFormat) -> Option<&'static ConverterSpec> {
    let raw = route
        .converter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if route.pass_through_body || matches!(raw, None | Some("auto" | "passthrough")) {
        return conv_registry::lookup(ingress, ingress);
    }
    let spec = conv_registry::lookup_by_name(raw?)?;
    (spec.from == ingress).then_some(spec)
}

/// 轻量 `*` 通配匹配（线性双指针，无正则回溯风险）。
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut match_ti = 0usize;
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            match_ti = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            match_ti += 1;
            ti = match_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// 计算单条声明式路由的模板标签（供账单与诊断 `upstream_endpoint` 记录）。
#[must_use]
pub fn endpoint_label(route: &AdvancedRouteDto, stream: bool) -> String {
    if stream
        && let Some(path) = route.stream_upstream_path.as_deref()
        && !path.trim().is_empty()
    {
        return path.trim().to_owned();
    }
    route.upstream_path.trim().to_owned()
}

/// 渲染最终上游请求 URL（`{model}` 经安全 URI 段编码，查询参数鉴权自动编码拼接）。
pub fn render_upstream_url(
    base: &str,
    route: &AdvancedRouteDto,
    upstream_model: &str,
    stream: bool,
    credential: &str,
) -> Result<String, UpstreamError> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(UpstreamError::Build("api_base_missing".into()));
    }
    let template = endpoint_label(route, stream);
    if !safe_upstream_path(&template) {
        return Err(UpstreamError::Build("advanced_routes_upstream_path".into()));
    }
    let encoded_model = crate::gemini::path_segment(upstream_model);
    let rendered_path = template.replace("{model}", &encoded_model);
    let mut url = format!("{base}{rendered_path}");
    if let Some(qp) = route
        .auth
        .as_ref()
        .and_then(|a| a.query_param.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let sep = if url.contains('?') { '&' } else { '?' };
        let encoded_cred = crate::aws_sigv4::uri_encode(credential);
        url.push(sep);
        url.push_str(qp);
        url.push('=');
        url.push_str(&encoded_cred);
    }
    Ok(url)
}

/// 解析本次声明式路由实际发送的鉴权 / 协议必需头。
#[must_use]
pub fn resolve_auth_headers(
    route: &AdvancedRouteDto,
    target: ApiFormat,
    credential: &str,
    outbound: &Outbound,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    let uses_query = route
        .auth
        .as_ref()
        .and_then(|a| a.query_param.as_deref())
        .is_some_and(|s| !s.trim().is_empty());

    if !uses_query {
        if let Some(auth) = route.auth.as_ref() {
            let name = auth
                .header
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default_auth_header(target));
            let prefix = auth.prefix.as_deref().unwrap_or_else(|| {
                if name.eq_ignore_ascii_case("authorization") {
                    "Bearer "
                } else {
                    ""
                }
            });
            headers.push((name.to_owned(), format!("{prefix}{credential}")));
        } else {
            let name = default_auth_header(target);
            let value = if name == "authorization" {
                format!("Bearer {credential}")
            } else {
                credential.to_owned()
            };
            headers.push((name.to_owned(), value));
        }
    }

    if target == ApiFormat::Messages
        && !outbound
            .extra_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("anthropic-version"))
    {
        headers.push((
            "anthropic-version".to_owned(),
            crate::anthropic::ANTHROPIC_VERSION.to_owned(),
        ));
    }
    headers
}

const fn default_auth_header(target: ApiFormat) -> &'static str {
    match target {
        ApiFormat::Messages => "x-api-key",
        ApiFormat::Gemini => "x-goog-api-key",
        ApiFormat::ChatCompletions | ApiFormat::Responses | ApiFormat::ResponsesCompact => {
            "authorization"
        }
    }
}

/// 按声明式路由把请求发往自定义上游路径，并按目标协议格式解析响应流/JSON。
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    http: &HttpPool,
    route: &AdvancedRouteDto,
    target: ApiFormat,
    base: &str,
    upstream_model: &str,
    credential: &str,
    body: Bytes,
    stream: bool,
    outbound: &Outbound,
) -> Result<InferenceResponse, UpstreamError> {
    let url = render_upstream_url(base, route, upstream_model, stream, credential)?;
    let owned_headers = resolve_auth_headers(route, target, credential, outbound);
    let header_refs: Vec<(&str, &str)> = owned_headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    match target {
        ApiFormat::ChatCompletions => Ok(InferenceResponse::OpenAi(
            crate::openai::send_chat_at(http, url, &header_refs, body, stream, outbound).await?,
        )),
        ApiFormat::Messages => Ok(InferenceResponse::Messages(
            crate::anthropic::send_messages_at(http, url, &header_refs, body, stream, outbound)
                .await?,
        )),
        ApiFormat::Gemini => Ok(InferenceResponse::Gemini(
            crate::gemini::send_generate_at_headers(http, url, &header_refs, body, stream, outbound)
                .await?,
        )),
        ApiFormat::Responses => Ok(InferenceResponse::OpenAi(
            crate::responses::send_responses_at(http, url, &header_refs, body, stream, outbound)
                .await?,
        )),
        ApiFormat::ResponsesCompact => Ok(InferenceResponse::OpenAi(
            crate::responses::send_compact_at(http, url, &header_refs, body, outbound).await?,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_safe_routes_and_rejects_unsafe_or_mismatched_configs() {
        let valid = json!([
            {
                "incoming_path": "/v1/chat/completions",
                "upstream_path": "/api/v2/chat",
                "converter": "openai_to_anthropic",
                "models": ["claude-*"],
                "auth": {"header": "X-Custom-Key", "prefix": "Token "}
            },
            {
                "incoming_path": "/v1beta/models/{model}:generateContent",
                "upstream_path": "/custom/models/{model}:generateContent",
                "stream_upstream_path": "/custom/models/{model}:streamGenerateContent?alt=sse",
                "converter": "passthrough",
                "auth": {"query_param": "key"}
            }
        ]);
        assert_eq!(validate_routes_value("openai_compat", &valid), Ok(()));
        assert_eq!(
            validate_routes_value("bedrock", &valid),
            Err("advanced_routes_provider_unsupported")
        );

        // 路径遍历 / 绝对 URL 逃逸必须拒绝
        for bad_path in ["https://evil.com/x", "//evil.com/x", "/v1/../secret", "no-slash"] {
            let bad = json!([{
                "incoming_path": "/v1/chat/completions",
                "upstream_path": bad_path
            }]);
            assert_eq!(
                validate_routes_value("openai_compat", &bad),
                Err("advanced_routes_upstream_path"),
                "path `{bad_path}` must be rejected"
            );
        }

        // 转换器入口与 incoming_path 矛盾必须拒绝
        let mismatched = json!([{
            "incoming_path": "/v1/messages",
            "upstream_path": "/v1/chat/completions",
            "converter": "openai_to_anthropic"
        }]);
        assert_eq!(
            validate_routes_value("openai_compat", &mismatched),
            Err("advanced_routes_converter_mismatch")
        );

        // pass_through_body=true 与跨方言转换器冲突必须拒绝
        let conflict = json!([{
            "incoming_path": "/v1/chat/completions",
            "upstream_path": "/custom",
            "converter": "openai_to_anthropic",
            "pass_through_body": true
        }]);
        assert_eq!(
            validate_routes_value("openai_compat", &conflict),
            Err("advanced_routes_pass_through_conflict")
        );

        // 禁止覆写 host 等受保护头
        let bad_header = json!([{
            "incoming_path": "/v1/chat/completions",
            "upstream_path": "/custom",
            "auth": {"header": "Host"}
        }]);
        assert_eq!(
            validate_routes_value("openai_compat", &bad_header),
            Err("advanced_routes_auth_header")
        );
    }

    #[test]
    fn matches_routes_by_path_model_and_gemini_stream_equivalence() {
        let routes = json!([
            {
                "incoming_path": "/v1/chat/completions",
                "upstream_path": "/anthropic/v1/messages",
                "converter": "openai_to_anthropic",
                "models": ["claude-*"]
            },
            {
                "incoming_path": "/v1beta/models/{model}:streamGenerateContent",
                "upstream_path": "/v1/models/{model}:generateContent",
                "stream_upstream_path": "/v1/models/{model}:streamGenerateContent?alt=sse"
            },
            {
                "incoming_path": "*",
                "upstream_path": "/fallback/{model}",
                "converter": "passthrough"
            }
        ]);

        let m1 = match_route(
            &routes,
            ApiFormat::ChatCompletions,
            "claude-sonnet-4",
            "claude-sonnet-4",
        )
        .expect("matches rule 0");
        assert_eq!(m1.index, 0);
        assert_eq!(m1.converter.name, "openai_to_anthropic");
        assert_eq!(m1.converter.to, ApiFormat::Messages);

        // 非 claude 模型跳过规则 0，命中通配规则 2
        let m2 = match_route(&routes, ApiFormat::ChatCompletions, "gpt-5", "gpt-5")
            .expect("matches fallback rule 2");
        assert_eq!(m2.index, 2);
        assert_eq!(m2.converter.name, "openai_passthrough");

        // Gemini :streamGenerateContent 规则同时匹配 Gemini 入口
        let m3 = match_route(&routes, ApiFormat::Gemini, "gemini-2.5-pro", "gemini-2.5-pro")
            .expect("matches gemini rule 1");
        assert_eq!(m3.index, 1);
        assert_eq!(m3.converter.name, "gemini_passthrough");
    }

    #[test]
    fn renders_url_with_encoded_model_and_custom_auth() {
        let route: AdvancedRouteDto = serde_json::from_value(json!({
            "incoming_path": "/v1beta/models/{model}:generateContent",
            "upstream_path": "/v1/models/{model}:generateContent",
            "stream_upstream_path": "/v1/models/{model}:streamGenerateContent?alt=sse",
            "auth": {"query_param": "api_key"}
        }))
        .expect("dto");

        let url = render_upstream_url(
            "https://gw.example.com/proxy/",
            &route,
            "gemini/2.5?x",
            true,
            "sk+123",
        )
        .expect("url");
        assert_eq!(
            url,
            "https://gw.example.com/proxy/v1/models/gemini%2F2.5%3Fx:streamGenerateContent?alt=sse&api_key=sk%2B123"
        );
        let headers = resolve_auth_headers(&route, ApiFormat::Gemini, "sk+123", &Outbound::default());
        assert_eq!(headers, Vec::<(String, String)>::new());

        let msg_route: AdvancedRouteDto = serde_json::from_value(json!({
            "incoming_path": "/v1/chat/completions",
            "upstream_path": "/custom/messages",
            "converter": "openai_to_anthropic",
            "auth": {"header": "X-Api-Token", "prefix": "Key "}
        }))
        .expect("dto");
        let msg_headers =
            resolve_auth_headers(&msg_route, ApiFormat::Messages, "secret", &Outbound::default());
        assert_eq!(
            msg_headers,
            vec![
                ("X-Api-Token".to_owned(), "Key secret".to_owned()),
                (
                    "anthropic-version".to_owned(),
                    crate::anthropic::ANTHROPIC_VERSION.to_owned()
                ),
            ]
        );
    }
}
