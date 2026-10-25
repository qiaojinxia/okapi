//! 渠道测活端点验收：可达（2xx ok）/ 凭证问题（401 可达不 ok）/ 不可达。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

#[path = "support/paged_lists.rs"]
mod paged_lists;

async fn mock_models(headers: axum::http::HeaderMap) -> axum::response::Response {
    if headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "Bearer good-credential")
    {
        axum::Json(json!({"object": "list",
            "data": [{"id": "gpt-x-2"}, {"id": "gpt-x-1"}, {"id": "gpt-x-2"}]}))
        .into_response()
    } else {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": {"message": "bad key"}})),
        )
            .into_response()
    }
}

/// new-api / one-api 生态的余额两端点：凭证不对 401，对了给额度 100 美元 / 用量 1234.5 美分。
async fn mock_subscription(headers: axum::http::HeaderMap) -> axum::response::Response {
    if !authorized(&headers) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(
        json!({"object": "billing_subscription", "hard_limit_usd": 100.0,
        "has_payment_method": true, "soft_limit_usd": 100.0}),
    )
    .into_response()
}

async fn mock_usage(headers: axum::http::HeaderMap) -> axum::response::Response {
    if !authorized(&headers) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(json!({"object": "list", "total_usage": 1234.5})).into_response()
}

/// 四家官方余额接口的 mock（响应样本取自各家文档示例）：DeepSeek 的在站点根，其余在 /v1 下。
fn authorized_json(headers: &axum::http::HeaderMap, body: Value) -> axum::response::Response {
    if authorized(headers) {
        axum::Json(body).into_response()
    } else {
        axum::http::StatusCode::UNAUTHORIZED.into_response()
    }
}

async fn mock_deepseek_balance(headers: axum::http::HeaderMap) -> axum::response::Response {
    authorized_json(
        &headers,
        json!({"is_available": true, "balance_infos": [{"currency": "CNY",
            "total_balance": "110.00", "granted_balance": "10.00", "topped_up_balance": "100.00"}]}),
    )
}

async fn mock_moonshot_balance(headers: axum::http::HeaderMap) -> axum::response::Response {
    authorized_json(
        &headers,
        json!({"code": 0, "data": {"available_balance": 49.58894, "voucher_balance": 46.58893,
            "cash_balance": 3.00001}, "scode": "0x0", "status": true}),
    )
}

async fn mock_siliconflow_info(headers: axum::http::HeaderMap) -> axum::response::Response {
    authorized_json(
        &headers,
        json!({"code": 20000, "message": "OK", "status": true, "data": {"balance": "0.88",
            "chargeBalance": "88.00", "totalBalance": "88.88"}}),
    )
}

async fn mock_openrouter_credits(headers: axum::http::HeaderMap) -> axum::response::Response {
    authorized_json(
        &headers,
        json!({"data": {"total_credits": 100.5, "total_usage": 25.75}}),
    )
}

fn authorized(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "Bearer good-credential")
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/v1/models", get(mock_models))
        .route("/v1/dashboard/billing/subscription", get(mock_subscription))
        .route("/v1/dashboard/billing/usage", get(mock_usage))
        .route("/user/balance", get(mock_deepseek_balance))
        .route("/v1/users/me/balance", get(mock_moonshot_balance))
        .route("/v1/user/info", get(mock_siliconflow_info))
        .route("/v1/credits", get(mock_openrouter_credits));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
    admin_token: String,
    mock: SocketAddr,
}

async fn setup() -> TestEnv {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-test-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(admin_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        addr,
        admin_token,
        mock,
    }
}

async fn mk_channel(env: &TestEnv, credential: &str, base: &str) -> i64 {
    let suffix = Uuid::new_v4().simple().to_string();
    let (id, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("t-{suffix}"),
        "openai",
        base,
        credential,
        &["m-x"],
        false,
        None,
    )
    .await
    .unwrap();
    id
}

async fn test_channel(env: &TestEnv, id: i64) -> Value {
    reqwest::Client::new()
        .post(format!("http://{}/admin/channels/{id}/test", env.addr))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn channel_test_probes_reachability() {
    let env = setup().await;

    // 凭证正确：ok + 2xx
    let good = mk_channel(&env, "good-credential", &format!("http://{}/v1", env.mock)).await;
    let r = test_channel(&env, good).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["http_status"], 200);
    assert!(r["latency_ms"].as_i64().unwrap() >= 0);

    // 凭证错误：可达但不 ok（401 供管理员判断）
    let bad = mk_channel(&env, "bad-credential", &format!("http://{}/v1", env.mock)).await;
    let r = test_channel(&env, bad).await;
    assert_eq!(r["ok"], false);
    assert_eq!(r["http_status"], 401);

    // 不可达：connect 失败带 error_code
    let dead = mk_channel(&env, "x", "http://127.0.0.1:9/v1").await;
    let r = test_channel(&env, dead).await;
    assert_eq!(r["ok"], false);
    assert!(r["error_code"].is_string(), "{r}");
    assert!(r["at"].is_string(), "测活结果带时间戳供列表回填：{r}");

    // 测活结果留痕：列表页每行回填 last_test（new-api response_time/test_time 语义）
    let (_, list) = paged_lists::get_all(env.addr, "/admin/channels", &env.admin_token).await;
    let row = |id: i64| {
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"].as_i64() == Some(id))
            .cloned()
            .unwrap()
    };
    assert_eq!(row(good)["last_test"]["ok"], true, "{}", row(good));
    assert_eq!(row(good)["last_test"]["http_status"], 200);
    assert_eq!(row(bad)["last_test"]["http_status"], 401);
    assert_eq!(row(dead)["last_test"]["ok"], false);
    assert!(row(dead)["last_test"]["error_code"].is_string());
    let never = mk_channel(&env, "never-tested", &format!("http://{}/v1", env.mock)).await;
    let (_, list2) = paged_lists::get_all(env.addr, "/admin/channels", &env.admin_token).await;
    let never_row = list2["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_i64() == Some(never))
        .unwrap();
    assert!(
        never_row["last_test"].is_null(),
        "没测过的渠道 last_test 为 null"
    );

    // 上游模型发现（§11.3）：openai 形状 data[].id
    let models: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/channels/{good}/fetch-models",
            env.addr
        ))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        models["models"],
        serde_json::json!(["gpt-x-1", "gpt-x-2"]),
        "{models}"
    );

    // 审计留痕
    let audits = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs WHERE action = 'channel.test'"#
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(audits >= 3);
}

/// 上游余额查询（§11.33）：dashboard 口径 = 额度 − 用量（美分）；结果留痕回填列表；
/// 凭证错 502 带上游状态；anthropic 没有余额接口 → 400 balance_unsupported。
#[tokio::test]
async fn channel_balance_probe() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let balance = |id: i64| {
        client
            .get(format!("http://{}/admin/channels/{id}/balance", env.addr))
            .bearer_auth(&env.admin_token)
            .send()
    };

    let good = mk_channel(&env, "good-credential", &format!("http://{}/v1", env.mock)).await;
    let resp = balance(good).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["probe"], "openai_dashboard", "{body}");
    assert_eq!(body["currency"], "USD");
    assert_eq!(body["total_micro"], 100_000_000);
    assert_eq!(body["used_micro"], 12_345_000);
    assert_eq!(body["balance_micro"], 87_655_000);
    assert!(body["at"].is_string());

    let bad = mk_channel(&env, "bad-credential", &format!("http://{}/v1", env.mock)).await;
    let resp = balance(bad).await.unwrap();
    assert_eq!(resp.status(), 502);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(body["error"]["param"], "status_401");

    let (anthropic, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("claude-{}", Uuid::new_v4().simple()),
        "anthropic",
        "https://api.anthropic.com/v1",
        "x",
        &["m-x"],
        false,
        None,
    )
    .await
    .unwrap();
    let resp = balance(anthropic).await.unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["param"], "balance_unsupported");

    // 列表回填 last_balance（与 last_test 同一留痕机制）
    let (_, list) = paged_lists::get_all(env.addr, "/admin/channels", &env.admin_token).await;
    let row = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_i64() == Some(good))
        .unwrap();
    assert_eq!(row["last_balance"]["balance_micro"], 87_655_000, "{row}");
    assert_eq!(row["last_balance"]["currency"], "USD");
}

async fn balance_of(env: &TestEnv, id: i64) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://{}/admin/channels/{id}/balance", env.addr))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

async fn put_vendor(env: &TestEnv, id: i64, vendor: &str) {
    let resp = reqwest::Client::new()
        .put(format!("http://{}/admin/channels/{id}/compat", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({"vendor": vendor}))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(status, 200, "{}", resp.text().await.unwrap());
}

/// 余额探针是厂商数据：选了厂商的渠道按那家的口径查，不看主机名。mock 只能起在 127.0.0.1 上，
/// 过去四家官方探针因此做不了端到端；现在每家的路径 / 凭证 / 解析都在这里走一遍。
/// 档案叠加内置厂商时取它叠加的那个；没有专属探针的厂商与没选厂商的渠道仍走通用 dashboard 口径。
#[tokio::test]
async fn channel_balance_follows_the_chosen_vendor() {
    let env = setup().await;
    let base = format!("http://{}/v1", env.mock);
    let channel = mk_channel(&env, "good-credential", &base).await;

    let (status, body) = balance_of(&env, channel).await;
    assert_eq!(
        (status, body["probe"].as_str()),
        (200, Some("openai_dashboard")),
        "{body}"
    );

    // (厂商, 币种, 剩余, 总额, 已用)
    let cases = [
        ("deepseek", "CNY", 110_000_000, None, None),
        ("moonshot", "CNY", 49_588_940, None, None),
        ("siliconflow", "CNY", 88_880_000, None, None),
        (
            "openrouter",
            "USD",
            74_750_000,
            Some(100_500_000),
            Some(25_750_000),
        ),
    ];
    for (vendor, currency, remaining, total, used) in cases {
        put_vendor(&env, channel, vendor).await;
        let (status, body) = balance_of(&env, channel).await;
        assert_eq!(status, 200, "{vendor}: {body}");
        assert_eq!(body["probe"], vendor, "{body}");
        assert_eq!(body["currency"], currency, "{vendor}");
        assert_eq!(body["balance_micro"], remaining, "{vendor}");
        assert_eq!(body["total_micro"], json!(total), "{vendor}");
        assert_eq!(body["used_micro"], json!(used), "{vendor}");
    }

    // 凭证不对：照常 502 带上游状态（这次是厂商探针的路径）
    let bad = mk_channel(&env, "bad-credential", &base).await;
    put_vendor(&env, bad, "deepseek").await;
    let (status, body) = balance_of(&env, bad).await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["param"], "status_401");

    // 选了没有专属探针的厂商：回到通用口径
    put_vendor(&env, channel, "zhipu").await;
    let (status, body) = balance_of(&env, channel).await;
    assert_eq!(
        (status, body["probe"].as_str()),
        (200, Some("openai_dashboard")),
        "{body}"
    );
    assert_eq!(body["balance_micro"], 87_655_000);

    // 档案：叠加了内置厂商就用那家的探针，没叠加就是通用口径
    let slug = format!("bal-{}", Uuid::new_v4().simple());
    let client = reqwest::Client::new();
    for (profile_slug, extends) in [
        (format!("{slug}-ds"), Some("deepseek")),
        (format!("{slug}-bare"), None),
    ] {
        let resp = client
            .post(format!("http://{}/admin/compat/profiles", env.addr))
            .bearer_auth(&env.admin_token)
            .json(&json!({"slug": profile_slug, "name": "余额测试", "extends": extends}))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        assert_eq!(status, 200, "{}", resp.text().await.unwrap());
        put_vendor(&env, channel, &format!("custom:{profile_slug}")).await;
        let (status, body) = balance_of(&env, channel).await;
        assert_eq!(status, 200, "{body}");
        let expected = if extends.is_some() {
            "deepseek"
        } else {
            "openai_dashboard"
        };
        assert_eq!(body["probe"], expected, "{profile_slug}: {body}");
    }
}

/// 兼容渠道没有缺省地址：没配 api_base 就查不了，明确告诉管理员缺哪个字段，而不是去问别人家的地址。
#[tokio::test]
async fn channel_balance_needs_an_api_base_for_compat_channels() {
    let env = setup().await;
    let channel = mk_channel(&env, "good-credential", &format!("http://{}/v1", env.mock)).await;
    sqlx::query("UPDATE channels SET provider = 'openai_compat', api_base = NULL WHERE id = $1")
        .bind(channel)
        .execute(&env.pg)
        .await
        .unwrap();
    let (status, body) = balance_of(&env, channel).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "api_base");
}

/// 管理面探针不跟随重定向：SSRF 闸只看得到管理员填的 api_base，上游一跳 302 不能把探测
/// 引到别的地址（私网 / 元数据）。测活拿到的是 302 本身，重定向目标零命中。
#[tokio::test]
async fn channel_test_does_not_follow_redirects() {
    let env = setup().await;
    let leaked = Arc::new(AtomicUsize::new(0));
    let router = axum::Router::new()
        .route(
            "/v1/models",
            axum::routing::get(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, "/leak")],
                )
            }),
        )
        .route(
            "/leak",
            axum::routing::get({
                let leaked = Arc::clone(&leaked);
                move || {
                    let leaked = Arc::clone(&leaked);
                    async move {
                        leaked.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({"data": []}))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirecting = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let id = mk_channel(&env, "sk-good", &format!("http://{redirecting}/v1")).await;
    let body = test_channel(&env, id).await;
    assert_eq!(body["ok"], false, "{body}");
    assert_eq!(
        body["http_status"], 302,
        "拿到的是 302 本身而非跟过去的 200：{body}"
    );
    assert_eq!(leaked.load(Ordering::SeqCst), 0, "重定向目标不得被请求");
}

/// 测活和模型发现先取启用的 key：id 最小的那把如果已停用 / 失效，测出来的 401 会把运维引到
/// 「渠道坏了」，其实别的 key 好好的。
#[tokio::test]
async fn channel_test_prefers_an_enabled_key() {
    let env = setup().await;
    let id = mk_channel(&env, "bad-credential", &format!("http://{}/v1", env.mock)).await;
    sqlx::query("UPDATE channel_keys SET status = 2 WHERE channel_id = $1")
        .bind(id)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO channel_keys (channel_id, credential_ciphertext, status) VALUES ($1, $2, 1)",
    )
    .bind(id)
    .bind(b"good-credential".as_slice())
    .execute(&env.pg)
    .await
    .unwrap();

    let probed = test_channel(&env, id).await;
    assert_eq!(
        probed["ok"], true,
        "停用的第一把 key 不该决定测活结果：{probed}"
    );
    let models: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/channels/{id}/fetch-models",
            env.addr
        ))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["models"], json!(["gpt-x-1", "gpt-x-2"]), "{models}");
}

/// 新建渠道时的上游模型发现：渠道还没落库，用表单里刚填的地址 / 凭证。校验与建渠道同一套
/// （地址过 SSRF 闸、设置合法、出口对操作者可见）；订阅账号要先建渠道；什么都不落库。
#[tokio::test]
async fn draft_model_discovery_works_before_the_channel_exists() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let url = format!("http://{}/admin/channels/fetch-models", env.addr);
    let base = format!("http://{}/v1", env.mock);
    let post = |token: Option<&str>, body: Value| {
        let mut request = client.post(&url).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        async move {
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            (
                status,
                response.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    let draft = |credential: &str| json!({"provider": "openai", "api_base": base, "credential": credential});
    let channels = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels")
            .fetch_one(&env.pg)
            .await
            .unwrap()
    };
    let before = channels().await;

    // 未登录 401；没有 channel.write 的用户 403
    assert_eq!(post(None, draft("good-credential")).await.0, 401);
    let suffix = Uuid::new_v4().simple().to_string();
    let plain = okapi_store::provision::create_user(&env.pg, &format!("plain-{suffix}"))
        .await
        .unwrap();
    let plain_token = format!("sk-okapi-plain-{suffix}");
    let plain_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(plain_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&env.pg, plain, &plain_hash, "sk-okapi-plain")
        .await
        .unwrap();
    let (status, body) = post(Some(&plain_token), draft("good-credential")).await;
    assert_eq!(
        (status, &body["error"]["param"]),
        (403, &json!("channel.write"))
    );

    // 缺省策略下回环 http 地址过不了 SSRF 闸（与建渠道同一道闸），凭证没有发出去
    let (status, body) = post(Some(&env.admin_token), draft("good-credential")).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "api_base_scheme_https_only");

    // 放开策略后：按表单里的凭证拉到列表，去重排序
    sqlx::query("INSERT INTO settings(key,value) VALUES('ssrf_policy',$1) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
        .bind(json!({"allow_http": true, "allow_private": true}))
        .execute(&env.pg)
        .await
        .unwrap();
    let (status, body) = post(Some(&env.admin_token), draft("good-credential")).await;
    assert_eq!(
        (status, &body["models"]),
        (200, &json!(["gpt-x-1", "gpt-x-2"])),
        "{body}"
    );
    // 错的凭证原样回上游状态
    let (status, body) = post(Some(&env.admin_token), draft("bad-credential")).await;
    assert_eq!(
        (status, &body["error"]["param"]),
        (502, &json!("status_401")),
        "{body}"
    );

    // 参数校验
    let cases = [
        (
            json!({"provider": "codex", "credential": "x"}),
            "fetch_models_requires_saved_channel",
        ),
        (
            json!({"provider": "anthropic_max", "credential": "x"}),
            "fetch_models_requires_saved_channel",
        ),
        (
            json!({"provider": "no-such-protocol", "credential": "x"}),
            "provider",
        ),
        (draft("   "), "credential"),
        (
            json!({"provider": "openai", "api_base": base, "credential": "x", "settings": "nope"}),
            "settings",
        ),
    ];
    for (request, param) in cases {
        let (status, body) = post(Some(&env.admin_token), request.clone()).await;
        assert_eq!(status, 400, "{request}: {body}");
        assert_eq!(body["error"]["param"], param, "{request}");
    }
    // 出口也按建渠道的规则校验：不存在的代理 404
    let mut with_proxy = draft("good-credential");
    with_proxy["egress"] = json!({"mode": "proxy", "proxy_id": 999_999_999});
    assert_eq!(post(Some(&env.admin_token), with_proxy).await.0, 404);

    // 只是问了一下上游：没有建渠道，也没有把凭证存下来
    assert_eq!(channels().await, before);
    sqlx::query("DELETE FROM settings WHERE key = 'ssrf_policy'")
        .execute(&env.pg)
        .await
        .unwrap();
}
