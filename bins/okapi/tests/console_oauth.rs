//! OAuth 通用模块验收（§6.4）：授权跳转（state 落 Redis）→ mock IdP 回调
//! → 换 token → userinfo → 首登注册 + 绑定 → session → 兑 key；
//! 二次登录复用同一用户；state 重放拒绝。依赖 .env（scripts/dev-deps.sh up）。

use axum::response::IntoResponse;
use axum::routing::{get, post};
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

/// mock IdP：token 端点校验 code/client；userinfo 返回固定主体。
fn mock_idp(subject: String) -> axum::Router {
    axum::Router::new()
        .route(
            "/oauth/token",
            post(move |body: String| async move {
                assert!(body.contains("grant_type=authorization_code"));
                assert!(body.contains("code=mock-code"));
                assert!(body.contains("client_id=cid-1"));
                assert!(body.contains("client_secret=sec-1"));
                axum::Json(json!({"access_token": "at-1", "token_type": "bearer"}))
            }),
        )
        .route(
            "/api/user",
            get(move |headers: axum::http::HeaderMap| {
                let subject = subject.clone();
                async move {
                    assert_eq!(
                        headers.get("authorization").and_then(|v| v.to_str().ok()),
                        Some("Bearer at-1")
                    );
                    axum::Json(json!({"id": subject, "username": "octocat"})).into_response()
                }
            }),
        )
}

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
}

async fn setup(subject: &str) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let idp = mock_idp(subject.to_owned());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let idp_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, idp).await.unwrap();
    });

    // 配置驱动：自定义三 URL（等价于任意标准 OAuth2 上游）
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('oauth_providers', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!([{
            "code": "mockhub",
            "client_id": "cid-1",
            "client_secret": "sec-1",
            "authorize_url": format!("http://{idp_addr}/oauth/authorize"),
            "token_url": format!("http://{idp_addr}/oauth/token"),
            "userinfo_url": format!("http://{idp_addr}/api/user"),
            "scopes": "read",
            "subject_field": "id",
            "display_field": "username",
        }])
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestEnv { pg, addr }
}

/// 完整授权码流：start 302 → callback → cookie → 兑 key；幂等绑定；state 一次性。
#[tokio::test]
// 端到端场景脚本：授权码流全链一体，拆分割裂时序
#[allow(clippy::too_many_lines)]
async fn oauth_authorization_code_flow() {
    let subject = format!("sub-{}", Uuid::new_v4().simple());
    let env = setup(&subject).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // start：302 + state 参数
    let start = client
        .get(format!("http://{}/auth/oauth/mockhub", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 302);
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_owned();
    assert!(location.contains("client_id=cid-1"));
    assert!(location.contains("/oauth/authorize"));
    let state_token = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_owned();

    // callback：换 token → userinfo → 注册 + session
    let cb = client
        .get(format!(
            "http://{}/auth/oauth/mockhub/callback?code=mock-code&state={state_token}",
            env.addr
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(cb.status(), 302, "{:?}", cb.text().await);
    let cookie = cb
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .expect("回调必须发会话")
        .to_owned();

    // 用户与绑定已建
    let bound = sqlx::query!(
        r#"SELECT user_id, display FROM oauth_identities WHERE provider = 'mockhub' AND subject = $1"#,
        subject
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(bound.display.as_deref(), Some("octocat"));

    // session 兑 key → 门户可用
    let key_resp: Value = client
        .post(format!("http://{}/auth/keys", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({"name": "oauth"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let me = client
        .get(format!("http://{}/api/me", env.addr))
        .bearer_auth(key_resp["api_key"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);
    let me: Value = me.json().await.unwrap();
    assert_eq!(me["user_id"].as_i64(), Some(bound.user_id));

    // state 重放拒绝（一次性键已销毁）
    let replay = client
        .get(format!(
            "http://{}/auth/oauth/mockhub/callback?code=mock-code&state={state_token}",
            env.addr
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 401);

    // 二次完整登录：复用同一用户（不重复注册）
    let start2 = client
        .get(format!("http://{}/auth/oauth/mockhub", env.addr))
        .send()
        .await
        .unwrap();
    let loc2 = start2
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_owned();
    let state2 = loc2
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let cb2 = client
        .get(format!(
            "http://{}/auth/oauth/mockhub/callback?code=mock-code&state={state2}",
            env.addr
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(cb2.status(), 302);
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM oauth_identities
           WHERE provider = 'mockhub' AND subject = $1"#,
        subject
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(count, 1, "同 subject 不得重复注册");

    // 未配置的 provider：404
    let unknown = client
        .get(format!("http://{}/auth/oauth/nonexistent", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
}
