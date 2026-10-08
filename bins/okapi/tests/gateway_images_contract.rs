//! Image validation, exact cardinality billing and non-idempotent retry boundaries.
//! Real gateway/PG/Redis; controlled HTTP upstream, no external provider credentials.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{FromRequest, Request},
    http::HeaderMap,
    response::Response,
    routing::post,
};
use okapi::{gateway, gateway::state::AppState};
use okapi_domain::Money;
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use uuid::Uuid;

#[path = "support/image_tasks_cases.rs"]
mod async_tasks;

#[path = "support/image_token_billing.rs"]
mod token_billing;

#[path = "support/image_cache_billing.rs"]
mod cache_billing;

#[path = "support/image_cost_source.rs"]
mod cost_source;
#[path = "support/image_streaming.rs"]
mod streaming;

#[path = "support/published_pricing.rs"]
mod published_pricing;

const WAIT: Duration = Duration::from_secs(10);
const BALANCE: i64 = 1_000_000;
const PRICE: i64 = 40_000;

#[derive(Debug)]
struct Upload {
    name: String,
    filename: Option<String>,
    mime: Option<String>,
    bytes: Bytes,
}
struct Pending {
    path: String,
    headers: HeaderMap,
    body: Value,
    uploads: Vec<Upload>,
    reply: oneshot::Sender<Response>,
}
impl Pending {
    fn raw(self, status: u16, body: String) {
        self.reply
            .send(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .header("x-request-id", "image-contract-upstream")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .unwrap();
    }
    fn images(self, count: usize) {
        self.raw(
            200,
            json!({"created":1_700_000_000,"data":(0..count)
            .map(|_|json!({"url":"https://image.example/result.png"})).collect::<Vec<_>>()})
            .to_string(),
        );
    }
}
async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    address
}
struct Env {
    state: AppState,
    address: SocketAddr,
    incoming: mpsc::Receiver<Pending>,
    hits: Arc<AtomicUsize>,
    model: String,
    token: String,
    user: i64,
    key: i64,
    channels: Vec<i64>,
}
struct Mock {
    address: SocketAddr,
    incoming: mpsc::Receiver<Pending>,
    hits: Arc<AtomicUsize>,
}
async fn spawn_upstream() -> Mock {
    let (send, incoming) = mpsc::channel(16);
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let upstream = serve(Router::new().fallback(post(move |req: Request| {
        let send = send.clone();
        count.fetch_add(1, Ordering::SeqCst);
        async move {
            let path = req.uri().path().to_owned();
            let headers = req.headers().clone();
            let mut uploads = Vec::new();
            let body = if headers
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("multipart/")
            {
                let mut form = axum::extract::Multipart::from_request(req, &())
                    .await
                    .unwrap();
                while let Some(field) = form.next_field().await.unwrap() {
                    let name = field.name().unwrap().to_owned();
                    let filename = field.file_name().map(str::to_owned);
                    let mime = field.content_type().map(str::to_owned);
                    let bytes = field.bytes().await.unwrap();
                    uploads.push(Upload {
                        name,
                        filename,
                        mime,
                        bytes,
                    });
                }
                Value::Null
            } else {
                serde_json::from_slice(&Bytes::from_request(req, &()).await.unwrap()).unwrap()
            };
            let (reply, receive) = oneshot::channel();
            send.send(Pending {
                path,
                headers,
                body,
                uploads,
                reply,
            })
            .await
            .unwrap();
            receive
                .await
                .unwrap_or_else(|_| Response::builder().status(503).body(Body::empty()).unwrap())
        }
    })))
    .await;
    Mock {
        address: upstream,
        incoming,
        hits,
    }
}
async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    setup_at(&database, None).await
}

async fn setup_at(
    database: &str,
    storage: Option<Arc<gateway::images::tasks::objects::Storage>>,
) -> Env {
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let model = format!("image-contract-{}", Uuid::new_v4().simple());
    let user = okapi_store::provision::create_user(&pg, &model)
        .await
        .unwrap();
    let token = format!("sk-{model}");
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let key = okapi_store::provision::create_api_key(&pg, user, &hash, "images")
        .await
        .unwrap();
    let model_id =
        sqlx::query_scalar::<_, i64>("INSERT INTO models (model_name) VALUES ($1) RETURNING id")
            .bind(&model)
            .fetch_one(&pg)
            .await
            .unwrap();
    sqlx::query("INSERT INTO model_pricing(model_id, pricing_mode, per_call_price_micro) VALUES ($1,'per_call',$2)")
        .bind(model_id).bind(PRICE).execute(&pg).await.unwrap();
    sqlx::query("UPDATE users SET balance_micro=$2 WHERE id=$1")
        .bind(user)
        .bind(BALANCE)
        .execute(&pg)
        .await
        .unwrap();
    let Mock {
        address: upstream,
        incoming,
        hits,
    } = spawn_upstream().await;
    // Two candidates make any accidental retry visible to the test.
    let mut channels = Vec::new();
    for index in 0..2 {
        let (channel, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("{model}-{index}"),
            "openai",
            &format!("http://{upstream}/v1"),
            "image-credential",
            &[&model],
            false,
            None,
        )
        .await
        .unwrap();
        channels.push(channel);
        sqlx::query("UPDATE channels SET model_mapping=$2 WHERE id=$1")
            .bind(channel)
            .bind(json!({&model:"mapped-image"}))
            .execute(&pg)
            .await
            .unwrap();
    }
    published_pricing::publish(&pg, user).await;
    let mut state = gateway::build_state(database, &redis, &model, None, None)
        .await
        .unwrap();
    if let Some(storage) = storage {
        state.image_storage = storage;
    }
    state
        .ledger
        .credit(user, Money::from_micros(BALANCE))
        .await
        .unwrap();
    let address = serve(gateway::router(state.clone())).await;
    Env {
        state,
        address,
        incoming,
        hits,
        model,
        token,
        user,
        key,
        channels,
    }
}
impl Env {
    async fn provider(&self, provider: &str) {
        sqlx::query("UPDATE channels SET provider=$2 WHERE id=ANY($1)")
            .bind(&self.channels)
            .bind(provider)
            .execute(&self.state.pg)
            .await
            .unwrap();
    }
    fn request(&self, edit: bool) -> reqwest::RequestBuilder {
        reqwest::Client::builder()
            .timeout(WAIT)
            .build()
            .unwrap()
            .post(format!(
                "http://{}/v1/images/{}",
                self.address,
                if edit { "edits" } else { "generations" }
            ))
            .bearer_auth(&self.token)
    }
    fn body(&self, count: u32) -> Value {
        json!({"model":self.model,"prompt":"test image","n":count})
    }
    fn form(&self) -> Form {
        Form::new()
            .text("model", self.model.clone())
            .text("prompt", "test image")
            .part("image", image_part())
    }
    async fn peer(&mut self) -> Pending {
        timeout(WAIT, self.incoming.recv()).await.unwrap().unwrap()
    }
    async fn assert_money(&self, amount: i64, records: i64) {
        self.state.settlements.wait_idle(WAIT).await;
        assert_eq!(self.state.settlements.in_flight(), 0);
        assert!(
            self.state
                .ledger
                .list_reservations(self.user)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            self.state
                .ledger
                .balance(self.user)
                .await
                .unwrap()
                .as_micros(),
            BALANCE - amount
        );
        let balance: i64 = sqlx::query_scalar("SELECT balance_micro FROM users WHERE id=$1")
            .bind(self.user)
            .fetch_one(&self.state.pg)
            .await
            .unwrap();
        assert_eq!(balance, BALANCE - amount);
        let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE id=$1")
            .bind(self.key)
            .fetch_one(&self.state.pg)
            .await
            .unwrap();
        assert_eq!(used, amount);
        let actual: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM billing_records WHERE user_id=$1 AND log_type=2",
        )
        .bind(self.user)
        .fetch_one(&self.state.pg)
        .await
        .unwrap();
        assert_eq!(actual, records);
        let invalid_failures: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE user_id=$1 AND log_type=5 AND (status<>40 OR amount_micro<>0)")
            .bind(self.user).fetch_one(&self.state.pg).await.unwrap();
        assert_eq!(invalid_failures, 0);
    }
    async fn record(&self, response: &reqwest::Response) -> Value {
        self.state.settlements.wait_idle(WAIT).await;
        let id =
            Uuid::parse_str(response.headers()["x-okapi-request-id"].to_str().unwrap()).unwrap();
        sqlx::query_scalar(
            "SELECT to_jsonb(b) FROM billing_records b WHERE user_id=$1 AND request_id=$2",
        )
        .bind(self.user)
        .bind(id)
        .fetch_one(&self.state.pg)
        .await
        .unwrap()
    }
}
fn image_part() -> Part {
    Part::bytes(b"\x89PNG\r\n\x00\xff\x01".as_slice())
        .file_name("input.png")
        .mime_str("image/png")
        .unwrap()
}
fn launch(request: reqwest::RequestBuilder) -> tokio::task::JoinHandle<reqwest::Response> {
    tokio::spawn(async move { request.send().await.unwrap() })
}
async fn finish(
    task: tokio::task::JoinHandle<reqwest::Response>,
    status: u16,
) -> reqwest::Response {
    let response = timeout(WAIT, task).await.unwrap().unwrap();
    assert_eq!(response.status(), status);
    assert!(response.headers().contains_key("x-okapi-request-id"));
    response
}

#[tokio::test]
async fn json_rejects_invalid_counts_duplicate_fields_and_stream_types_before_reserve() {
    let env = setup().await;
    let mut cases: Vec<Value> = [
        json!(0),
        json!(11),
        json!(-1),
        json!(1.5),
        json!("2"),
        json!(4_294_967_296_u64),
    ]
    .into_iter()
    .map(|n| {
        let mut body = env.body(1);
        body["n"] = n;
        body
    })
    .collect();
    for field in ["model", "prompt"] {
        for value in [Value::Null, json!(" "), json!(false)] {
            let mut body = env.body(1);
            body[field] = value;
            cases.push(body);
        }
        let mut body = env.body(1);
        body.as_object_mut().unwrap().remove(field);
        cases.push(body);
    }
    for value in [json!("false"), json!(1)] {
        let mut body = env.body(1);
        body["stream"] = value;
        cases.push(body);
    }
    for body in cases {
        let response = env.request(false).json(&body).send().await.unwrap();
        assert_eq!(response.status(), 400, "{body}");
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "bad_request");
    }
    for extra in [
        "\"n\":2",
        "\"n\":null,\"n\":2",
        "\"model\":\"different\"",
        "\"prompt\":\"second\"",
    ] {
        let body = format!(
            "{{\"model\":\"{}\",\"prompt\":\"test\",\"n\":1,{extra}}}",
            env.model
        );
        assert_eq!(
            env.request(false)
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn json_default_cardinality_is_forwarded_and_preserves_provider_options() {
    let mut env = setup().await;
    let task=launch(env.request(false).json(&json!({"model":env.model,"prompt":"  original prompt  ","size":"1024x1024","quality":"high","background":"transparent"})));
    let peer = env.peer().await;
    assert_eq!(peer.path, "/v1/images/generations");
    assert_eq!(peer.headers["authorization"], "Bearer image-credential");
    assert_eq!(
        peer.body,
        json!({"model":"mapped-image","n":1,"prompt":"  original prompt  ","size":"1024x1024","quality":"high","background":"transparent"})
    );
    peer.images(1);
    let response = finish(task, 200).await;
    let record = env.record(&response).await;
    assert_eq!(record["pricing_snapshot"]["media_units"], 1);
    assert_eq!(record["upstream_request_id"], "image-contract-upstream");
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn json_edits_preserve_image_references_masks_and_options() {
    let mut env = setup().await;
    let mut body = env.body(2);
    body["images"] = json!([{"image_url":"https://image.example/input.png"},{"image_url":"data:image/png;base64,aGVsbG8="}]);
    body["mask"] = json!({"image_url":"https://image.example/mask.png"});
    body["input_fidelity"] = json!("high");
    let task = launch(env.request(true).json(&body));
    let peer = env.peer().await;
    assert_eq!(peer.path, "/v1/images/edits");
    body["model"] = json!("mapped-image");
    assert_eq!(peer.body, body);
    peer.images(2);
    let response = finish(task, 200).await;
    let record = env.record(&response).await;
    assert_eq!(record["pricing_snapshot"]["media_units"], 2);
    env.assert_money(2 * PRICE, 1).await;
}

#[tokio::test]
async fn json_edits_require_usable_references_and_reject_unowned_files() {
    let env = setup().await;
    for images in [
        Value::Null,
        json!([]),
        json!([{}]),
        json!([{"file_id":"file-other-tenant"}]),
        json!([{"image_url":"file:///tmp/input.png"}]),
        json!([{"image_url":""}]),
        json!([{"image_url":true}]),
    ] {
        let mut body = env.body(1);
        body["images"] = images;
        assert_eq!(
            env.request(true).json(&body).send().await.unwrap().status(),
            400
        );
    }
    let mut body = env.body(1);
    body["images"] = json!([{"image_url":"https://image.example/input.png"}]);
    body["mask"] = json!({"file_id":"file-other-mask"});
    assert_eq!(
        env.request(true).json(&body).send().await.unwrap().status(),
        400
    );
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn multipart_array_preserves_files_and_normalizes_the_billed_count() {
    let mut env = setup().await;
    let form = Form::new()
        .text("model", env.model.clone())
        .text("prompt", "edit both")
        .text("n", " 02 ")
        .text("stream", "false")
        .text("size", "1024x1024")
        .part("image[]", image_part())
        .part("image[]", image_part())
        .part("mask", image_part());
    let task = launch(env.request(true).multipart(form));
    let peer = env.peer().await;
    assert_eq!(peer.path, "/v1/images/edits");
    let field = |name: &str| peer.uploads.iter().find(|part| part.name == name).unwrap();
    assert_eq!(field("n").bytes.as_ref(), b"2");
    assert_eq!(field("model").bytes.as_ref(), b"mapped-image");
    assert_eq!(field("prompt").bytes.as_ref(), b"edit both");
    assert_eq!(field("size").bytes.as_ref(), b"1024x1024");
    let images: Vec<_> = peer
        .uploads
        .iter()
        .filter(|part| part.name == "image[]")
        .collect();
    assert_eq!(images.len(), 2);
    for part in images.into_iter().chain([field("mask")]) {
        assert_eq!(part.bytes.as_ref(), b"\x89PNG\r\n\x00\xff\x01");
        assert_eq!(part.filename.as_deref(), Some("input.png"));
        assert_eq!(part.mime.as_deref(), Some("image/png"));
    }
    peer.images(2);
    finish(task, 200).await;
    env.assert_money(2 * PRICE, 1).await;
}

#[tokio::test]
async fn multipart_default_count_and_partial_results_use_actual_image_count() {
    let mut env = setup().await;
    for n in [None, Some("3")] {
        let mut form = env.form();
        if let Some(n) = n {
            form = form.text("n", n);
        }
        let task = launch(env.request(true).multipart(form));
        let peer = env.peer().await;
        let counts: Vec<_> = peer
            .uploads
            .iter()
            .filter(|part| part.name == "n")
            .collect();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].bytes.as_ref(), n.unwrap_or("1").as_bytes());
        peer.images(1);
        let response = finish(task, 200).await;
        let record = env.record(&response).await;
        assert_eq!(record["amount_micro"], PRICE);
        assert_eq!(record["pricing_snapshot"]["media_units"], 1);
    }
    env.assert_money(2 * PRICE, 2).await;
}

#[tokio::test]
async fn multipart_rejects_bad_counts_duplicates_empty_files_and_streaming() {
    let env = setup().await;
    let mut forms = Vec::new();
    for value in ["0", "11", "-1", "1.5", "abc", "", "4294967296"] {
        forms.push(env.form().text("n", value));
    }
    for (field, value) in [
        ("n", "2"),
        ("model", "other"),
        ("prompt", "other"),
        ("stream", "invalid"),
    ] {
        let form = env.form().text("n", "1").text(field, value);
        forms.push(form);
    }
    forms.push(
        Form::new()
            .text("model", env.model.clone())
            .text("prompt", "test")
            .part("image", Part::bytes(Vec::new())),
    );
    forms.push(
        Form::new()
            .text("model", env.model.clone())
            .text("prompt", "test"),
    );
    forms.push(
        Form::new()
            .text("model", env.model.clone())
            .part("image", image_part()),
    );
    for form in forms {
        assert_eq!(
            env.request(true)
                .multipart(form)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn partial_json_result_refunds_unused_reservation() {
    let mut env = setup().await;
    let task = launch(env.request(false).json(&env.body(3)));
    let peer = env.peer().await;
    assert_eq!(peer.body["n"], 3);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        BALANCE - 3 * PRICE
    );
    peer.images(1);
    let response = finish(task, 200).await;
    let record = env.record(&response).await;
    for field in ["amount_micro", "original_amount_micro"] {
        assert_eq!(record[field], PRICE, "{record}");
    }
    assert_eq!(record["pricing_snapshot"]["media_units"], 1);
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn malformed_success_refunds_and_never_replays_generation() {
    let mut env = setup().await;
    for body in [
        "not json",
        "{}",
        "{\"data\":[]}",
        "{\"data\":[{}]}",
        "{\"data\":[{\"url\":\" \"}]}",
        "{\"data\":[{\"url\":\"https://image.example/1.png\"},{\"url\":\"https://image.example/2.png\"}]}",
    ] {
        for edit in [false, true] {
            let request = if edit {
                env.request(true).multipart(env.form())
            } else {
                env.request(false).json(&env.body(1))
            };
            let task = launch(request);
            env.peer().await.raw(200, body.into());
            let response = finish(task, 502).await;
            let error: Value = response.json().await.unwrap();
            assert_eq!(error["error"]["param"], "invalid_image_response");
        }
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 12);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn uncertain_upstream_failures_do_not_replay_and_explicit_rejections_can_failover() {
    let mut env = setup().await;
    // 5xx/超时虽不重放，仍登记 key 健康；阈值调高，这几次失败只计数不冷却，后段换渠道才有两个候选。
    sqlx::query("UPDATE channels SET settings=$2 WHERE id=ANY($1)")
        .bind(&env.channels)
        .bind(json!({"account_control":{"failure_threshold":20}}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    for status in [408, 500, 502, 503] {
        let task = launch(env.request(false).json(&env.body(1)));
        env.peer().await.raw(
            status,
            "{\"error\":{\"message\":\"private upstream text\"}}".into(),
        );
        let response = finish(task, 502).await;
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("private upstream text")
        );
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 4);
    let failures: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(failed_count),0)::bigint FROM channel_keys WHERE channel_id=ANY($1)",
    )
    .bind(&env.channels)
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    // 408 归请求级失败不动 key；500/502/503 三次按瞬态计数。
    assert_eq!(failures, 3, "uncertain 5xx still feed key health");
    env.assert_money(0, 0).await;
    let task = launch(env.request(true).multipart(env.form()));
    env.peer().await.raw(429, "{}".into());
    env.peer().await.images(1);
    let response = finish(task, 200).await;
    let record = env.record(&response).await;
    assert_eq!(record["failover_count"], 1);
    assert_eq!(env.hits.load(Ordering::SeqCst), 6);
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn in_flight_billing_keeps_original_pricing_epoch() {
    let mut env = setup().await;
    let epoch = env.state.pricebook.epoch();
    let task = launch(env.request(false).json(&env.body(2)));
    let peer = env.peer().await;
    // An empty next version ensures accidentally reloading price/epoch cannot go unnoticed.
    env.state.pricebook.replace(
        okapi_pricing::book::compile(okapi_pricing::PriceBookSource {
            epoch: epoch + 1,
            models: vec![],
            groups: vec![],
            overrides: vec![],
            rules: vec![],
        })
        .unwrap(),
    );
    peer.images(2);
    let response = finish(task, 200).await;
    let record = env.record(&response).await;
    assert_eq!(record["pricing_epoch"], epoch);
    env.assert_money(2 * PRICE, 1).await;
}

#[tokio::test]
async fn overflowing_batch_price_is_rejected_before_reserve_or_upstream() {
    let env = setup().await;
    env.state.pricebook.replace(
        okapi_pricing::book::compile(okapi_pricing::PriceBookSource {
            epoch: 1,
            models: vec![okapi_pricing::ModelEntry {
                model: env.model.as_str().into(),
                pricing: okapi_pricing::PricingMode::PerCall {
                    price: Money::from_micros(i64::MAX / 2),
                },
                tier_ratios: vec![],
            }],
            groups: vec![okapi_pricing::GroupEntry {
                group: "default".into(),
                ratio: okapi_pricing::RatioFp::ONE,
            }],
            overrides: vec![],
            rules: vec![],
        })
        .unwrap(),
    );
    for edit in [false, true] {
        let request = if edit {
            env.request(true).multipart(env.form().text("n", "3"))
        } else {
            env.request(false).json(&env.body(3))
        };
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 500);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "internal_error");
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn authentication_precedes_body_validation_and_oversized_bodies_use_json_413() {
    let env = setup().await;
    for route in ["generations", "edits"] {
        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/images/{route}", env.address))
            .header("content-type", "multipart/form-data")
            .body("invalid boundary")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        let error: Value = response.json().await.unwrap();
        assert!(error["error"]["code"].is_string());
    }
    for edit in [false, true] {
        let response = env
            .request(edit)
            .header("content-type", "application/json")
            .body(" ".repeat(33 * 1024 * 1024))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 413);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "bad_request");
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn redirects_cannot_repost_images_on_openai_or_azure() {
    let mut env = setup().await;
    for provider in ["openai", "azure"] {
        env.provider(provider).await;
        for status in [302, 307, 308] {
            for edit in [false, true] {
                let request = if edit {
                    env.request(true).multipart(env.form())
                } else {
                    env.request(false).json(&env.body(1))
                };
                let task = launch(request);
                let peer = env.peer().await;
                if provider == "azure" {
                    assert_eq!(peer.headers["api-key"], "image-credential");
                    assert!(peer.path.starts_with("/openai/deployments/mapped-image/"));
                }
                peer.reply
                    .send(
                        Response::builder()
                            .status(status)
                            .header("location", "/unexpected-image-replay")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .unwrap();
                finish(task, 502).await;
            }
        }
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 12);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn chunked_oversized_image_results_stop_without_charge_or_retry() {
    let mut env = setup().await;
    for provider in ["openai", "azure"] {
        env.provider(provider).await;
        for edit in [false, true] {
            let request = if edit {
                env.request(true).multipart(env.form())
            } else {
                env.request(false).json(&env.body(1))
            };
            let task = launch(request);
            let peer = env.peer().await;
            let chunk = Bytes::from(vec![b'x'; 1024 * 1024]);
            let chunks =
                futures::stream::iter((0..65).map(move |_| Ok::<_, std::io::Error>(chunk.clone())));
            peer.reply
                .send(
                    Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from_stream(chunks))
                        .unwrap(),
                )
                .unwrap();
            finish(task, 502).await;
        }
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 4);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn base64_result_is_preserved_and_billed_once() {
    let mut env = setup().await;
    let task = launch(env.request(false).json(&env.body(1)));
    let output = json!({"data":[{"b64_json":"aGVsbG8=","revised_prompt":"revised"}],"usage":{"total_tokens":20}});
    env.peer().await.raw(200, output.to_string());
    let response = finish(task, 200).await;
    assert_eq!(
        env.record(&response).await["pricing_snapshot"]["image_usage_reported"],
        false
    );
    let returned: Value = response.json().await.unwrap();
    assert_eq!(returned, output);
    env.assert_money(PRICE, 1).await;
}
