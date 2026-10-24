//! 缺省输出上限（预扣封顶值）写进上游请求的位置。
//! 客户端没声明上限、模型 max_output 又大于封顶值时，转发前要把封顶值写进请求，免得上游按
//! max_output 生成、结算时透支预扣。这个值必须先写进入口方言的原文、再由出向转换器翻译成
//! 上游自己的字段；在转换之后按入口方言写，跨方言时会落成上游不认的键（Gemini 上游多出顶层
//! max_tokens，Anthropic / chat 上游多出 max_output_tokens 或 generationConfig），
//! OpenAI 推理模型还会因为"改名在前、写入在后"收到被拒的 max_tokens。
//! 依赖 .env 的 DATABASE_URL / OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// 预扣封顶值（chat.rs 的 `MAX_COMPLETION_CAP`）。
const CAP: u64 = 32_768;

/// 上游收到的请求体，按到达顺序记。
type Seen = Arc<Mutex<Vec<Value>>>;

fn record(seen: &Seen, body: &[u8]) {
    seen.lock()
        .unwrap()
        .push(serde_json::from_slice(body).unwrap());
}

// ---- mock 上游：四种方言各一个端点，只记请求体、回最小的合法响应 ----

async fn chat(State(seen): State<Seen>, body: axum::body::Bytes) -> axum::response::Response {
    record(&seen, &body);
    axum::Json(json!({
        "id": "c1", "object": "chat.completion", "model": "up",
        "choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "ok"}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    }))
    .into_response()
}

async fn responses(State(seen): State<Seen>, body: axum::body::Bytes) -> axum::response::Response {
    record(&seen, &body);
    axum::Json(json!({
        "id": "resp_1", "object": "response", "status": "completed", "model": "up",
        "output": [{"type": "message", "id": "msg_1", "status": "completed", "role": "assistant",
            "content": [{"type": "output_text", "text": "ok", "annotations": []}]}],
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
    }))
    .into_response()
}

async fn messages(State(seen): State<Seen>, body: axum::body::Bytes) -> axum::response::Response {
    record(&seen, &body);
    axum::Json(json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "up",
        "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 5}
    }))
    .into_response()
}

async fn generate(
    State(seen): State<Seen>,
    Path(_action): Path<String>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    record(&seen, &body);
    axum::Json(json!({
        "responseId": "r1",
        "candidates": [{"content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5}
    }))
    .into_response()
}

// ---- 环境 ----

/// 每个公开模型名对应一种上游方言（渠道各一个），都声明了大于封顶值的 `max_output`。
struct Models {
    /// openai 渠道，`responses_native:false`：Responses 入口降级成 chat。
    chat: String,
    /// openai 渠道默认直转 Responses。
    native: String,
    /// 映射到 `o3-mini`：OpenAI 推理模型，chat 上游要 `max_completion_tokens`。
    reasoning: String,
    claude: String,
    gemini: String,
    /// chat 渠道，`max_output` 不超过封顶值：不该补任何上限。
    small: String,
}

struct Env {
    gateway: SocketAddr,
    token: String,
    seen: Seen,
    models: Models,
}

/// `max_output` 大于预扣封顶值的模型：客户端没写上限时要补封顶值。
const BIG_OUTPUT: i32 = 128_000;

/// 一个公开模型名和它背后的那条渠道。
struct Spec<'a> {
    label: &'a str,
    provider: &'a str,
    /// 渠道地址在 mock 上游下的路径前缀。
    path: &'a str,
    /// `channels.settings`。
    settings: Value,
    /// 渠道把公开模型名映射到的上游模型名。
    upstream_model: Option<&'a str>,
    max_output: i32,
}

impl<'a> Spec<'a> {
    fn new(label: &'a str, provider: &'a str, path: &'a str) -> Self {
        Self {
            label,
            provider,
            path,
            settings: json!({}),
            upstream_model: None,
            max_output: BIG_OUTPUT,
        }
    }

    /// Responses 入口降级成 chat，而不是直转。
    fn degraded(mut self) -> Self {
        self.settings = json!({"responses_native": false});
        self
    }

    fn mapped_to(mut self, model: &'a str) -> Self {
        self.upstream_model = Some(model);
        self
    }

    fn max_output(mut self, tokens: i32) -> Self {
        self.max_output = tokens;
        self
    }
}

async fn add_model(pg: &PgPool, suffix: &str, upstream: SocketAddr, spec: Spec<'_>) -> String {
    let model = format!("m-{}-{}", spec.label, &suffix[..8]);
    okapi_store::provision::create_model_ratio(pg, &model, "1", "1", "1")
        .await
        .unwrap();
    sqlx::query("UPDATE models SET max_output = $2::int WHERE model_name = $1")
        .bind(&model)
        .bind(spec.max_output)
        .execute(pg)
        .await
        .unwrap();
    let (channel_id, _) = okapi_store::provision::create_channel(
        pg,
        &format!("ch-{}-{suffix}", spec.label),
        spec.provider,
        &format!("http://{upstream}{}", spec.path),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channels SET settings = $2 WHERE id = $1")
        .bind(channel_id)
        .bind(&spec.settings)
        .execute(pg)
        .await
        .unwrap();
    if let Some(mapped) = spec.upstream_model {
        sqlx::query("UPDATE channels SET model_mapping = $2 WHERE id = $1")
            .bind(channel_id)
            .bind(json!({ model.as_str(): mapped }))
            .execute(pg)
            .await
            .unwrap();
    }
    model
}

async fn add_models(pg: &PgPool, suffix: &str, upstream: SocketAddr) -> Models {
    let openai = "/openai/v1";
    Models {
        chat: add_model(
            pg,
            suffix,
            upstream,
            Spec::new("chat", "openai", openai).degraded(),
        )
        .await,
        native: add_model(pg, suffix, upstream, Spec::new("native", "openai", openai)).await,
        reasoning: add_model(
            pg,
            suffix,
            upstream,
            Spec::new("reasoning", "openai", openai)
                .degraded()
                .mapped_to("o3-mini"),
        )
        .await,
        claude: add_model(
            pg,
            suffix,
            upstream,
            Spec::new("claude", "anthropic", "/claude/v1"),
        )
        .await,
        gemini: add_model(
            pg,
            suffix,
            upstream,
            Spec::new("gemini", "gemini", "/gemini/v1beta"),
        )
        .await,
        small: add_model(
            pg,
            suffix,
            upstream,
            Spec::new("small", "openai", openai)
                .degraded()
                .max_output(8_192),
        )
        .await,
    }
}

async fn spawn_upstream() -> (SocketAddr, Seen) {
    let seen = Seen::default();
    let router = Router::new()
        .route("/openai/v1/chat/completions", post(chat))
        .route("/openai/v1/responses", post(responses))
        .route("/claude/v1/messages", post(messages))
        .route("/gemini/v1beta/models/{action}", post(generate))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (address, seen)
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let suffix = Uuid::new_v4().simple().to_string();

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-test-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();

    let (upstream, seen) = spawn_upstream().await;
    let models = add_models(&pg, &suffix, upstream).await;

    published_pricing::publish(&pg, user_id).await;
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_000_000_000))
        .await
        .unwrap();
    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Env {
        gateway,
        token,
        seen,
        models,
    }
}

// ---- 请求与断言 ----

#[derive(Clone, Copy, Debug)]
enum Ingress {
    OpenAi,
    Responses,
    Gemini,
    Anthropic,
}

/// 上游请求体里可能带输出上限的四个位置；每种上游方言只认其中一个。
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cap {
    /// chat 与 Anthropic Messages：顶层 `max_tokens`。
    Tokens,
    /// OpenAI 推理模型的 chat：顶层 `max_completion_tokens`。
    CompletionTokens,
    /// Responses：顶层 `max_output_tokens`。
    OutputTokens,
    /// Gemini：`generationConfig.maxOutputTokens`。
    GenerationConfig,
}

const ALL: [Cap; 4] = [
    Cap::Tokens,
    Cap::CompletionTokens,
    Cap::OutputTokens,
    Cap::GenerationConfig,
];

impl Cap {
    fn read(self, body: &Value) -> Option<&Value> {
        match self {
            Cap::Tokens => body.get("max_tokens"),
            Cap::CompletionTokens => body.get("max_completion_tokens"),
            Cap::OutputTokens => body.get("max_output_tokens"),
            Cap::GenerationConfig => body.pointer("/generationConfig/maxOutputTokens"),
        }
    }
}

/// 以入口方言发一条没有其它字段的请求（`cap` 是客户端显式写的上限，None = 不写），
/// 返回上游实际收到的请求体。
async fn send(env: &Env, ingress: Ingress, model: &str, cap: Option<u64>) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let base = format!("http://{}", env.gateway);
    let request = match ingress {
        Ingress::OpenAi => {
            let mut body = json!({"model": model,
                "messages": [{"role": "user", "content": "hi there"}]});
            if let Some(cap) = cap {
                body["max_tokens"] = json!(cap);
            }
            client
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(&env.token)
                .json(&body)
        }
        Ingress::Responses => {
            let mut body = json!({"model": model, "input": "hi there"});
            if let Some(cap) = cap {
                body["max_output_tokens"] = json!(cap);
            }
            client
                .post(format!("{base}/v1/responses"))
                .bearer_auth(&env.token)
                .json(&body)
        }
        Ingress::Gemini => {
            let mut body = json!({"contents": [{"role": "user", "parts": [{"text": "hi there"}]}]});
            if let Some(cap) = cap {
                body["generationConfig"] = json!({"maxOutputTokens": cap});
            }
            client
                .post(format!("{base}/v1beta/models/{model}:generateContent"))
                .bearer_auth(&env.token)
                .json(&body)
        }
        Ingress::Anthropic => {
            let mut body = json!({"model": model,
                "messages": [{"role": "user", "content": "hi there"}]});
            if let Some(cap) = cap {
                body["max_tokens"] = json!(cap);
            }
            client
                .post(format!("{base}/v1/messages"))
                .header("x-api-key", &env.token)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
        }
    };
    env.seen.lock().unwrap().clear();
    let resp = request.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("HTTP {status}: {text}"));
    }
    let mut seen = env.seen.lock().unwrap();
    if seen.len() != 1 {
        return Err(format!("上游应恰好收到一条请求，实际 {}", seen.len()));
    }
    Ok(seen.pop().unwrap())
}

/// 上游只在 `want` 这个位置带 `expected`，其余三个位置都不能出现。
fn check(label: &str, body: &Value, want: Cap, expected: u64) -> Vec<String> {
    let mut failures = Vec::new();
    for cap in ALL {
        let got = cap.read(body);
        if cap == want {
            if got != Some(&json!(expected)) {
                failures.push(format!(
                    "{label}: {cap:?} 应为 {expected}，实际 {got:?}；{body}"
                ));
            }
        } else if got.is_some() {
            failures.push(format!("{label}: 上游不认 {cap:?}，却收到 {got:?}；{body}"));
        }
    }
    failures
}

/// 没声明上限、`max_output` 大于封顶值：封顶值落在每种"入口 × 上游"组合的上游自有字段上；
/// 声明了的上限原样翻译。此前 Gemini / chat / Anthropic 上游会收到入口方言的字段名，
/// 推理模型的 chat 上游会收到 `max_tokens`。
#[tokio::test]
async fn default_cap_lands_in_the_field_the_upstream_dialect_reads() {
    use Cap::{CompletionTokens, GenerationConfig, OutputTokens, Tokens};
    use Ingress::{Anthropic, Gemini, OpenAi, Responses};
    let env = setup().await;
    let m = &env.models;
    // (入口, 上游标签, 公开模型名, 客户端声明的上限, 上游自己读上限的位置)
    let cases: Vec<(Ingress, &str, &str, Option<u64>, Cap)> = vec![
        // 没声明：补封顶值
        (OpenAi, "chat", &m.chat, None, Tokens),
        (OpenAi, "reasoning", &m.reasoning, None, CompletionTokens),
        (OpenAi, "claude", &m.claude, None, Tokens),
        (OpenAi, "gemini", &m.gemini, None, GenerationConfig),
        (Responses, "chat", &m.chat, None, Tokens),
        (Responses, "native", &m.native, None, OutputTokens),
        (Responses, "claude", &m.claude, None, Tokens),
        (Responses, "gemini", &m.gemini, None, GenerationConfig),
        (Gemini, "chat", &m.chat, None, Tokens),
        (Gemini, "claude", &m.claude, None, Tokens),
        (Gemini, "gemini", &m.gemini, None, GenerationConfig),
        (Anthropic, "chat", &m.chat, None, Tokens),
        (Anthropic, "reasoning", &m.reasoning, None, CompletionTokens),
        (Anthropic, "claude", &m.claude, None, Tokens),
        // 声明了：原样翻译，不被封顶值覆盖
        (
            OpenAi,
            "reasoning",
            &m.reasoning,
            Some(50),
            CompletionTokens,
        ),
        (OpenAi, "gemini", &m.gemini, Some(50), GenerationConfig),
        (Responses, "claude", &m.claude, Some(50), Tokens),
        (Gemini, "chat", &m.chat, Some(50), Tokens),
    ];
    let mut failures = Vec::new();
    for (ingress, upstream, model, declared, want) in cases {
        let label = format!("{ingress:?} → {upstream}（声明 {declared:?}）");
        match send(&env, ingress, model, declared).await {
            Ok(body) => failures.extend(check(&label, &body, want, declared.unwrap_or(CAP))),
            Err(error) => failures.push(format!("{label}: {error}")),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// `max_output` 不超过封顶值的模型生成不到更多：任何入口都不往上游补上限。
#[tokio::test]
async fn small_models_get_no_default_cap_in_any_dialect() {
    let env = setup().await;
    let model = &env.models.small;
    let mut failures = Vec::new();
    for ingress in [
        Ingress::OpenAi,
        Ingress::Responses,
        Ingress::Gemini,
        Ingress::Anthropic,
    ] {
        match send(&env, ingress, model, None).await {
            Ok(body) => {
                for cap in ALL {
                    if let Some(got) = cap.read(&body) {
                        failures.push(format!("{ingress:?}: 不该补 {cap:?}，却收到 {got}；{body}"));
                    }
                }
            }
            Err(error) => failures.push(format!("{ingress:?}: {error}")),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
