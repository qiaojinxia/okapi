//! OAuth 内置预设验收（§6.4，`oauth.rs::preset`）：github / discord / linuxdo 只配 client_id /
//! client_secret（这里另把三条 URL 指到本地 mock），字段名与 scopes 走预设。
//! 要钉住的是身份稳定性：绑定键必须是 IdP 的稳定 `id`（数字 / snowflake 都转成字符串），
//! 而不是可改名的 login / username——改名后仍是同一个账号，同名的另一个 id 不能顶替。
//! 独立临时库：`oauth_providers` 是全局设置，`console_oauth` 也在写它。依赖 .env。

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// 每个 provider 当前的 userinfo；token 端点是否故障。
#[derive(Clone, Default)]
struct Idp {
    userinfo: Arc<Mutex<HashMap<String, Value>>>,
    token_down: Arc<Mutex<bool>>,
}

async fn token(
    State(idp): State<Idp>,
    Path(code): Path<String>,
    body: String,
) -> axum::response::Response {
    if *idp.token_down.lock().unwrap() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    assert!(body.contains("grant_type=authorization_code"), "{body}");
    assert!(body.contains(&format!("client_id=cid-{code}")), "{body}");
    axum::Json(json!({ "access_token": format!("at-{code}"), "token_type": "bearer" }))
        .into_response()
}

async fn userinfo(
    State(idp): State<Idp>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    assert_eq!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some(format!("Bearer at-{code}").as_str())
    );
    let body = idp
        .userinfo
        .lock()
        .unwrap()
        .get(&code)
        .cloned()
        .unwrap_or(Value::Null);
    axum::Json(body).into_response()
}

struct Bed {
    pg: PgPool,
    console: SocketAddr,
    idp: Idp,
    client: reqwest::Client,
    admin_pool: PgPool,
    db_name: String,
}

impl Bed {
    /// 用完即删：临时库不清，跑一天测试就在开发 PG 里留下上百个库。
    async fn teardown(self) {
        self.pg.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
            self.db_name
        )))
        .execute(&self.admin_pool)
        .await;
    }
}

async fn bed() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let admin = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!("okapi_oauth_{}", &Uuid::new_v4().simple().to_string()[..12]);
    // 库名为本测试生成的随机标识符（无注入面）
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin)
    .await
    .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let fresh_url = format!("{base}/{db_name}");
    let pg = okapi_store::connect_pg(&fresh_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let idp = Idp::default();
    let router = Router::new()
        .route("/{code}/token", post(token))
        .route("/{code}/userinfo", get(userinfo))
        .with_state(idp.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let idp_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // 只给 client 凭证 + 三条指向 mock 的 URL；scopes / subject_field / display_field 留给预设
    let providers: Vec<Value> = ["github", "discord", "linuxdo"]
        .into_iter()
        .map(|code| {
            json!({
                "code": code,
                "client_id": format!("cid-{code}"),
                "client_secret": format!("sec-{code}"),
                "authorize_url": format!("http://{idp_addr}/{code}/authorize"),
                "token_url": format!("http://{idp_addr}/{code}/token"),
                "userinfo_url": format!("http://{idp_addr}/{code}/userinfo"),
            })
        })
        .collect();
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('oauth_providers', $1)"#,
        json!(providers)
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&fresh_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Bed {
        pg,
        console,
        idp,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        admin_pool: admin,
        db_name,
    }
}

impl Bed {
    fn set_userinfo(&self, code: &str, body: Value) {
        self.idp
            .userinfo
            .lock()
            .unwrap()
            .insert(code.to_owned(), body);
    }

    /// start → 取 state → callback；返回 (callback 状态码, Location 首段, 响应体)。
    async fn login(&self, code: &str) -> (u16, String, Value) {
        let start = self
            .client
            .get(format!("http://{}/auth/oauth/{code}", self.console))
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
        let state = location
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();
        let cb = self
            .client
            .get(format!(
                "http://{}/auth/oauth/{code}/callback?code=mock-code&state={state}",
                self.console
            ))
            .send()
            .await
            .unwrap();
        let status = cb.status().as_u16();
        let body = cb.json::<Value>().await.unwrap_or(Value::Null);
        (status, location, body)
    }

    async fn identities(&self, provider: &str) -> Vec<(String, i64, Option<String>)> {
        sqlx::query!(
            r#"SELECT subject, user_id, display FROM oauth_identities WHERE provider = $1 ORDER BY id"#,
            provider
        )
        .fetch_all(&self.pg)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.subject, r.user_id, r.display))
        .collect()
    }
}

/// 三家预设：scopes 进授权跳转；数字 / snowflake id 作 subject；login / username 作展示名。
#[tokio::test]
async fn presets_bind_on_stable_id_and_use_handle_as_display() {
    let bed = bed().await;
    bed.set_userinfo("github", json!({ "id": 583_231, "login": "octocat", "email": "octocat@github.com", "name": "The Octocat" }));
    bed.set_userinfo("discord", json!({ "id": "80351110224678912", "username": "nelly", "discriminator": "0", "global_name": "Nelly" }));
    bed.set_userinfo(
        "linuxdo",
        json!({ "id": 42, "username": "linuxfan", "name": "Fan", "trust_level": 2 }),
    );

    // `:` 是 RFC 3986 允许留在 query 里的字符，form_escape 不动它
    for (code, scope, subject, display) in [
        ("github", "read:user", "583231", "octocat"),
        ("discord", "identify", "80351110224678912", "nelly"),
        ("linuxdo", "read", "42", "linuxfan"),
    ] {
        let (status, location, body) = bed.login(code).await;
        assert_eq!(status, 302, "{code}: {body}");
        assert!(
            location.contains(&format!("scope={scope}&")),
            "{code} 预设 scopes：{location}"
        );
        assert!(
            location.contains(&format!("client_id=cid-{code}")),
            "{location}"
        );
        let rows = bed.identities(code).await;
        assert_eq!(rows.len(), 1, "{code}");
        assert_eq!(rows[0].0, subject, "{code} 绑定键必须是稳定 id");
        assert_eq!(
            rows[0].2.as_deref(),
            Some(display),
            "{code} 展示名取 handle"
        );
        let username: String =
            sqlx::query_scalar!(r#"SELECT username FROM users WHERE id = $1"#, rows[0].1)
                .fetch_one(&bed.pg)
                .await
                .unwrap();
        assert_eq!(
            username,
            format!("{code}-{display}"),
            "首登用户名 = provider-handle"
        );
    }
    bed.teardown().await;
}

/// 身份稳定性：改名不换账号；同名的另一个 id 是另一个人；缺 id 拒绝；token 端点故障拒绝。
#[tokio::test]
async fn renamed_handle_keeps_account_and_same_handle_cannot_hijack() {
    let bed = bed().await;
    bed.set_userinfo("github", json!({ "id": 1001, "login": "alice" }));
    let (status, _, _) = bed.login("github").await;
    assert_eq!(status, 302);
    let first = bed.identities("github").await;
    let alice_user = first[0].1;

    // alice 把 GitHub 用户名改成 alice-dev：仍是同一个 Okapi 用户，不新建
    bed.set_userinfo("github", json!({ "id": 1001, "login": "alice-dev" }));
    let (status, _, _) = bed.login("github").await;
    assert_eq!(status, 302);
    let after_rename = bed.identities("github").await;
    assert_eq!(after_rename.len(), 1);
    assert_eq!(after_rename[0].1, alice_user);
    assert_eq!(
        after_rename[0].2.as_deref(),
        Some("alice"),
        "展示名保留首登时的审计值"
    );

    // 另一个人抢注了旧用户名 alice：id 不同 → 另一个账号，不能顶替
    bed.set_userinfo("github", json!({ "id": 2002, "login": "alice" }));
    let (status, _, _) = bed.login("github").await;
    assert_eq!(status, 302);
    let rows = bed.identities("github").await;
    assert_eq!(rows.len(), 2);
    let impostor = rows.iter().find(|r| r.0 == "2002").unwrap();
    assert_ne!(impostor.1, alice_user, "同名不同 id 必须是不同用户");
    let impostor_name: String =
        sqlx::query_scalar!(r#"SELECT username FROM users WHERE id = $1"#, impostor.1)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert!(
        impostor_name.starts_with("github-alice-"),
        "用户名撞了要加盐而不是失败：{impostor_name}"
    );

    // userinfo 没有 id 字段：不能拿 login 凑数
    bed.set_userinfo("github", json!({ "login": "ghost" }));
    let (status, _, body) = bed.login("github").await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"]["code"], "oauth_userinfo_missing_subject");
    assert_eq!(
        bed.identities("github").await.len(),
        2,
        "失败登录不得留下绑定"
    );

    // token 端点 500：明确的 upstream_error + 状态码 param
    *bed.idp.token_down.lock().unwrap() = true;
    bed.set_userinfo("github", json!({ "id": 3003, "login": "carol" }));
    let (status, _, body) = bed.login("github").await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"]["code"], "oauth_upstream_error");
    assert_eq!(body["error"]["param"], "status_500");
    bed.teardown().await;
}

/// 登录页据以决定露出哪些第三方按钮的公开端点。此前零集成覆盖，而它读的正是那条同时装着
/// `client_secret` 的 `settings.oauth_providers`——只要哪天顺手把整行 value 回出去，秘钥就跟着
/// 到了未登录的登录页上。这里钉住"无需鉴权、只出 code、整个响应里搜不到任何凭证字段"，
/// 外加设置缺失 / 形状不认时回空列表而不是 500（否则登录页整页挂掉）。
#[tokio::test]
async fn provider_list_is_public_and_leaks_no_credentials() {
    let bed = bed().await;
    let get = async || -> (u16, String) {
        // 不带任何 cookie / bearer：这就是登录页的调法
        let resp = bed
            .client
            .get(format!("http://{}/auth/oauth-providers", bed.console))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        (status, resp.text().await.unwrap())
    };

    let (status, raw) = get().await;
    assert_eq!(status, 200, "未登录也要能拿到：{raw}");
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        body,
        json!({ "providers": ["github", "discord", "linuxdo"] }),
        "只出 code，且保持配置顺序（登录页按钮次序）"
    );
    for leaked in ["sec-", "cid-", "client_secret", "client_id", "token_url"] {
        assert!(!raw.contains(leaked), "响应里不该出现 {leaked}：{raw}");
    }

    // 形状不认（比如有人手改设置写成了对象）：回空列表，登录页只是不露第三方按钮
    sqlx::query!(
        r#"UPDATE settings SET value = $1 WHERE key = 'oauth_providers'"#,
        json!({"github": "x"})
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    let (status, raw) = get().await;
    assert_eq!(status, 200, "{raw}");
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap(),
        json!({ "providers": [] })
    );

    // 压根没配：同样是空列表，不是 500
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'oauth_providers'"#)
        .execute(&bed.pg)
        .await
        .unwrap();
    let (status, raw) = get().await;
    assert_eq!(status, 200, "{raw}");
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap(),
        json!({ "providers": [] })
    );
    bed.teardown().await;
}
