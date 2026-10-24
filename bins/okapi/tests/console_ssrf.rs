//! SSRF 校验验收（§14.4）：缺省策略拒 http/私网/环回/localhost，
//! 公网 https 放行；用独立临时库保证缺省策略（共享库被其他套件放行）。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

#[tokio::test]
async fn ssrf_default_policy_blocks_private_targets() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    // 独立临时库：缺省无 ssrf_policy
    let admin_pool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!("okapi_ssrf_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin_pool)
    .await
    .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let fresh_url = format!("{base}/{db_name}");
    let pg = okapi_store::connect_pg(&fresh_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 超管
    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("ss-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-ss-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &hash, "sk-okapi-ss")
        .await
        .unwrap();

    let state = gateway::build_state(&fresh_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let create = |api_base: &str| {
        let client = client.clone();
        let token = token.clone();
        let api_base = api_base.to_owned();
        let name = format!("ch-{}", Uuid::new_v4().simple());
        async move {
            client
                .post(format!("http://{addr}/admin/channels"))
                .bearer_auth(&token)
                .json(&json!({"name": name, "api_base": api_base,
                    "credential": "c", "models": ["m-x"]}))
                .send()
                .await
                .unwrap()
        }
    };

    // http：缺省仅 https
    let resp = create("http://api.example.com/v1").await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["param"], "api_base_scheme_https_only");

    // 私网/环回/localhost（https 也拒）
    for target in [
        "https://10.0.0.8/v1",
        "https://192.168.1.1/v1",
        "https://127.0.0.1:8080/v1",
        "https://[::1]/v1",
        "https://localhost/v1",
        // 共享地址空间：阿里云元数据 100.100.100.200 在 100.64.0.0/10 里，以及内嵌私网地址的 v6 写法
        "https://100.100.100.200/latest/meta-data",
        "https://[::ffff:169.254.169.254]/v1",
        "https://[64:ff9b::a9fe:a9fe]/v1",
    ] {
        let resp = create(target).await;
        assert_eq!(resp.status(), 400, "{target} 必须被拒");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(
            body["error"]["param"], "api_base_private_target",
            "{target}"
        );
    }

    // Turnstile's custom verification endpoint must be guarded before sending its secret.
    sqlx::query("INSERT INTO settings(key,value) VALUES('turnstile_secret',$1),('turnstile_verify_url',$2) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
        .bind(json!("test-secret")).bind(json!("https://127.0.0.1:9/siteverify")).execute(&pg).await.unwrap();
    let denied=reqwest::Client::new().post(format!("http://{addr}/auth/register"))
        .json(&json!({"username":format!("blocked-{suffix}"),"email":format!("blocked-{suffix}@example.com"),"password":"pass-test-123456","turnstile_token":"token"})).send().await.unwrap();
    assert_eq!(denied.status(), 400);
    let denied: Value = denied.json().await.unwrap();
    assert_eq!(denied["error"]["param"], "api_base_private_target");

    // 公网 https：放行
    let resp = create("https://api.example.com/v1").await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let channel_id = resp.json::<Value>().await.unwrap()["channel_id"]
        .as_i64()
        .unwrap();

    oauth_token_url_goes_through_the_same_gate(&client, &addr, &token, channel_id).await;
    vertex_token_uri_goes_through_the_same_gate(&client, &addr, &token).await;

    // 用完即删：临时库不清，跑一天测试就在开发 PG 里留下上百个库
    pg.close().await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#
    )))
    .execute(&admin_pool)
    .await;
}

/// DNS 重绑定：保存时过闸（策略放行、域名解析出来的地址当时都算合格），之后同一个域名改指内网 /
/// 策略收紧，连接时必须再被拦下。此前只在保存时解析一次 DNS，连接时直接信任系统解析的结果，
/// 一个改了 DNS 的域名就能让网关带着渠道凭证去打内网。这里用 `localhost` 充当「重绑定之后解析到环回」的域名。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 一条完整的重绑定剧本：建库 / 起上游 / 存渠道 / 先拦后放，拆开看不出先后
async fn a_host_that_turns_private_after_the_save_is_refused_at_connect_time() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    let admin_pool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!("okapi_ssrf_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin_pool)
    .await
    .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let fresh_url = format!("{base}/{db_name}");
    let pg = okapi_store::connect_pg(&fresh_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 策略放行 http + 私网：保存 `http://localhost:…` 时能过闸
    let policy = |allow_private: bool| json!({"allow_http": true, "allow_private": allow_private});
    sqlx::query("INSERT INTO settings(key,value) VALUES('ssrf_policy',$1) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
        .bind(policy(true))
        .execute(&pg)
        .await
        .unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("rb-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-rb-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &hash, "sk-okapi-rb")
        .await
        .unwrap();

    // 假上游：只听 127.0.0.1，数命中
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let upstream = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(json!({"data": []}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let state = gateway::build_state(&fresh_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let created = client
        .post(format!("http://{addr}/admin/channels"))
        .bearer_auth(&token)
        .json(
            &json!({"name": format!("rb-{suffix}"), "credential": "c", "models": ["m-x"],
            "api_base": format!("http://localhost:{upstream_port}/v1")}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200, "{:?}", created.text().await);
    let channel_id = created.json::<Value>().await.unwrap()["channel_id"]
        .as_i64()
        .unwrap();
    let probe = || {
        let client = client.clone();
        let token = token.clone();
        async move {
            client
                .post(format!("http://{addr}/admin/channels/{channel_id}/test"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let sync = || async {
        // settings 有 60s 缓存：先清掉，再按库里的策略同步给出站校验（运行中是 15s 一次的后台同步）
        state.invalidate_routing_caches_local();
        gateway::sync_ssrf_policy(&state).await;
    };

    // 保存之后、第一次连接之前，策略不再放行私网（等价于域名在保存后改指了内网）：连接时被拦，
    // 请求到不了上游。必须先拦后放——reqwest 会复用已经建好的 keep-alive 连接，先放行一次的话，
    // 之后的探测根本不再解析、不再过校验，测不出这道闸
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'ssrf_policy'")
        .bind(policy(false))
        .execute(&pg)
        .await
        .unwrap();
    sync().await;
    let blocked = probe().await;
    assert_eq!(blocked["ok"], false, "{blocked}");
    assert!(
        blocked.get("http_status").is_none(),
        "不是上游回的状态，而是连接就被拦了：{blocked}"
    );
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "被拦下的请求不得到达上游"
    );

    // 再放行：同一个渠道连得上（策略是连接时才看的；同时恢复开关，免得残留影响同进程里别的用例）
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'ssrf_policy'")
        .bind(policy(true))
        .execute(&pg)
        .await
        .unwrap();
    sync().await;
    let reached = probe().await;
    assert_eq!(reached["ok"], true, "{reached}");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

    pg.close().await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#
    )))
    .execute(&admin_pool)
    .await;
}

/// settings.oauth_token_url 过同一道闸：网关刷新 token 时会往它 POST refresh token，
/// 渠道设置的通用写入口不能成为绕过 OAuth 登录端点校验的后门。
async fn oauth_token_url_goes_through_the_same_gate(
    client: &reqwest::Client,
    addr: &SocketAddr,
    token: &str,
    channel_id: i64,
) {
    for (settings, param) in [
        (
            json!({"oauth_token_url": "http://169.254.169.254/token"}),
            "api_base_scheme_https_only",
        ),
        (
            json!({"oauth_token_url": "https://10.0.0.8/token"}),
            "api_base_private_target",
        ),
        (json!({"oauth_token_url": 42}), "oauth_token_url"),
    ] {
        let resp = client
            .patch(format!("http://{addr}/admin/channels/{channel_id}"))
            .bearer_auth(token)
            .json(&json!({"settings": settings}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "{settings}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["param"], param, "{settings}");
    }
    let resp = client
        .patch(format!("http://{addr}/admin/channels/{channel_id}"))
        .bearer_auth(token)
        .json(&json!({"settings": {"oauth_token_url": "https://auth.example.com/oauth/token"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
}

/// Vertex 服务账号 JSON 里的 `token_uri` 也是网关会 POST 的地址（JWT 换 access token），
/// 而且测活会把它的非 2xx 响应体回给管理员：凭证的三个写入口（建渠道 / 轮换 / MCP 同函数）
/// 都得过闸，按凭证形状而不按 provider 判断。
async fn vertex_token_uri_goes_through_the_same_gate(
    client: &reqwest::Client,
    addr: &SocketAddr,
    token: &str,
) {
    let service_account = |token_uri: Option<&str>| {
        let mut sa = json!({"type": "service_account", "client_email": "x@p.iam.gserviceaccount.com",
            "private_key": "-----BEGIN PRIVATE KEY-----\nAA==\n-----END PRIVATE KEY-----\n"});
        if let Some(uri) = token_uri {
            sa["token_uri"] = json!(uri);
        }
        sa.to_string()
    };
    let create = |provider: &str, credential: String| {
        let client = client.clone();
        let provider = provider.to_owned();
        let name = format!("vx-{}", Uuid::new_v4().simple());
        async move {
            client
                .post(format!("http://{addr}/admin/channels"))
                .bearer_auth(token)
                .json(&json!({"name": name, "provider": provider, "models": ["m-x"],
                    "api_base": "https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1",
                    "credential": credential}))
                .send()
                .await
                .unwrap()
        }
    };

    for (provider, token_uri) in [
        ("vertex", "http://169.254.169.254/computeMetadata/v1/token"),
        ("vertex", "https://10.0.0.8/token"),
        // 先按别的协议存下再改成 vertex 也不行：闸看凭证形状
        ("openai", "https://127.0.0.1:8123/token"),
    ] {
        let resp = create(provider, service_account(Some(token_uri))).await;
        assert_eq!(resp.status(), 400, "{provider} {token_uri}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(
            body["error"]["param"], "credential_token_uri",
            "{token_uri}"
        );
    }

    // 缺省 token_uri（oauth2.googleapis.com）放行
    let resp = create("vertex", service_account(None)).await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let channel_id = resp.json::<Value>().await.unwrap()["channel_id"]
        .as_i64()
        .unwrap();

    // 轮换凭证是另一个写入口
    for (credential, status, param) in [
        (
            service_account(Some("http://169.254.169.254/token")),
            400,
            Some("credential_token_uri"),
        ),
        (
            service_account(Some("https://oauth2.googleapis.com/token")),
            200,
            None,
        ),
    ] {
        let resp = client
            .post(format!(
                "http://{addr}/admin/channels/{channel_id}/credential"
            ))
            .bearer_auth(token)
            .json(&json!({"credential": credential}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), status, "{credential}");
        if let Some(param) = param {
            let body: Value = resp.json().await.unwrap();
            assert_eq!(body["error"]["param"], param);
        }
    }
}
