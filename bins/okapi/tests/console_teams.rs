//! Team 层验收（§6.1 定案）：建团（owner 自动入团）→ 加成员（限额）→
//! 成员自助发团 key → 请求扣团钱包 → 结算累计月度消费 → 超限拒绝 →
//! 分账端点 → 非成员越权拒绝。依赖 .env（scripts/dev-deps.sh up）。

use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":150}
    }))
    .into_response()
}

struct TestEnv {
    ledger: okapi_ledger::BalanceLedger,
    console: SocketAddr,
    gateway: SocketAddr,
    model: String,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);
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
        &format!("tm-{suffix}"),
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
    let ledger = state.ledger.clone();

    let console_app = console::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, console_app).await.unwrap();
    });

    let gw_app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, gw_app).await.unwrap();
    });

    TestEnv {
        ledger,
        console: console_addr,
        gateway: gw_addr,
        model,
    }
}

/// 注册 + 登录 → (user_id, session cookie)。
async fn register_login(env: &TestEnv, client: &reqwest::Client) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let email = format!("tm-{suffix}@ok.test");
    let reg: Value = client
        .post(format!("http://{}/auth/register", env.console))
        .json(&json!({"email": email, "username": format!("tm-{suffix}"),
            "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user_id = reg["user_id"].as_i64().unwrap();
    let login = client
        .post(format!("http://{}/auth/login", env.console))
        .json(&json!({"email": email, "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    let cookie = login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap()
        .to_owned();
    (user_id, cookie)
}

async fn chat(env: &TestEnv, key: &str) -> u16 {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(key)
        .json(&json!({"model": env.model, "max_tokens": 16,
            "messages": [{"role":"user","content": format!("q-{}", Uuid::new_v4())}]}))
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

#[tokio::test]
// 团队全生命周期场景脚本
#[allow(clippy::too_many_lines)]
async fn team_wallet_member_limit_full_cycle() {
    let env = setup().await;
    let client = reqwest::Client::new();

    // owner 建团；bob 加入（月限 $0.0006 = 600 micro，恰好一次请求 500 micro 内）
    let (_owner_id, owner_cookie) = register_login(&env, &client).await;
    let (bob_id, bob_cookie) = register_login(&env, &client).await;

    let team: Value = client
        .post(format!("http://{}/api/teams", env.console))
        .header(reqwest::header::COOKIE, &owner_cookie)
        .json(&json!({"name": "acme"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let team_id = team["team_id"].as_i64().unwrap();

    // 团钱包主体 = team 用户：直接 credit
    env.ledger
        .credit(team_id, Money::from_micros(1_000_000))
        .await
        .unwrap();

    // 非成员发团 key：403
    let (_, outsider_cookie) = register_login(&env, &client).await;
    let denied = client
        .post(format!("http://{}/api/teams/{team_id}/keys", env.console))
        .header(reqwest::header::COOKIE, &outsider_cookie)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);

    // owner 加 bob（限额 600 micro）
    let added = client
        .post(format!(
            "http://{}/api/teams/{team_id}/members",
            env.console
        ))
        .header(reqwest::header::COOKIE, &owner_cookie)
        .json(&json!({"user_id": bob_id, "role": "member",
            "monthly_spend_limit_micro": 600}))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 200);
    // bob（member）不能管理成员
    let bob_manage = client
        .post(format!(
            "http://{}/api/teams/{team_id}/members",
            env.console
        ))
        .header(reqwest::header::COOKIE, &bob_cookie)
        .json(&json!({"user_id": bob_id, "role": "admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bob_manage.status(), 403);

    // bob 自助发团 key
    let key_resp: Value = client
        .post(format!("http://{}/api/teams/{team_id}/keys", env.console))
        .header(reqwest::header::COOKIE, &bob_cookie)
        .json(&json!({"name": "bob-cli"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_key = key_resp["api_key"].as_str().unwrap();

    // 第一次请求：扣团钱包（(100+150)×1×$2/1M = 500 micro）
    assert_eq!(chat(&env, bob_key).await, 200);
    // 结算是后台任务：等团钱包扣款到位
    let mut balance = 0;
    for _ in 0..50 {
        balance = env.ledger.balance(team_id).await.unwrap().as_micros();
        if balance == 1_000_000 - 500 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(balance, 1_000_000 - 500, "扣团钱包而非成员个人");

    // 后续请求直至限额生效（软实时：结算后计数，落账前的并发窗口可能多放行 1-2 笔，
    // §6.1 明示语义）；统计实际成功次数
    let mut ok_count: i64 = 1; // 首笔已成功
    let mut denied_code = 0;
    for _ in 0..50 {
        let code = chat(&env, bob_key).await;
        if code == 429 {
            denied_code = 429;
            break;
        }
        assert_eq!(code, 200);
        ok_count += 1;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(denied_code, 429, "月度限额必须最终生效");
    assert!(ok_count >= 2, "限额 600 至少放行 2 笔（每笔 500）");

    // 分账端点：与实际成功笔数一致
    let expected_spend = ok_count * 500;
    let mut usage = Value::Null;
    for _ in 0..50 {
        usage = client
            .get(format!("http://{}/api/teams/{team_id}/usage", env.console))
            .header(reqwest::header::COOKIE, &owner_cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if usage["balance_micro"].as_i64() == Some(1_000_000 - expected_spend) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        usage["balance_micro"].as_i64().unwrap(),
        1_000_000 - expected_spend,
        "团钱包扣款 = 成功笔数 × 500"
    );
    let bob_row = usage["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["member_user_id"].as_i64() == Some(bob_id))
        .expect("分账必须含 bob");
    assert_eq!(
        bob_row["month_spend_micro"].as_i64().unwrap(),
        expected_spend,
        "月度计数与成功笔数一致"
    );
    assert_eq!(bob_row["monthly_spend_limit_micro"], 600);
    assert_eq!(bob_row["role"], "member");

    // 非成员看分账：403
    let denied = client
        .get(format!("http://{}/api/teams/{team_id}/usage", env.console))
        .header(reqwest::header::COOKIE, &outsider_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);

    // ---- 我所属团队列表（UI 入口；没有它前端无从知道自己在哪些团）----
    let mine: Value = client
        .get(format!("http://{}/api/teams", env.console))
        .header(reqwest::header::COOKIE, &owner_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = mine["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["team_id"].as_i64() == Some(team_id))
        .expect("owner 必须看到自己建的团");
    assert_eq!(row["name"], "acme", "展示名须剥掉唯一性后缀");
    assert_eq!(row["role"], "owner");
    assert_eq!(row["member_count"], 2, "owner + bob");
    assert_eq!(
        row["balance_micro"].as_i64().unwrap(),
        1_000_000 - expected_spend,
        "列表余额与分账口径一致"
    );

    // bob 也能看到该团，且角色是 member
    let bob_teams: Value = client
        .get(format!("http://{}/api/teams", env.console))
        .header(reqwest::header::COOKIE, &bob_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_view = bob_teams["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["team_id"].as_i64() == Some(team_id))
        .expect("成员必须看到所属团");
    assert_eq!(bob_view["role"], "member");
    assert_eq!(bob_view["monthly_spend_limit_micro"], 600);

    // 非成员的列表里不含该团（越权可见性）
    let outsider_teams: Value = client
        .get(format!("http://{}/api/teams", env.console))
        .header(reqwest::header::COOKIE, &outsider_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !outsider_teams["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["team_id"].as_i64() == Some(team_id)),
        "非成员不得在列表中看到该团"
    );

    // 无会话（仅 API key 单轨）访问：401——前端据此降级提示改用邮箱密码登录
    let no_session = client
        .get(format!("http://{}/api/teams", env.console))
        .send()
        .await
        .unwrap();
    assert_eq!(no_session.status(), 401);
}
