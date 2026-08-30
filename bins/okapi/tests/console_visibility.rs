//! M2 收尾批验收：own/all 渠道属主范围（#6267）+ 分组可见性矩阵与分组定价（§6.3）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    use std::fmt::Write as _;
    let chunks = [
        json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
        json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":0}}}),
    ];
    let mut body = String::new();
    for c in chunks {
        let _ = write!(body, "data: {c}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

struct Env {
    pg: PgPool,
    state: gateway::state::AppState,
    console: SocketAddr,
    gateway: SocketAddr,
    mock: SocketAddr,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('ssrf_policy', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        serde_json::json!({"allow_http": true, "allow_private": true})
    )
    .execute(&pg)
    .await
    .unwrap();
    // 本测试会切换全局 strict 开关，先归位（并行测试互不干扰依赖宽松默认）
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('strict_group_isolation', 'false'::jsonb)
           ON CONFLICT (key) DO UPDATE SET value = 'false'::jsonb"#
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let console = serve(console::router(state.clone())).await;
    let gw = serve(gateway::router(state.clone())).await;
    let mock = serve(Router::new().route("/ok/v1/chat/completions", post(mock_ok))).await;
    Env {
        pg,
        state,
        console,
        gateway: gw,
        mock,
    }
}

async fn mk_user(pg: &PgPool, role: i16, admin_role_id: Option<i64>) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(pg, &format!("v-{suffix}"))
        .await
        .unwrap();
    sqlx::query!(
        "UPDATE users SET role = $2, admin_role_id = $3 WHERE id = $1",
        user_id,
        role,
        admin_role_id
    )
    .execute(pg)
    .await
    .unwrap();
    let token = format!("sk-okapi-vis-{suffix}");
    okapi_store::provision::create_api_key(pg, user_id, &hash(&token), "sk-vis")
        .await
        .unwrap();
    (user_id, token)
}

async fn cpost(env: &Env, token: &str, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn cget(env: &Env, token: &str, path: &str) -> Value {
    reqwest::Client::new()
        .get(format!("http://{}{path}", env.console))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn chat(env: &Env, token: &str, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(token)
        .json(&json!({
            "model": model, "stream": true, "max_tokens": 32,
            "messages": [{"role":"user","content":"hello visibility"}]
        }))
        .send()
        .await
        .unwrap()
}

async fn settled_amount(pg: &PgPool, resp: reqwest::Response) -> i64 {
    let request_id = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap();
    let _ = resp.text().await.unwrap();
    for _ in 0..50 {
        if let Some(r) = sqlx::query!(
            r#"SELECT amount_micro FROM billing_records WHERE request_id = $1"#,
            request_id
        )
        .fetch_optional(pg)
        .await
        .unwrap()
        {
            return r.amount_micro;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("结算未出现");
}

/// own 范围：channel_admin 只能看到/操作自己创建的渠道。
#[tokio::test]
async fn own_scope_isolates_channel_admins() {
    let env = setup().await;
    let (_, super_token) = mk_user(&env.pg, 100, None).await;
    let code = format!("chadmin-{}", Uuid::new_v4().simple());
    let r = cpost(
        &env,
        &super_token,
        "/admin/roles",
        json!({"role_code": code, "display_name": "渠道管理员",
               "permissions": ["channel.read.own", "channel.write.own"]}),
    )
    .await;
    let role_id = r.json::<Value>().await.unwrap()["admin_role_id"]
        .as_i64()
        .unwrap();
    let (x_id, x_token) = mk_user(&env.pg, 10, Some(role_id)).await;
    let (_, y_token) = mk_user(&env.pg, 10, Some(role_id)).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let r = cpost(
        &env,
        &x_token,
        "/admin/channels",
        json!({"name": format!("own-{suffix}"), "api_base": "http://127.0.0.1:9/v1",
               "credential": "c", "models": [format!("m-own-{suffix}")]}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let channel_id = r.json::<Value>().await.unwrap()["channel_id"]
        .as_i64()
        .unwrap();

    // 属主传播
    let owner = sqlx::query_scalar!(r#"SELECT owner_id FROM channels WHERE id = $1"#, channel_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(owner, Some(x_id), "创建人即属主");

    // X 可见，Y 不可见，super 全见
    let in_list = |body: &Value, id: i64| {
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"].as_i64() == Some(id))
    };
    assert!(in_list(
        &cget(&env, &x_token, "/admin/channels").await,
        channel_id
    ));
    assert!(!in_list(
        &cget(&env, &y_token, "/admin/channels").await,
        channel_id
    ));
    assert!(in_list(
        &cget(&env, &super_token, "/admin/channels").await,
        channel_id
    ));

    // Y 改不动，X 改得动
    let r = cpost(
        &env,
        &y_token,
        &format!("/admin/channels/{channel_id}/status"),
        json!({"status": 2}),
    )
    .await;
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["param"], "owner");
    let r = cpost(
        &env,
        &x_token,
        &format!("/admin/channels/{channel_id}/status"),
        json!({"status": 2}),
    )
    .await;
    assert_eq!(r.status(), 200);
}

/// 分组可见性 + 分组定价：绑定组渠道仅组内可用且按组倍率计价；严格模式未绑定即不可见。
#[tokio::test]
async fn group_visibility_matrix_and_group_pricing() {
    let env = setup().await;
    let (super_id, super_token) = mk_user(&env.pg, 100, None).await;
    let suffix = Uuid::new_v4().simple().to_string();
    let vip = format!("vip-{}", &suffix[..8]);
    let m_bound = format!("m-vb-{}", &suffix[..8]);
    let m_free = format!("m-vf-{}", &suffix[..8]);

    // 组（半价）+ 两个模型 + 两条渠道（一条绑定 vip，一条不绑定）
    let r = cpost(
        &env,
        &super_token,
        "/admin/groups",
        json!({"group_code": vip, "group_ratio": "0.5"}),
    )
    .await;
    assert_eq!(r.status(), 200);
    for m in [&m_bound, &m_free] {
        let r = cpost(
            &env,
            &super_token,
            "/admin/models",
            json!({"model_name": m, "model_ratio": "1"}),
        )
        .await;
        assert_eq!(r.status(), 200);
    }
    let r = cpost(
        &env,
        &super_token,
        "/admin/channels",
        json!({"name": format!("vb-{suffix}"), "api_base": format!("http://{}/ok/v1", env.mock),
               "credential": "c", "models": [m_bound], "groups": [vip]}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let r = cpost(
        &env,
        &super_token,
        "/admin/channels",
        json!({"name": format!("vf-{suffix}"), "api_base": format!("http://{}/ok/v1", env.mock),
               "credential": "c", "models": [m_free]}),
    )
    .await;
    assert_eq!(r.status(), 200);

    // 发布（组倍率进 PriceBook）并热更
    let r = cpost(&env, &super_token, "/admin/pricing/publish", json!({})).await;
    assert_eq!(r.status(), 200);
    gateway::refresh_pricebook_if_newer(&env.state)
        .await
        .unwrap();

    // 用户：D 默认组；U 归入 vip（先分组再首次鉴权，避开缓存）
    let (d_id, d_token) = mk_user(&env.pg, 1, None).await;
    let (u_id, u_token) = mk_user(&env.pg, 1, None).await;
    let r = cpost(
        &env,
        &super_token,
        &format!("/admin/users/{u_id}/groups"),
        json!({"groups": [{"group_code": vip, "priority": 10}]}),
    )
    .await;
    assert_eq!(r.status(), 200);
    for uid in [d_id, u_id] {
        env.state
            .ledger
            .credit(uid, Money::from_micros(1_000_000))
            .await
            .unwrap();
    }

    // 宽松模式：绑定渠道仅 vip 可用；未绑定渠道人人可用
    let resp = chat(&env, &d_token, &m_bound).await;
    assert_eq!(resp.status(), 503, "默认组不应看见 vip 绑定渠道");
    let resp = chat(&env, &u_token, &m_bound).await;
    assert_eq!(resp.status(), 200, "vip 用户应可用绑定渠道");
    let amount = settled_amount(&env.pg, resp).await;
    assert_eq!(amount, 120, "vip 组倍率 0.5：240 → 120");
    let resp = chat(&env, &d_token, &m_free).await;
    assert_eq!(resp.status(), 200, "宽松模式未绑定渠道全可见");
    let _ = settled_amount(&env.pg, resp).await;

    // 严格模式：未绑定渠道对所有人不可见
    let r = cpost(
        &env,
        &super_token,
        "/admin/settings",
        json!({"key": "strict_group_isolation", "value": true}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let resp = chat(&env, &d_token, &m_free).await;
    assert_eq!(resp.status(), 503, "严格模式未绑定渠道不可见");
    let resp = chat(&env, &u_token, &m_bound).await;
    assert_eq!(resp.status(), 200, "严格模式绑定组内仍可用");
    let _ = resp.text().await;

    // 归位宽松，避免影响并行/后续测试
    let r = cpost(
        &env,
        &super_token,
        "/admin/settings",
        json!({"key": "strict_group_isolation", "value": false}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let _ = super_id;
}
