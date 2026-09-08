//! 分组级限流（IMPLEMENTATION §11.32）：`price_groups.rpm_limit / rph_limit` 是分组内
//! **每用户**的固定窗上限，随鉴权缓存下发、reserve 前检查。依赖 .env（scripts/dev-deps.sh up）。

use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":10,"completion_tokens":2}
    }))
    .into_response()
}

struct TestEnv {
    pg: PgPool,
    state: gateway::state::AppState,
    gateway: SocketAddr,
    model: String,
    group: String,
    suffix: String,
}

async fn setup(rpm: Option<i32>, rph: Option<i32>) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-gr-{}", &suffix[..10]);
    let group = format!("gr-{}", &suffix[..10]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    okapi_store::admin::upsert_price_group(
        &pg,
        okapi_store::admin::PriceGroupInput {
            group_code: &group,
            group_ratio: "1",
            description: "",
            pool_code: None,
            self_select: false,
            rpm_limit: rpm,
            rph_limit: rph,
        },
    )
    .await
    .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock_app = axum::Router::new().route("/v1/chat/completions", post(mock_ok));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_app).await.unwrap();
    });
    okapi_store::provision::create_channel(
        &pg,
        &format!("gr-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = gateway::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    TestEnv {
        pg,
        state,
        gateway: addr,
        model,
        group,
        suffix,
    }
}

/// 建一个有余额的用户 + key；`group = Some(g)` 则绑到该分组。
async fn user_with_key(env: &TestEnv, tag: &str, group: Option<&str>) -> String {
    let user_id = okapi_store::provision::create_user(&env.pg, &format!("{tag}-{}", env.suffix))
        .await
        .unwrap();
    if let Some(g) = group {
        okapi_store::admin::set_user_groups(&env.pg, user_id, &[(g.to_owned(), 10)])
            .await
            .unwrap();
    }
    let token = format!("sk-okapi-{tag}-{}", env.suffix);
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&env.pg, user_id, &hash, "sk-okapi-gr")
        .await
        .unwrap();
    env.state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
    token
}

async fn chat(env: &TestEnv, token: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(token)
        .json(&json!({"model": env.model, "max_tokens": 16,
            "messages": [{"role":"user","content": format!("q-{}", Uuid::new_v4())}]}))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// rpm=2：同一用户第 3 笔 429 param=group_rpm；同组另一用户有自己的桶；别组用户不受影响。
#[tokio::test]
async fn group_rpm_is_per_user_in_group() {
    let env = setup(Some(2), None).await;
    let a = user_with_key(&env, "a", Some(&env.group)).await;
    let b = user_with_key(&env, "b", Some(&env.group)).await;
    let other = user_with_key(&env, "c", None).await;

    assert_eq!(chat(&env, &a).await.0, 200);
    assert_eq!(chat(&env, &a).await.0, 200);
    let (status, body) = chat(&env, &a).await;
    assert_eq!(status, 429, "{body}");
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["error"]["param"], "group_rpm");

    // 每用户各自计数：b 的窗口是空的
    assert_eq!(chat(&env, &b).await.0, 200);
    assert_eq!(chat(&env, &b).await.0, 200);
    assert_eq!(chat(&env, &b).await.0, 429);

    // 未配限额的分组零行为
    for _ in 0..4 {
        assert_eq!(chat(&env, &other).await.0, 200);
    }
}

/// rph=1：第 2 笔 429 param=group_rph（小时窗）。
#[tokio::test]
async fn group_rph_caps_hourly() {
    let env = setup(None, Some(1)).await;
    let a = user_with_key(&env, "h", Some(&env.group)).await;
    assert_eq!(chat(&env, &a).await.0, 200);
    let (status, body) = chat(&env, &a).await;
    assert_eq!(status, 429, "{body}");
    assert_eq!(body["error"]["param"], "group_rph");
}

/// 不计费但会打上游的两个端点同样进窗（§11.32，09-08 补）：`count_tokens` 有 anthropic 候选时
/// 代理上游 tokenizer，视频任务轮询 / 下载拿渠道凭证打上游——此前两者鉴权后直接放行，
/// 一把 key 就能无限消耗渠道配额。限速在任务查找之前，所以连不存在的 task_id 也先 429 而非 404。
#[tokio::test]
async fn non_billing_upstream_endpoints_are_rate_limited() {
    let env = setup(Some(2), None).await;
    let client = reqwest::Client::new();

    // count_tokens：本用例没有 anthropic 渠道，前两笔走本地估算 200，第三笔进不来
    let ct_token = user_with_key(&env, "ct", Some(&env.group)).await;
    let count_tokens = || {
        client
            .post(format!("http://{}/v1/messages/count_tokens", env.gateway))
            .bearer_auth(&ct_token)
            .json(&json!({"model": env.model, "max_tokens": 16,
                "messages": [{"role": "user", "content": "hello"}]}))
            .send()
    };
    for i in 0..2 {
        let resp = count_tokens().await.unwrap();
        assert_eq!(
            resp.status(),
            200,
            "第 {i} 笔应放行：{:?}",
            resp.text().await
        );
    }
    let resp = count_tokens().await.unwrap();
    assert_eq!(resp.status(), 429);
    let body: Value = resp.json().await.unwrap();
    // Anthropic 入口的错误壳：{"type":"error","error":{"type":<code>,"message":"<code> <param>"}}
    assert_eq!(body["error"]["type"], "rate_limited", "{body}");
    assert_eq!(body["error"]["message"], "rate_limited group_rpm", "{body}");

    // 视频任务轮询：任务不存在本该 404，超限后先撞 429（证明限速在查找之前）
    let v_token = user_with_key(&env, "vd", Some(&env.group)).await;
    let poll = |suffix: &str| {
        client
            .get(format!("http://{}/v1/videos/task-{suffix}", env.gateway))
            .bearer_auth(&v_token)
            .send()
    };
    for i in 0..2 {
        assert_eq!(poll("missing").await.unwrap().status(), 404, "第 {i} 笔");
    }
    let resp = poll("missing").await.unwrap();
    assert_eq!(resp.status(), 429);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited", "{body}");
    assert_eq!(body["error"]["param"], "group_rpm", "{body}");
    // 下载入口与轮询共用同一段准入
    let resp = client
        .get(format!(
            "http://{}/v1/videos/task-missing/content",
            env.gateway
        ))
        .bearer_auth(&v_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429, "{:?}", resp.text().await);
}

/// 管理面改限额：写入即失效鉴权缓存，下一请求按新值判；负数 400 带 param；列表回显。
#[tokio::test]
async fn console_updates_group_limits() {
    let env = setup(None, None).await;
    let admin_id = okapi_store::provision::create_user(&env.pg, &format!("adm-{}", env.suffix))
        .await
        .unwrap();
    sqlx::query!(r#"UPDATE users SET role = 100 WHERE id = $1"#, admin_id)
        .execute(&env.pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-adm-{}", env.suffix);
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(admin_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&env.pg, admin_id, &hash, "sk-okapi-adm")
        .await
        .unwrap();
    let console = okapi::console::router(env.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, console).await.unwrap();
    });
    let client = reqwest::Client::new();
    let upsert = |body: Value| {
        client
            .post(format!("http://{console_addr}/admin/groups"))
            .bearer_auth(&admin_token)
            .json(&body)
            .send()
    };

    let bad = upsert(json!({"group_code": env.group, "group_ratio": "1", "rpm_limit": -1}))
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let body: Value = bad.json().await.unwrap();
    assert_eq!(body["error"]["param"], "rpm_limit");

    let user = user_with_key(&env, "u", Some(&env.group)).await;
    assert_eq!(chat(&env, &user).await.0, 200, "未配限额时放行");

    let ok = upsert(
        json!({"group_code": env.group, "group_ratio": "1", "rpm_limit": 1, "rph_limit": 0}),
    )
    .await
    .unwrap();
    assert_eq!(ok.status(), 200);
    // 鉴权缓存已全量失效：第一笔进新窗放行，第二笔即超限
    assert_eq!(chat(&env, &user).await.0, 200);
    assert_eq!(chat(&env, &user).await.0, 429);

    // 共享开发库里分组很多，按页翻到本用例的分组为止
    let mut offset = 0;
    let row = loop {
        let list: Value = client
            .get(format!(
                "http://{console_addr}/admin/groups?limit=100&offset={offset}"
            ))
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let page = list["data"].as_array().unwrap();
        if let Some(row) = page.iter().find(|g| g["group_code"] == env.group) {
            break row.clone();
        }
        assert!(!page.is_empty(), "翻完全部分页仍未找到 {}", env.group);
        offset += 100;
    };
    assert_eq!(row["rpm_limit"], 1);
    assert!(
        row["rph_limit"].is_null(),
        "0 写入应归一为 null（不限）：{row}"
    );
}
