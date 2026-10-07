//! 全路由门面探测：清单从当前源码生成，公开接口必须显式声明。
//! API_PROBE 行区分鉴权拒绝、参数拒绝、HEAD 状态检查，不把 400 当作权限已验证。
//! 各业务套件仍负责有效请求、归属和副作用；本文件不宣称业务全覆盖。
//! 需要 Python 3（与 CI 静态守卫一致）、DATABASE_URL 和 OKAPI_REDIS_URL。

use okapi::{console, gateway};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

#[derive(Deserialize)]
struct Endpoint {
    surface: String,
    method: String,
    path: String,
}

fn routes_from_source() -> Vec<Endpoint> {
    #[derive(Deserialize)]
    struct Inventory {
        endpoints: Vec<Endpoint>,
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/api-surface.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .output()
        .expect("API inventory requires Python 3");
    assert!(
        output.status.success(),
        "API inventory failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory: Inventory = serde_json::from_slice(&output.stdout).expect("valid API inventory");
    assert!(
        inventory.endpoints.len() > 150,
        "API inventory unexpectedly small"
    );
    inventory.endpoints
}

fn fill_params(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}').expect("closed path parameter");
        let name = &rest[open + 1..open + close];
        out.push_str(if name == "model_action" {
            "audit-model:generateContent"
        } else if name.contains("uuid") || name.contains("request") || name.contains("batch") {
            "00000000-0000-0000-0000-000000000000"
        } else if name.contains("id") || name == "key" {
            "1"
        } else {
            "probe"
        });
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

fn public_endpoint(endpoint: &Endpoint) -> bool {
    let method = if endpoint.method == "HEAD" {
        "GET"
    } else {
        &endpoint.method
    };
    if endpoint.path == "/healthz" && method == "GET" {
        return true;
    }
    // 既有公开目录契约，成功内容由 gateway_models_list / gateway_gemini_ingress 验证。
    if endpoint.surface == "gateway"
        && method == "GET"
        && matches!(endpoint.path.as_str(), "/v1/models" | "/v1beta/models")
    {
        return true;
    }
    endpoint.surface == "console"
        && matches!(
            (method, endpoint.path.as_str()),
            (
                "GET",
                "/api/pricing"
                    | "/api/pricing/models"
                    | "/api/pricing/groups"
                    | "/api/pricing/stats"
                    | "/api/notice"
                    | "/api/registration"
                    | "/api/setup/status"
                    | "/api/plans"
                    | "/api/playground/presets"
                    | "/auth/oauth-providers"
                    | "/auth/oauth/{provider}"
                    | "/auth/oauth/{provider}/callback"
                    | "/pay/callback/epay"
            ) | (
                "POST",
                "/api/setup"
                    | "/auth/register"
                    | "/auth/email-code"
                    | "/auth/password/forgot"
                    | "/auth/password/reset"
                    | "/auth/login"
                    | "/auth/logout"
                    | "/pay/callback/stripe"
            )
        )
}

fn probe_request(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    addr: SocketAddr,
) -> reqwest::RequestBuilder {
    let mut path = fill_params(&endpoint.path);
    let websocket = endpoint.surface == "gateway" && endpoint.path == "/v1/realtime";
    if websocket || endpoint.path == "/admin/diagnose/route" {
        path.push_str("?model=audit-model");
    } else if endpoint.path == "/admin/stats/entity-usage" {
        path.push_str("?kind=user&ids=1");
    }
    let mut request = client.request(
        endpoint.method.parse().unwrap(),
        format!("http://{addr}{path}"),
    );
    if websocket {
        request = request
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
    }
    let upload = match endpoint.path.as_str() {
        "/v1/audio/transcriptions" | "/v1/audio/translations" => {
            Some(("file", "audit.wav", "audio/wav"))
        }
        "/v1/images/edits" => Some(("image", "audit.png", "image/png")),
        _ => None,
    };
    if let Some((field, filename, mime)) = upload {
        // 正确的 multipart 结构才会越过提取器；无凭证应在读取文件内容前被拒绝。
        let part = reqwest::multipart::Part::bytes(vec![0; 16])
            .file_name(filename)
            .mime_str(mime)
            .unwrap();
        request = request.multipart(
            reqwest::multipart::Form::new()
                .text("model", "audit-model")
                .part(field, part),
        );
    } else if matches!(endpoint.method.as_str(), "POST" | "PUT" | "PATCH") {
        let body = if endpoint.surface == "gateway" {
            json!({"model": "audit-model", "input": "audit", "prompt": "audit", "voice": "alloy",
                "messages": [{"role": "user", "content": "audit"}], "max_tokens": 1,
                "contents": [{"parts": [{"text": "audit"}]}]})
        } else {
            console_probe_body(&endpoint.path)
        };
        request = request.json(&body);
    }
    request.header("accept", "application/json")
}

// 必填字段必须能反序列化，才能真正进入权限闸；空 {} 造成的 400 不算 RBAC 证据。
fn console_probe_body(path: &str) -> Value {
    match path {
        "/api/me/playground/chat" => {
            json!({"model": "audit-model", "messages": [{"role": "user", "content": "audit"}]})
        }
        "/api/me/redeem" => json!({"code": "audit-fixture"}),
        "/api/me/profile" => json!({"username": "audit", "language": "auto"}),
        "/api/me/subscriptions/checkout" => json!({"plan_code": "audit", "gateway": "stripe"}),
        "/api/me/topup" => json!({"amount_micro": 1_000_000, "gateway": "stripe"}),
        "/api/teams" => json!({"name": "audit"}),
        "/api/teams/{id}/members" | "/admin/reconciliation/repair" => json!({"user_id": 1}),
        "/auth/totp/confirm" => json!({"pending": "00", "code": "000000"}),
        "/auth/totp/enroll" => json!({"password": "audit-fixture"}),
        "/auth/totp/disable" => json!({"password": "audit-fixture", "code": "000000"}),
        "/admin/billing/refund" => json!({"request_id": "00000000-0000-0000-0000-000000000000"}),
        "/admin/cache/flush" => json!({"scope": "auth"}),
        "/admin/channels" => {
            json!({"name": "audit", "api_base": "http://127.0.0.1:9/v1", "credential": "audit-fixture", "models": ["audit-model"]})
        }
        "/admin/channels/batch" => json!({"ids": [1], "action": "enable"}),
        "/admin/channels/oauth/start" => json!({"provider": "codex"}),
        "/admin/channels/oauth/exchange" => json!({"state": "audit", "code": "audit"}),
        "/admin/channels/{id}/credential" => json!({"credential": "audit-fixture"}),
        "/admin/channels/{id}/duplicate" => json!({"name": "audit-copy"}),
        "/admin/channels/{id}/pools" => json!({"pools": ["default"]}),
        "/admin/channels/{id}/status" => json!({"status": 1}),
        "/admin/dlq/requeue" | "/admin/dlq/discard" => json!({"ids": [1]}),
        "/admin/groups" => json!({"group_code": "audit", "group_ratio": "1"}),
        "/admin/margin-breaker/lift" => json!({"group_code": "default", "channel_id": 1}),
        "/admin/models" => json!({"model_name": "audit-model", "model_ratio": "1"}),
        "/admin/plans" => json!({"plan_code": "audit", "display_name": "Audit", "grant_micro": 1}),
        "/admin/pools" => json!({"pool_code": "audit"}),
        "/admin/pricing/rules" => {
            json!({"rule_code": "audit", "rule_type": "discount", "multiplier": "1"})
        }
        "/admin/pricing/rules/{code}/toggle" => json!({"enabled": false}),
        "/admin/pricing/sync/apply" => json!({"changes": []}),
        "/admin/pricing/sync/fetch" => json!({"sources": []}),
        "/admin/redemptions" => json!({"count": 1, "amount_micro": 1}),
        "/admin/roles" => json!({"role_code": "audit", "display_name": "Audit", "permissions": []}),
        "/admin/settings" => json!({"key": "api_audit_fixture", "value": false}),
        "/admin/settings/smtp/test" => json!({"to": "nobody@example.invalid"}),
        "/admin/users/{id}/balance-expiry" => json!({"expires_at": null}),
        "/admin/users/{id}/credit" => json!({"amount_micro": 1}),
        "/admin/users/{id}/groups" => json!({"groups": []}),
        "/admin/users/{id}/manage" => json!({"action": "ban"}),
        "/admin/users/{id}/multiplier" => json!({"multiplier": "1"}),
        "/admin/users/{id}/subscription" => json!({"plan_code": "audit"}),
        "/admin/channels/{id}/egress" | "/admin/egress/default" => json!({"mode": "direct"}),
        "/admin/proxies" | "/admin/proxies/test" => json!({"url": "http://127.0.0.1:9"}),
        "/admin/proxies/import" => json!({"text": "127.0.0.1:9"}),
        "/admin/proxy-groups" => json!({"code": "audit", "mode": "pinned"}),
        "/admin/proxy-groups/{code}/assignments" => json!({"key_id": 1, "proxy_id": 1}),
        _ => json!({}),
    }
}

fn error_tag(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = &value["error"];
    error["code"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| error["type"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| error["status"].as_str().filter(|s| !s.is_empty()))
        .map(str::to_owned)
        .or_else(|| error["code"].as_i64().map(|n| n.to_string()))
}

fn report(endpoint: &Endpoint, phase: &str, status: u16, evidence: &str, code: Option<&str>) {
    println!(
        "API_PROBE {}",
        json!({"surface": endpoint.surface, "method": endpoint.method,
        "path": endpoint.path, "phase": phase, "status": status, "evidence": evidence, "code": code})
    );
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn serve(router: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

#[tokio::test]
async fn every_route_returns_a_well_formed_error_envelope_without_auth() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "envelope-node", None, None)
        .await
        .unwrap();
    // 单个探针进程会连续产生超过 60 次无凭证请求。只在该 AppState 的缓存关闭此项
    // 限流，不写共享库；否则后半清单只测到 429。限流本身由 gateway_invalid_key_rate 验证。
    state
        .settings_cache
        .insert(
            "critical_rate_limits".to_owned(),
            std::sync::Arc::new(Some(json!({"invalid_api_key": 0}))),
        )
        .await;
    let gw = serve(gateway::router(state.clone())).await;
    let cs = serve(console::router(state)).await;
    let client = client();
    let mut problems = Vec::new();
    for endpoint in routes_from_source() {
        let addr = if endpoint.surface == "console" {
            cs
        } else {
            gw
        };
        let resp = probe_request(&client, &endpoint, addr).send().await;
        let Ok(resp) = resp else {
            problems.push(format!(
                "{} {} {}: transport failed: {resp:?}",
                endpoint.surface, endpoint.method, endpoint.path
            ));
            continue;
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap();
        let code = error_tag(&body);
        let success = (200..400).contains(&status);
        // HTTP/1 CONNECT 使用 authority-form，不包含 /pass 路径；这是传输拒绝，
        // 不是透传路由鉴权证据。HEAD 虽被 Axum 隐式注册，WS 提取器只接受 GET。
        let connect = endpoint.surface == "gateway"
            && endpoint.method == "CONNECT"
            && endpoint.path == "/pass/{channel_id}/{*path}";
        let websocket_head = endpoint.surface == "gateway"
            && endpoint.method == "HEAD"
            && endpoint.path == "/v1/realtime";
        let evidence = if connect {
            "connect_authority_form_rejected"
        } else if websocket_head {
            "websocket_requires_get"
        } else if success && public_endpoint(&endpoint) {
            "public_contract"
        } else if success {
            "unexpected_anonymous_success"
        } else if endpoint.method == "HEAD" {
            "head_status_only"
        } else if matches!(status, 401 | 403) {
            "authentication_denied"
        } else {
            "error_envelope_only"
        };
        report(&endpoint, "anonymous", status, evidence, code.as_deref());
        let bad = if connect {
            status != 404 || !body.is_empty()
        } else if websocket_head {
            status != 405 || !body.is_empty()
        } else {
            (!public_endpoint(&endpoint) && !matches!(status, 401 | 403))
                || status == 405
                || status == 500
                || (status >= 500 && code.as_deref().is_some_and(|v| v.starts_with("internal")))
                || (!success && endpoint.method != "HEAD" && code.is_none())
        };
        if bad {
            problems.push(format!(
                "{} {} {} -> {status}: {}",
                endpoint.surface,
                endpoint.method,
                endpoint.path,
                body.chars().take(160).collect::<String>()
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "Invalid anonymous/API surface responses:\n{}",
        problems.join("\n")
    );
}

async fn plain_user_token(pg: &sqlx::PgPool) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..10].to_owned();
    let user_id = okapi_store::provision::create_user(pg, &format!("probe-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-probe-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(pg, user_id, &hash, "sk-probe")
        .await
        .unwrap();
    token
}

/// 每个管理接口都必须返回 403；参数错误、资源不存在不能替代权限拒绝。
#[tokio::test]
async fn admin_routes_never_succeed_for_an_authenticated_unprivileged_user() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let token = plain_user_token(&pg).await;
    let state = gateway::build_state(&database_url, &redis_url, "rbac-probe-node", None, None)
        .await
        .unwrap();
    let cs = serve(console::router(state)).await;
    let client = client();
    assert_eq!(
        client
            .get(format!("http://{cs}/api/me"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "普通用户 fixture 必须确实能通过鉴权"
    );
    let mut problems = Vec::new();
    for endpoint in routes_from_source()
        .into_iter()
        .filter(|e| e.surface == "console" && e.path.starts_with("/admin/"))
    {
        let resp = probe_request(&client, &endpoint, cs)
            .bearer_auth(&token)
            .send()
            .await;
        let Ok(resp) = resp else {
            problems.push(format!(
                "{} {}: transport failed: {resp:?}",
                endpoint.method, endpoint.path
            ));
            continue;
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap();
        let code = error_tag(&body);
        let denied = status == 403;
        report(
            &endpoint,
            "unprivileged",
            status,
            if denied {
                "permission_denied"
            } else {
                "permission_not_proven"
            },
            code.as_deref(),
        );
        if !denied || (endpoint.method != "HEAD" && code.as_deref() != Some("permission_denied")) {
            problems.push(format!(
                "{} {} -> {status}: {}",
                endpoint.method,
                endpoint.path,
                body.chars().take(160).collect::<String>()
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "Invalid unprivileged responses:\n{}",
        problems.join("\n")
    );
}
