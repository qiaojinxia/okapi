//! 适配规则 P2 的响应 / 错误 / 能力阶段在聊天链路上的验收：
//! 响应改写（非流式与流式）、`usage_map` 真的改变了账单、错误阶段按规则重发 / 换渠道 / 改写错误、
//! 能力事实让候选在发出任何请求之前就被排除、命中轨迹进诊断。依赖 .env（scripts/dev-deps.sh up）。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// 上游对一次请求的回应。
#[derive(Clone)]
enum Reply {
    Json(Value),
    /// 逐条 SSE data 负载（`[DONE]` 由 mock 补）。
    Sse(Vec<Value>),
    Status(u16, Value),
}

type Script = Arc<dyn Fn(usize, &Value) -> Reply + Send + Sync>;

struct Mock {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Value>>>,
}

async fn spawn_mock(script: Script) -> Mock {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::body::Bytes| {
            let recorded = Arc::clone(&recorded);
            let script = Arc::clone(&script);
            async move {
                let request: Value = serde_json::from_slice(&body).unwrap();
                let index = {
                    let mut seen = recorded.lock().unwrap();
                    seen.push(request.clone());
                    seen.len() - 1
                };
                match script(index, &request) {
                    Reply::Json(body) => axum::Json(body).into_response(),
                    Reply::Status(status, body) => {
                        (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
                    }
                    Reply::Sse(chunks) => {
                        use std::fmt::Write as _;
                        let mut text = String::new();
                        for chunk in chunks {
                            let _ = write!(text, "data: {chunk}\n\n");
                        }
                        text.push_str("data: [DONE]\n\n");
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            text,
                        )
                            .into_response()
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Mock { addr, seen }
}

/// 用量自洽（total = prompt + completion）的普通回包。
fn completion() -> Value {
    json!({
        "id": "cmpl", "object": "chat.completion", "model": "m",
        "choices": [{"index": 0, "finish_reason": "stop",
                     "message": {"role": "assistant", "content": "ok", "reasoning": "think"}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

/// xAI 一类的回包：completion_tokens 不含推理 token，total_tokens 才是全部（10 + 5 + 10）。
/// 网关对 total ≠ prompt + completion 的用量一律当无效，所以不加规则这种回包会被拒。
fn xai_completion() -> Value {
    let mut body = completion();
    body["usage"] = json!({
        "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 25,
        "completion_tokens_details": {"reasoning_tokens": 10}
    });
    body
}

fn stream_chunks_with(usage: &Value) -> Vec<Value> {
    vec![
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
               "choices": [{"index": 0, "delta": {"role": "assistant", "content": "", "reasoning": "think"}}]}),
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
               "choices": [{"index": 0, "delta": {"content": "hi"}}]}),
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
               "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m", "choices": [], "usage": usage}),
    ]
}

fn stream_chunks() -> Vec<Value> {
    stream_chunks_with(&completion()["usage"])
}

fn always(reply: Reply) -> Script {
    Arc::new(move |_, _| reply.clone())
}

/// 一个渠道的配置。
struct Channel {
    mock: Mock,
    priority: i32,
    /// 渠道层规则（走正常发布路径，经过校验）。
    rules: Option<Value>,
}

struct TestEnv {
    gateway: SocketAddr,
    token: String,
    model: String,
    pg: sqlx::PgPool,
    user_id: i64,
}

async fn setup(channels: &[Channel]) -> TestEnv {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("cp-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-cp-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-cp")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();

    for (i, channel) in channels.iter().enumerate() {
        let (channel_id, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("cp-ch-{i}-{suffix}"),
            "openai",
            &format!("http://{}/v1", channel.mock.addr),
            "mock-credential",
            &[model.as_str()],
            true,
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE channels SET priority = $2 WHERE id = $1")
            .bind(channel_id)
            .bind(channel.priority)
            .execute(&pg)
            .await
            .unwrap();
        if let Some(rules) = &channel.rules {
            let mut conn = pg.acquire().await.unwrap();
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
    }

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
        pg,
        user_id,
    }
}

async fn post_chat(env: &TestEnv, mut body: Value) -> (u16, String) {
    body["model"] = json!(env.model);
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap())
}

fn hello() -> Value {
    json!({"messages": [{"role": "user", "content": "hi"}]})
}

fn streamed() -> Value {
    json!({"messages": [{"role": "user", "content": "hi"}], "stream": true,
           "stream_options": {"include_usage": true}})
}

/// SSE 文本里每条 data 负载（不含 `[DONE]`）。
fn sse_chunks(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| data.trim() != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

/// 等账单落库，取最近一条记账的补全 token 数。
async fn billed_completion_tokens(env: &TestEnv) -> i32 {
    for _ in 0..80 {
        if let Some(value) = sqlx::query_scalar::<_, i32>(
            "SELECT completion_tokens FROM billing_records WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
            .bind(env.user_id)
            .fetch_optional(&env.pg)
            .await
            .unwrap()
        {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("账单没有落库");
}

async fn diagnostics(env: &TestEnv, key: &str) -> Value {
    let sql = "SELECT usage_details->'diagnostics'->$2 FROM billing_records
                WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1";
    for _ in 0..80 {
        let value: Option<Option<Value>> = sqlx::query_scalar(sql)
            .bind(env.user_id)
            .bind(key)
            .fetch_optional(&env.pg)
            .await
            .unwrap();
        if let Some(Some(value)) = value
            && !value.is_null()
        {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Value::Null
}

fn alias_reasoning() -> Value {
    json!([{
        "id": "alias-reasoning", "phase": "response", "select": {"dialect": ["openai.chat"]},
        "do": [{"op": "each", "path": "/choices", "do": [
            {"op": "copy", "from": "/message/reasoning", "to": "/message/reasoning_content", "mode": "if_absent"},
            {"op": "rename", "from": "/delta/reasoning", "to": "/delta/reasoning_content"}
        ]}]
    }])
}

fn completion_from_total() -> Value {
    json!([{
        "id": "completion-from-total", "phase": "response", "select": {"dialect": ["openai.chat"]},
        "billing_critical_ack": true,
        "do": [{"op": "usage_map", "slots": {"completion": "total - prompt"}}]
    }])
}

// ------------------------------------------------------------------------------ 响应阶段

#[tokio::test]
async fn a_response_rule_rewrites_json_and_stream_before_the_client_sees_them() {
    let json_env = setup(&[Channel {
        mock: spawn_mock(always(Reply::Json(completion()))).await,
        priority: 0,
        rules: Some(alias_reasoning()),
    }])
    .await;
    let (status, text) = post_chat(&json_env, hello()).await;
    assert_eq!(status, 200, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["choices"][0]["message"]["reasoning_content"], "think");
    assert_eq!(
        body["choices"][0]["message"]["reasoning"], "think",
        "copy 不删来源"
    );
    assert_eq!(body["choices"][0]["message"]["content"], "ok");

    let stream_env = setup(&[Channel {
        mock: spawn_mock(always(Reply::Sse(stream_chunks()))).await,
        priority: 0,
        rules: Some(alias_reasoning()),
    }])
    .await;
    let (status, text) = post_chat(&stream_env, streamed()).await;
    assert_eq!(status, 200, "{text}");
    let chunks = sse_chunks(&text);
    assert_eq!(
        chunks[0]["choices"][0]["delta"]["reasoning_content"],
        "think"
    );
    assert!(
        chunks[0]["choices"][0]["delta"].get("reasoning").is_none(),
        "rename 把来源改走了"
    );
    assert_eq!(
        chunks[1]["choices"][0]["delta"]["content"], "hi",
        "没有命中的帧原样放行"
    );
    assert_eq!(chunks.last().unwrap()["usage"]["total_tokens"], 15);
}

#[tokio::test]
async fn a_channel_without_response_rules_passes_everything_through_untouched() {
    let env = setup(&[Channel {
        mock: spawn_mock(always(Reply::Json(completion()))).await,
        priority: 0,
        rules: None,
    }])
    .await;
    let (_, text) = post_chat(&env, hello()).await;
    let body: Value = serde_json::from_str(&text).unwrap();
    assert!(
        body["choices"][0]["message"]
            .get("reasoning_content")
            .is_none()
    );
}

#[tokio::test]
async fn usage_map_makes_an_xai_style_usage_billable_for_json_and_stream() {
    // 对照：没有规则时，total ≠ prompt + completion 的用量被网关当无效拒掉，客户端拿到 502
    let control = setup(&[Channel {
        mock: spawn_mock(always(Reply::Json(xai_completion()))).await,
        priority: 0,
        rules: None,
    }])
    .await;
    let (status, text) = post_chat(&control, hello()).await;
    assert_eq!(status, 502, "{text}");

    let json_env = setup(&[Channel {
        mock: spawn_mock(always(Reply::Json(xai_completion()))).await,
        priority: 0,
        rules: Some(completion_from_total()),
    }])
    .await;
    let (status, text) = post_chat(&json_env, hello()).await;
    assert_eq!(status, 200, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        body["usage"]["completion_tokens"], 15,
        "客户端看到的用量也是改过的"
    );
    assert_eq!(billed_completion_tokens(&json_env).await, 15);

    let stream_env = setup(&[Channel {
        mock: spawn_mock(always(Reply::Sse(stream_chunks_with(
            &xai_completion()["usage"],
        ))))
        .await,
        priority: 0,
        rules: Some(completion_from_total()),
    }])
    .await;
    let (status, text) = post_chat(&stream_env, streamed()).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(
        sse_chunks(&text).last().unwrap()["usage"]["completion_tokens"],
        15
    );
    assert_eq!(billed_completion_tokens(&stream_env).await, 15);
}

#[tokio::test]
async fn diagnostics_name_the_response_rules_that_fired() {
    let env = setup(&[Channel {
        mock: spawn_mock(always(Reply::Json(completion()))).await,
        priority: 0,
        rules: Some(alias_reasoning()),
    }])
    .await;
    post_chat(&env, hello()).await;
    let record = diagnostics(&env, "compat_response").await;
    assert!(record["channel_id"].is_i64(), "{record}");
    assert_eq!(record["fired"][0]["rule"], "alias-reasoning", "{record}");
    assert!(
        !record.to_string().contains("think"),
        "诊断只记规则 id，不记响应内容"
    );
}

// ------------------------------------------------------------------------------ 错误阶段

fn reject_user_field() -> Script {
    Arc::new(|_, request| {
        if request.get("user").is_some() {
            Reply::Status(
                400,
                json!({"error": {"message": "Unsupported parameter: user", "type": "invalid_request_error"}}),
            )
        } else {
            Reply::Json(completion())
        }
    })
}

fn drop_unsupported_param() -> Value {
    json!([{
        "id": "drop-unsupported-param", "phase": "error",
        "when": {"status": [400], "body_contains": ["unsupported parameter"]},
        "do": [{"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}]}]
    }])
}

#[tokio::test]
async fn an_error_rule_resends_a_mutated_request_on_the_same_candidate() {
    // 对照：没有规则，上游的 400 原样回给客户端，只发了一次
    let control_mock = spawn_mock(reject_user_field()).await;
    let control = setup(&[Channel {
        mock: control_mock,
        priority: 0,
        rules: None,
    }])
    .await;
    let mut body = hello();
    body["user"] = json!("end-user");
    let (status, text) = post_chat(&control, body.clone()).await;
    assert_eq!(status, 400, "{text}");

    let mock = spawn_mock(reject_user_field()).await;
    let seen = Arc::clone(&mock.seen);
    let env = setup(&[Channel {
        mock,
        priority: 0,
        rules: Some(drop_unsupported_param()),
    }])
    .await;
    let (status, text) = post_chat(&env, body).await;
    assert_eq!(status, 200, "改请求重发后成功：{text}");
    // 同一候选收到两次：第一次带 user，第二次不带
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0].get("user").is_some() && seen[1].get("user").is_none(),
        "{seen:?}"
    );
    let record = diagnostics(&env, "compat_error").await;
    assert_eq!(record["rule"], "drop-unsupported-param", "{record}");
    assert_eq!(record["action"], "retry_same");
    assert_eq!(record["upstream_status"], 400);
}

#[tokio::test]
async fn the_resend_budget_stops_rules_from_looping() {
    // 上游无论如何都回 400；四条规则各删一个字段。每条规则默认只能重发一次，全部规则合计在同一个候选上
    // 最多重发 3 次：上游一共收到 1 + 3 = 4 个请求，最后一次还带着第四个字段，然后 400 回给客户端
    let mock = spawn_mock(always(Reply::Status(
        400,
        json!({"error": {"message": "Unsupported parameter: always"}}),
    )))
    .await;
    let seen = Arc::clone(&mock.seen);
    let rule = |id: &str, field: &str| {
        json!({"id": id, "phase": "error", "when": {"status": [400]},
               "do": [{"op": "retry_same", "mutate": [{"op": "remove", "paths": [format!("/{field}")]}]}]})
    };
    let rules = json!([
        rule("r1", "a"),
        rule("r2", "b"),
        rule("r3", "c"),
        rule("r4", "d")
    ]);
    let env = setup(&[Channel {
        mock,
        priority: 0,
        rules: Some(rules),
    }])
    .await;
    let mut body = hello();
    for field in ["a", "b", "c", "d"] {
        body[field] = json!(1);
    }
    let (status, _) = post_chat(&env, body).await;
    assert_eq!(status, 400);
    let seen = seen.lock().unwrap().clone();
    let shapes: Vec<Vec<&str>> = seen
        .iter()
        .map(|request| {
            ["a", "b", "c", "d"]
                .into_iter()
                .filter(|f| request.get(*f).is_some())
                .collect()
        })
        .collect();
    assert_eq!(
        shapes,
        [
            vec!["a", "b", "c", "d"],
            vec!["b", "c", "d"],
            vec!["c", "d"],
            vec!["d"]
        ]
    );
}

#[tokio::test]
async fn an_error_rule_can_send_the_request_to_the_next_channel_instead() {
    let not_here = spawn_mock(always(Reply::Status(
        404,
        json!({"error": {"code": "model_not_found", "message": "no such model on this account"}}),
    )))
    .await;
    let fine = spawn_mock(always(Reply::Json(completion()))).await;

    // 对照：没有规则，第一个渠道的 404 就是最终答案
    let control_a = spawn_mock(always(Reply::Status(
        404,
        json!({"error": {"code": "model_not_found"}}),
    )))
    .await;
    let control_b = spawn_mock(always(Reply::Json(completion()))).await;
    let control = setup(&[
        Channel {
            mock: control_a,
            priority: 10,
            rules: None,
        },
        Channel {
            mock: control_b,
            priority: 0,
            rules: None,
        },
    ])
    .await;
    let (status, _) = post_chat(&control, hello()).await;
    assert_eq!(status, 404);

    let rules = json!([{
        "id": "model-not-on-this-account", "phase": "error",
        "when": {"status": [404], "code": ["model_not_found"]},
        "do": [{"op": "failover"}]
    }]);
    let env = setup(&[
        Channel {
            mock: not_here,
            priority: 10,
            rules: Some(rules),
        },
        Channel {
            mock: fine,
            priority: 0,
            rules: None,
        },
    ])
    .await;
    let (status, text) = post_chat(&env, hello()).await;
    assert_eq!(status, 200, "{text}");
}

#[tokio::test]
async fn an_error_rule_can_rewrite_the_error_the_client_gets() {
    let bad = |_: usize, _: &Value| {
        Reply::Status(
            400,
            json!({"error": {"message": "upstream says: context window exceeded 8192"}}),
        )
    };
    let rules = json!([{
        "id": "context-too-long", "phase": "error",
        "when": {"status": [400], "body_contains": ["context window"]},
        "do": [{"op": "fail", "status": 413, "code": "context_length_exceeded"}]
    }]);
    let env = setup(&[Channel {
        mock: spawn_mock(Arc::new(bad)).await,
        priority: 0,
        rules: Some(rules),
    }])
    .await;
    let (status, text) = post_chat(&env, hello()).await;
    assert_eq!(status, 413, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["error"]["code"], "context_length_exceeded", "{text}");
    assert!(!text.contains("8192"), "上游的自然语言细节不再外露：{text}");

    // 只给 status：保留上游的错误体，只换状态码
    let rules = json!([{
        "id": "teapot", "phase": "error", "when": {"status": [400]},
        "do": [{"op": "fail", "status": 422}]
    }]);
    let env = setup(&[Channel {
        mock: spawn_mock(Arc::new(bad)).await,
        priority: 0,
        rules: Some(rules),
    }])
    .await;
    let (status, text) = post_chat(&env, hello()).await;
    assert_eq!(status, 422, "{text}");
    assert!(text.contains("context window exceeded 8192"), "{text}");
}

// ------------------------------------------------------------------------------ 能力阶段

#[tokio::test]
async fn a_capability_rule_excludes_a_candidate_before_any_request_is_sent() {
    let no_tools = spawn_mock(always(Reply::Json(completion()))).await;
    let capable = spawn_mock(always(Reply::Json(completion()))).await;
    let rules = json!([{
        "id": "no-tools", "phase": "capability",
        "do": [{"op": "capability", "facts": {"tools": false}}]
    }]);
    let first_hits = Arc::clone(&no_tools.seen);
    let env = setup(&[
        Channel {
            mock: no_tools,
            priority: 10,
            rules: Some(rules),
        },
        Channel {
            mock: capable,
            priority: 0,
            rules: None,
        },
    ])
    .await;
    let mut with_tools = hello();
    with_tools["tools"] =
        json!([{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}]);
    let (status, text) = post_chat(&env, with_tools).await;
    assert_eq!(status, 200, "{text}");
    assert!(
        first_hits.lock().unwrap().is_empty(),
        "声明了不支持工具的渠道一个请求也没收到"
    );

    // 没带工具的请求照常走优先级最高的渠道（换一段对话：同一段对话会按会话粘性回到刚才服务它的渠道）
    let (status, _) = post_chat(
        &env,
        json!({"messages": [{"role": "user", "content": "a completely different conversation"}]}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(first_hits.lock().unwrap().len(), 1);
}
