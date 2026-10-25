//! 适配规则在聊天链路上的验收（取代原来的渠道 strip / inject 设置，new-api rc.23 #6847 对齐）：
//! 配置字段被剥、未配置字段照传、受保护键不可动、未配置渠道原样透传，再加上厂商层只对选了它
//! 的渠道生效、护栏管到所有渠道且最后执行、命中轨迹进诊断。依赖 .env（scripts/dev-deps.sh up）。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use uuid::Uuid;

type Captured = Arc<Mutex<Option<Value>>>;

async fn spawn_mock(captured: Captured) -> SocketAddr {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::body::Bytes| {
            let captured = Arc::clone(&captured);
            async move {
                let req: Value = serde_json::from_slice(&body).unwrap();
                *captured.lock().unwrap() = Some(req);
                axum::Json(json!({
                    "id":"cmpl","object":"chat.completion","model":"m",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
                    "usage":{"prompt_tokens":10,"completion_tokens":5,
                             "prompt_tokens_details":{"cached_tokens":0}}
                }))
                .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    gateway: SocketAddr,
    token: String,
    model: String,
    captured: Captured,
    pg: sqlx::PgPool,
    user_id: i64,
}

/// 一次测试的适配规则配置；全空 = 原样透传。
#[derive(Default)]
struct Config {
    /// 渠道层规则（走正常发布路径，经过校验）。
    channel_rules: Option<Value>,
    /// 渠道选定的内置厂商。
    vendor: Option<&'static str>,
    /// 全局护栏（整个网关，最后执行）。
    guard: Option<Value>,
    /// 绕过写入校验直接塞进库的渠道层规则集（模拟被手改过的库）。
    raw_channel_rules: Option<Value>,
}

/// 把一次测试的适配规则配置写进库。
async fn apply_config(pg: &sqlx::PgPool, channel_id: i64, config: &Config) {
    if let Some(vendor) = config.vendor {
        sqlx::query("UPDATE channels SET vendor = $2 WHERE id = $1")
            .bind(channel_id)
            .bind(vendor)
            .execute(pg)
            .await
            .unwrap();
    }
    let mut conn = pg.acquire().await.unwrap();
    if let Some(rules) = &config.channel_rules {
        okapi_store::compat::publish(
            &mut conn,
            okapi_store::compat::Scope::Channel(channel_id),
            rules,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    }
    if let Some(rules) = &config.guard {
        okapi_store::compat::publish(
            &mut conn,
            okapi_store::compat::Scope::Global,
            rules,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    }
    if let Some(rules) = &config.raw_channel_rules {
        sqlx::query(
            "INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, rules, published_at)
             VALUES ('compat', 'channel', $1, 1, 'published', $2, now())",
        )
        .bind(channel_id)
        .bind(rules)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    drop(conn);
}

async fn setup(config: Config) -> TestEnv {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("sf-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-sf-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-sf")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();

    let captured: Captured = Arc::new(Mutex::new(None));
    let mock = spawn_mock(Arc::clone(&captured)).await;
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("sf-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    apply_config(&pg, channel_id, &config).await;

    published_pricing::publish(&pg, user_id).await;
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        gateway: addr,
        token,
        model,
        captured,
        pg,
        user_id,
    }
}

async fn chat(env: &TestEnv, body: Value) -> Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();
    let captured = env.captured.lock().unwrap().clone();
    captured.expect("mock 应收到上游请求")
}

async fn chat_with_extras(env: &TestEnv) -> Value {
    chat(
        env,
        json!({
            "model": env.model,
            "messages": [{"role": "user", "content": "hi"}],
            "logit_bias": {"50256": -100},
            "user": "end-user-1",
            "temperature": 0.5
        }),
    )
    .await
}

/// 配置剥除：logit_bias/user 不透传，temperature 照传，受保护键仍在。
#[tokio::test]
async fn strips_configured_fields_only() {
    let env = setup(Config {
        channel_rules: Some(json!([
            {"id": "strip-request-fields", "do": [{"op": "remove", "paths": ["/logit_bias", "/user"]}]}
        ])),
        ..Config::default()
    })
    .await;
    let upstream = chat_with_extras(&env).await;
    assert!(
        upstream.get("logit_bias").is_none(),
        "配置字段应被剥：{upstream}"
    );
    assert!(upstream.get("user").is_none(), "配置字段应被剥");
    assert_eq!(upstream["temperature"], 0.5, "未配置字段照传");
    assert!(upstream.get("model").is_some());
    assert!(upstream.get("messages").is_some());
}

/// 未配置渠道：全部原样透传。
#[tokio::test]
async fn passes_through_without_config() {
    let env = setup(Config::default()).await;
    let upstream = chat_with_extras(&env).await;
    assert_eq!(upstream["logit_bias"]["50256"], -100, "缺省应原样透传");
    assert_eq!(upstream["user"], "end-user-1");
}

/// 同一份规则里先剥后注（文档顺序）：强制覆盖 temperature、user。
#[tokio::test]
async fn injects_after_strip() {
    let env = setup(Config {
        channel_rules: Some(json!([
            {"id": "strip-request-fields", "do": [{"op": "remove", "paths": ["/logit_bias"]}]},
            {"id": "inject-request-fields", "do": [
                {"op": "set", "path": "/temperature", "value": 0.1},
                {"op": "set", "path": "/user", "value": "forced"}
            ]}
        ])),
        ..Config::default()
    })
    .await;
    let upstream = chat_with_extras(&env).await;
    assert!(upstream.get("logit_bias").is_none());
    assert_eq!(upstream["temperature"], 0.1);
    assert_eq!(upstream["user"], "forced");
    assert_eq!(upstream["model"], env.model, "model 受保护，原样不动");
}

/// 被手改过的库里躺着想剥 / 改主链字段的规则集：加载时校验不放行，这一层整个跳过，
/// 请求照常成功且主链字段、其余字段都原样。
#[tokio::test]
async fn a_stored_rule_set_touching_protected_fields_is_ignored_not_fatal() {
    let env = setup(Config {
        raw_channel_rules: Some(json!([
            {"id": "hijack", "do": [{"op": "set", "path": "/model", "value": "hijack"}]},
            {"id": "strip", "do": [{"op": "remove", "paths": ["/messages", "/user"]}]}
        ])),
        ..Config::default()
    })
    .await;
    let upstream = chat_with_extras(&env).await;
    assert_eq!(upstream["model"], env.model);
    assert!(upstream.get("messages").is_some());
    assert_eq!(
        upstream["user"], "end-user-1",
        "坏规则集整层跳过，连合法部分也不执行"
    );
}

fn developer_chat(env: &TestEnv) -> Value {
    json!({
        "model": env.model,
        "messages": [
            {"role": "developer", "content": "be brief"},
            {"role": "user", "content": "hi"}
        ]
    })
}

/// 厂商层只对**选了它**的渠道生效：同样的请求，没选厂商时 developer 角色原样发给上游。
#[tokio::test]
async fn the_vendor_layer_applies_only_to_channels_that_chose_it() {
    let plain = setup(Config::default()).await;
    let upstream = chat(&plain, developer_chat(&plain)).await;
    assert_eq!(upstream["messages"][0]["role"], "developer");

    let chosen = setup(Config {
        vendor: Some("builtin:deepseek"),
        ..Config::default()
    })
    .await;
    let upstream = chat(&chosen, developer_chat(&chosen)).await;
    assert_eq!(
        upstream["messages"][0]["role"], "system",
        "严格 role 厂商拒收 developer"
    );
    assert_eq!(upstream["messages"][1]["role"], "user");
}

/// 护栏管到所有渠道且最后执行：渠道层写进去的 user 被护栏删掉。
#[tokio::test]
async fn the_guard_runs_last_over_every_channel() {
    let env = setup(Config {
        channel_rules: Some(json!([
            {"id": "put-user", "do": [{"op": "set", "path": "/user", "value": "channel"}]}
        ])),
        guard: Some(json!([
            {"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}
        ])),
        ..Config::default()
    })
    .await;
    let upstream = chat_with_extras(&env).await;
    assert!(upstream.get("user").is_none(), "{upstream}");
    assert_eq!(upstream["temperature"], 0.5);

    // 没有任何渠道层规则、没选厂商的渠道同样被护栏管到
    let bystander = setup(Config {
        guard: Some(json!([{"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}])),
        ..Config::default()
    })
    .await;
    assert!(chat_with_extras(&bystander).await.get("user").is_none());
}

/// 命中的规则与用到的各层版本进诊断（`usage_details.diagnostics.compat`），回放时能对上。
#[tokio::test]
async fn diagnostics_record_the_layers_and_the_fired_rules() {
    let env = setup(Config {
        vendor: Some("builtin:deepseek"),
        channel_rules: Some(json!([
            {"id": "strip-request-fields", "do": [{"op": "remove", "paths": ["/user"]}]}
        ])),
        ..Config::default()
    })
    .await;
    chat(&env, {
        let mut body = developer_chat(&env);
        body["user"] = json!("u");
        body
    })
    .await;
    let mut compat = Value::Null;
    for _ in 0..50 {
        compat = sqlx::query_scalar(
            "SELECT usage_details->'diagnostics'->'compat' FROM billing_records
              WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(env.user_id)
        .fetch_optional(&env.pg)
        .await
        .unwrap()
        .flatten()
        .unwrap_or(Value::Null);
        if !compat.is_null() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(compat["channel_id"].is_i64(), "{compat}");
    let versions = compat["versions"].as_array().expect("versions");
    assert_eq!(versions[0], "builtin:deepseek");
    assert!(
        versions[1].as_str().unwrap().starts_with("channel#"),
        "{compat}"
    );
    let fired: Vec<&str> = compat["fired"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert_eq!(
        fired,
        ["strict-role.developer-to-system", "strip-request-fields"]
    );
    assert!(compat["skipped"].as_array().unwrap().is_empty(), "{compat}");
    assert!(
        !compat.to_string().contains("be brief"),
        "诊断只记规则 id，不记请求内容"
    );
}
