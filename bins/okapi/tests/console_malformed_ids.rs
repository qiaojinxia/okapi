//! 畸形 id / 请求体不得换来 5xx：把垃圾的 request_id、路径 id、id 列表和请求体打向每个接受它们的
//! 管理端 / 门户入口，回的必须是调用方的错误（400 / 404）或正常的空结果，不能是服务端错误。
//! 起因：管理端日志过滤把垃圾 request_id 原样交给 ClickHouse，CH 的 400 被映射成 500 + error 日志。
//! （`/admin/logs` 本身要 ClickHouse，它的 400 用例在 `console_logs.rs`。）

use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Env {
    addr: SocketAddr,
    super_token: String,
    user_token: String,
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let super_id = okapi_store::provision::create_user(&pg, &format!("mi-s-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-mi-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-mi-s")
        .await
        .unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("mi-u-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-mi-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-mi-u")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = console::router(state);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Env {
        addr,
        super_token,
        user_token,
    }
}

/// 一次探测：谁来打、打什么；`expect` 给了就钉死状态码，没给就只要求不是服务端错误。
struct Probe {
    admin: bool,
    method: &'static str,
    path: &'static str,
    body: Option<Value>,
    expect: Option<u16>,
}

fn get(admin: bool, path: &'static str, expect: Option<u16>) -> Probe {
    Probe {
        admin,
        method: "GET",
        path,
        body: None,
        expect,
    }
}

fn post(path: &'static str, body: Value, expect: Option<u16>) -> Probe {
    Probe {
        admin: true,
        method: "POST",
        path,
        body: Some(body),
        expect,
    }
}

/// 打向哪些入口、期望什么：钉死状态码的是形状明确的那几类，其余只要求不是服务端错误。
fn probes() -> Vec<Probe> {
    let nil = "00000000-0000-0000-0000-000000000000";
    vec![
        // 门户日志：request_id 是 UUID 类型的查询参数
        get(false, "/api/me/logs?request_id=garbage", Some(400)),
        get(false, "/api/me/logs?request_id=", Some(400)),
        get(false, "/api/me/logs/stat?request_id=garbage", Some(400)),
        get(false, "/api/me/logs/series?request_id=garbage", None),
        get(false, "/api/me/logs?start_date=garbage", Some(400)),
        get(false, "/api/me/logs?end_date=2026-13-45", Some(400)),
        get(false, "/api/me/logs?limit=-5&offset=-9", None),
        get(false, "/api/me/logs?limit=99999999999999999999", Some(400)),
        // 路径里的 id
        get(true, "/admin/users/abc/overview", Some(400)),
        get(true, "/admin/users/-1/overview", Some(404)),
        get(
            true,
            "/admin/users/99999999999999999999/overview",
            Some(400),
        ),
        get(true, "/admin/users/abc/usage", Some(400)),
        get(true, "/admin/channels/abc/usage", Some(400)),
        get(true, "/admin/channels/0/usage", Some(404)),
        get(true, "/admin/channels/9223372036854775807/usage", Some(404)),
        get(true, "/api/teams/abc/usage", Some(400)),
        // 审计与死信的游标 / 分页参数
        get(true, "/admin/audit?before=abc", Some(400)),
        get(true, "/admin/audit?from=notadate", Some(400)),
        get(true, "/admin/audit?limit=-1", None),
        get(true, "/admin/dlq?limit=-1&offset=-1", None),
        // 退款：request_id 的各种错形状
        post(
            "/admin/billing/refund",
            json!({"request_id": "garbage"}),
            Some(400),
        ),
        post(
            "/admin/billing/refund",
            json!({"request_id": 123}),
            Some(400),
        ),
        post(
            "/admin/billing/refund",
            json!({"request_id": null}),
            Some(400),
        ),
        post(
            "/admin/billing/refund",
            json!({"request_id": ["a"]}),
            Some(400),
        ),
        post("/admin/billing/refund", json!({}), Some(400)),
        post("/admin/billing/refund", json!([]), Some(400)),
        post("/admin/billing/refund", json!("str"), Some(400)),
        post(
            "/admin/billing/refund",
            json!({"request_id": nil}),
            Some(404),
        ),
        post(
            "/admin/billing/refund",
            json!({"request_id": nil, "reason": "r".repeat(100_000)}),
            None,
        ),
        // 死信重投 / 丢弃：id 列表
        post("/admin/dlq/requeue", json!({"ids": ["x"]}), Some(400)),
        post("/admin/dlq/requeue", json!({"ids": [-1]}), None),
        post(
            "/admin/dlq/requeue",
            json!({"ids": [9_223_372_036_854_775_807_i64]}),
            None,
        ),
        post("/admin/dlq/requeue", json!({"ids": [1.5]}), Some(400)),
        post("/admin/dlq/requeue", json!({"ids": []}), Some(400)),
        post("/admin/dlq/requeue", json!({}), Some(400)),
        post("/admin/dlq/discard", json!({"ids": ["x"]}), Some(400)),
        post("/admin/dlq/discard", json!({"ids": [-1]}), None),
        post("/admin/dlq/discard", json!({"ids": [0, 0, 0]}), None),
        // 管理动作：路径 id 与动作名
        post(
            "/admin/users/abc/manage",
            json!({"action": "ban"}),
            Some(400),
        ),
        post("/admin/users/1/manage", json!({"action": 123}), Some(400)),
        post(
            "/admin/users/1/manage",
            json!({"action": "nuke"}),
            Some(400),
        ),
    ]
}

#[tokio::test]
async fn malformed_ids_and_bodies_never_answer_5xx() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let mut server_errors = Vec::new();
    let mut mismatched = Vec::new();
    for probe in &probes() {
        let token = if probe.admin {
            &env.super_token
        } else {
            &env.user_token
        };
        let url = format!("http://{}{}", env.addr, probe.path);
        let request = if probe.method == "GET" {
            client.get(url)
        } else {
            client.post(url).json(probe.body.as_ref().unwrap())
        };
        let response = request.bearer_auth(token).send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let shown: String = text.chars().take(160).collect();
        let label = format!("{} {} -> {status} {shown}", probe.method, probe.path);
        if status >= 500 {
            server_errors.push(label.clone());
        }
        if probe.expect.is_some_and(|expected| expected != status) {
            mismatched.push(format!("{label} (expected {:?})", probe.expect));
        }
    }
    assert!(server_errors.is_empty(), "5xx: {server_errors:#?}");
    assert!(mismatched.is_empty(), "unexpected status: {mismatched:#?}");
}
