//! A price publication during upstream IO must not relabel an earlier quote.
//! Real HTTP, isolated PG/Redis/CH; fixed integer money and unit oracles.

use axum::extract::State;
use axum::response::IntoResponse as _;
use futures::FutureExt as _;
use okapi::gateway;
use okapi_domain::{GroupCode, ModelCode, Money};
use okapi_pricing::{GroupEntry, ModelEntry, PriceBookSource, PricingMode, RatioFp};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row as _};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

const INITIAL_BALANCE: i64 = 10_000_000;
const OLD_EPOCH: i64 = 41;
const NEW_EPOCH: i64 = 42;

#[derive(Clone, Copy, Debug)]
enum Case {
    SpeechRatio,
    SpeechPerCall,
    Transcription,
    Translation,
    Video,
    Pass,
    PassFailure,
}

impl Case {
    const fn path(self) -> &'static str {
        match self {
            Self::SpeechRatio | Self::SpeechPerCall => "/v1/audio/speech",
            Self::Transcription => "/v1/audio/transcriptions",
            Self::Translation => "/v1/audio/translations",
            Self::Video => "/v1/videos",
            Self::Pass | Self::PassFailure => "/ok/tool",
        }
    }

    const fn price(self) -> i64 {
        match self {
            Self::SpeechRatio => 22,
            Self::SpeechPerCall => 7000,
            Self::Transcription | Self::Translation => 6000,
            Self::Video => 40_000,
            Self::Pass => 5000,
            Self::PassFailure => 0,
        }
    }

    const fn is_pass(self) -> bool {
        matches!(self, Self::Pass | Self::PassFailure)
    }

    const fn is_speech(self) -> bool {
        matches!(self, Self::SpeechRatio | Self::SpeechPerCall)
    }

    fn pricing(self, factor: i64) -> PricingMode {
        if matches!(self, Self::SpeechRatio) {
            return PricingMode::Ratio {
                model_ratio: RatioFp::from_scaled(factor.checked_mul(1_000_000).unwrap()).unwrap(),
                completion_ratio: RatioFp::ONE,
                cache_ratio: RatioFp::ONE,
                cache_write_ratio: RatioFp::ONE,
                audio_ratio: RatioFp::ONE,
                audio_completion_ratio: RatioFp::ONE,
                image_ratio: RatioFp::ONE,
                modality_ratios: okapi_pricing::ModalityRatios::default(),
            };
        }
        let base = match self {
            Self::Video => 10_000,
            Self::PassFailure => 5000,
            _ => self.price(),
        };
        PricingMode::PerCall {
            price: Money::from_micros(base.checked_mul(factor).unwrap()),
        }
    }
}

#[derive(Clone)]
struct Gate {
    entered: mpsc::UnboundedSender<()>,
    release: Arc<Notify>,
    case: Case,
}

async fn upstream(State(gate): State<Gate>) -> axum::response::Response {
    gate.entered.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), gate.release.notified())
        .await
        .expect("test must explicitly release the upstream");
    match gate.case {
        Case::SpeechRatio | Case::SpeechPerCall => (
            [(axum::http::header::CONTENT_TYPE, "audio/mpeg")],
            vec![0xFFu8, 0xFB, 0x90, 0x00],
        )
            .into_response(),
        Case::Transcription | Case::Translation => {
            axum::Json(json!({"text":"hello","duration":3})).into_response()
        }
        Case::Video => axum::Json(
            json!({"id":format!("video_epoch_{}",Uuid::new_v4().simple()),"status":"queued"}),
        )
        .into_response(),
        Case::Pass => axum::Json(json!({"ok":true})).into_response(),
        Case::PassFailure => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":"mock_failure"})),
        )
            .into_response(),
    }
}

async fn spawn(router: axum::Router) -> (SocketAddr, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (addr, task)
}

struct Env {
    pg: PgPool,
    state: gateway::state::AppState,
    addr: SocketAddr,
    token: String,
    model: String,
    user: i64,
    channel: i64,
    case: Case,
    entered: mpsc::UnboundedReceiver<()>,
    release: Arc<Notify>,
    servers: Vec<JoinHandle<()>>,
}

impl Drop for Env {
    fn drop(&mut self) {
        for server in &self.servers {
            server.abort();
        }
    }
}

fn book(case: Case, model: &str, epoch: i64, factor: i64) -> okapi_pricing::PriceBook {
    okapi_pricing::book::compile(PriceBookSource {
        epoch,
        models: vec![ModelEntry {
            model: ModelCode::from(model),
            pricing: case.pricing(factor),
            tier_ratios: vec![],
        }],
        groups: vec![GroupEntry {
            group: GroupCode::from("default"),
            ratio: RatioFp::ONE,
        }],
        overrides: vec![],
        rules: vec![],
    })
    .unwrap()
}

async fn setup(case: Case) -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("isolated PG required");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("isolated Redis required");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("epoch-{suffix}");
    let user = okapi_store::provision::create_user(&pg, &format!("epoch-{suffix}"))
        .await
        .unwrap();
    if matches!(case, Case::PassFailure) {
        // Nonzero attempted discount must also disappear from failed consumption.
        sqlx::query("UPDATE users SET price_multiplier=0.5 WHERE id=$1")
            .bind(user)
            .execute(&pg)
            .await
            .unwrap();
    }
    let token = format!("sk-epoch-{suffix}");
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    okapi_store::provision::create_api_key(&pg, user, &hash, "epoch")
        .await
        .unwrap();
    if matches!(case, Case::SpeechRatio) {
        okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
            .await
            .unwrap();
    } else {
        okapi_store::admin::upsert_model_per_call(&pg, &model, 5000)
            .await
            .unwrap();
    }
    let (tx, entered) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let gate = Gate {
        entered: tx,
        release: release.clone(),
        case,
    };
    let router = axum::Router::new()
        .route(case.path(), axum::routing::post(upstream))
        .with_state(gate);
    let (upstream_addr, upstream_task) = spawn(router).await;
    let channel = create_channel(&pg, &suffix, &model, case, upstream_addr).await;
    let state = gateway::build_state(&database_url, &redis_url, "epoch-test", None, None)
        .await
        .unwrap();
    state.pricebook.replace(book(case, &model, OLD_EPOCH, 1));
    let credit = okapi_ledger::operations::credit(
        &pg,
        &state.ledger,
        user,
        Money::from_micros(INITIAL_BALANCE),
        "recharge",
        "system:epoch-test",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        credit.balance_after,
        Some(Money::from_micros(INITIAL_BALANCE))
    );
    let (addr, gateway_task) = spawn(gateway::router(state.clone())).await;
    Env {
        pg,
        state,
        addr,
        token,
        model,
        user,
        channel,
        case,
        entered,
        release,
        servers: vec![upstream_task, gateway_task],
    }
}

async fn create_channel(
    pg: &PgPool,
    suffix: &str,
    model: &str,
    case: Case,
    addr: SocketAddr,
) -> i64 {
    let provider = if case.is_pass() {
        "custom_pass"
    } else {
        "openai"
    };
    let base = if case.is_pass() {
        format!("http://{addr}")
    } else {
        format!("http://{addr}/v1")
    };
    let models = if case.is_pass() { vec![] } else { vec![model] };
    let (channel, _) = okapi_store::provision::create_channel(
        pg,
        &format!("epoch-{suffix}"),
        provider,
        &base,
        "mock-credential",
        &models,
        false,
        None,
    )
    .await
    .unwrap();
    if case.is_pass() {
        sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1")
            .bind(channel)
            .bind(json!({"allowed_paths":["/ok"],"billing_model":model,
                "auth_header":"x-api-key","auth_scheme":""}))
            .execute(pg)
            .await
            .unwrap();
    }
    channel
}

fn request(env: &Env) -> reqwest::RequestBuilder {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let path = if env.case.is_pass() {
        format!("/pass/{}{}", env.channel, env.case.path())
    } else {
        env.case.path().to_owned()
    };
    let request = client
        .post(format!("http://{}{path}", env.addr))
        .bearer_auth(&env.token);
    match env.case {
        Case::SpeechRatio | Case::SpeechPerCall => request.json(&json!({
            "model":env.model,"input":"hello world","voice":"alloy"
        })),
        Case::Transcription | Case::Translation => request.multipart(
            reqwest::multipart::Form::new()
                .text("model", env.model.clone())
                .part(
                    "file",
                    reqwest::multipart::Part::bytes(vec![0; 16])
                        .file_name("clip.wav")
                        .mime_str("audio/wav")
                        .unwrap(),
                ),
        ),
        Case::Video => request.json(&json!({"model":env.model,"prompt":"test","seconds":"4"})),
        Case::Pass | Case::PassFailure => request.json(&json!({"input":"test"})),
    }
}

async fn respond(env: &Env, response: reqwest::Response) -> Uuid {
    let expected = if matches!(env.case, Case::PassFailure) {
        400
    } else {
        200
    };
    let status = response.status().as_u16();
    let id = response
        .headers()
        .get("x-okapi-request-id")
        .map(|header| header.to_str().unwrap().parse::<Uuid>().unwrap());
    let body = response.bytes().await.unwrap();
    assert_eq!(
        status,
        expected,
        "unexpected HTTP body: {}",
        String::from_utf8_lossy(&body)
    );
    if env.case.is_speech() {
        assert_eq!(body.as_ref(), &[0xFF, 0xFB, 0x90, 0x00]);
    }
    if matches!(env.case, Case::PassFailure) {
        // Existing transparent upstream errors have no request-ID header.
        sqlx::query_scalar("SELECT request_id FROM billing_records WHERE user_id=$1 AND model_name=$2 ORDER BY id DESC LIMIT 1")
            .bind(env.user).bind(&env.model).fetch_one(&env.pg).await.unwrap()
    } else {
        id.expect("successful gateway response must identify its bill")
    }
}

fn number(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn compare(errors: &mut Vec<String>, label: &str, actual: Option<i64>, expected: Option<i64>) {
    if actual != expected {
        errors.push(format!("{label}: actual={actual:?}, expected={expected:?}"));
    }
}

async fn check_bill(
    env: &Env,
    ch: &okapi_store::ChClient,
    id: Uuid,
    epoch: i64,
    factor: i64,
    errors: &mut Vec<String>,
) {
    let amount = env.case.price().checked_mul(factor).unwrap();
    let row = sqlx::query("SELECT pricing_epoch,pricing_snapshot,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,prompt_tokens,completion_tokens,status FROM billing_records WHERE request_id=$1")
        .bind(id).fetch_one(&env.pg).await.unwrap();
    compare(errors, "PG epoch", row.get("pricing_epoch"), Some(epoch));
    let snapshot: Value = row.get("pricing_snapshot");
    compare(
        errors,
        "PG snapshot epoch",
        number(&snapshot["epoch"]),
        Some(epoch),
    );
    for (field, expected) in [
        ("amount_micro", amount),
        ("original_amount_micro", amount),
        ("discount_micro", 0),
    ] {
        compare(
            errors,
            &format!("PG {field}"),
            Some(row.get::<i64, _>(field)),
            Some(expected),
        );
    }
    let cost = (!env.case.is_pass()).then_some(amount);
    compare(
        errors,
        "PG upstream cost",
        row.get("upstream_cost_micro"),
        cost,
    );
    assert_eq!(row.get::<i32, _>("prompt_tokens"), 0);
    assert_eq!(row.get::<i32, _>("completion_tokens"), 0);
    assert_eq!(
        row.get::<i16, _>("status"),
        if matches!(env.case, Case::PassFailure) {
            40
        } else {
            20
        }
    );
    if env.case.is_speech() {
        assert_eq!(snapshot["input_unit"], "characters");
        assert_eq!(snapshot["input_characters"], 11);
    }
    if matches!(env.case, Case::Video) {
        assert_eq!(snapshot["media_units"], 4);
    }
    if matches!(env.case, Case::Transcription | Case::Translation) {
        assert_eq!(snapshot["media_units"], 3);
    }
    let mut payload: Value =
        sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1")
            .bind(id.to_string())
            .fetch_one(&env.pg)
            .await
            .unwrap();
    compare(
        errors,
        "outbox epoch",
        number(&payload["pricing_epoch"]),
        Some(epoch),
    );
    let ratio: Value = serde_json::from_str(payload["ratio_snapshot"].as_str().unwrap()).unwrap();
    assert_eq!(ratio, snapshot, "outbox must preserve the exact PG quote");
    for (field, expected) in [
        ("amount_micro", amount),
        ("original_amount_micro", amount),
        ("discount_micro", 0),
        ("upstream_cost_micro", cost.unwrap_or(0)),
    ] {
        compare(
            errors,
            &format!("outbox {field}"),
            number(&payload[field]),
            Some(expected),
        );
    }
    check_ch(env, ch, id, epoch, amount, &mut payload, errors).await;
}

async fn check_ch(
    env: &Env,
    ch: &okapi_store::ChClient,
    id: Uuid,
    epoch: i64,
    amount: i64,
    payload: &mut Value,
    errors: &mut Vec<String>,
) {
    payload["ts"] = json!(
        chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string()
    );
    let row = okapi::worker::chsink::js_payload_to_ch_row(payload);
    for _ in 0..2 {
        ch.insert_json_each_row(
            "request_log_raw",
            std::slice::from_ref(&row),
            &id.to_string(),
        )
        .await
        .unwrap();
    }
    let rows = ch.query_with_params("SELECT pricing_epoch,ratio_snapshot,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,prompt_tokens,completion_tokens,input_unit,input_characters FROM request_log_raw WHERE request_id={id:String}", &[("id",&id.to_string())]).await.unwrap();
    assert_eq!(rows.len(), 1, "outbox replay must not duplicate statistics");
    compare(
        errors,
        "CH epoch",
        number(&rows[0]["pricing_epoch"]),
        Some(epoch),
    );
    let snapshot: Value =
        serde_json::from_str(rows[0]["ratio_snapshot"].as_str().unwrap()).unwrap();
    compare(
        errors,
        "CH snapshot epoch",
        number(&snapshot["epoch"]),
        Some(epoch),
    );
    for (field, expected) in [
        ("amount_micro", amount),
        ("original_amount_micro", amount),
        ("discount_micro", 0),
        (
            "upstream_cost_micro",
            if env.case.is_pass() { 0 } else { amount },
        ),
    ] {
        compare(
            errors,
            &format!("CH {field}"),
            number(&rows[0][field]),
            Some(expected),
        );
    }
    assert_eq!(rows[0]["prompt_tokens"], 0);
    assert_eq!(rows[0]["completion_tokens"], 0);
    if env.case.is_speech() {
        assert_eq!(rows[0]["input_unit"], "characters");
        assert_eq!(rows[0]["input_characters"], 11);
    }
}

async fn exercise(env: &mut Env, ch: &okapi_store::ChClient) {
    let first = request(env);
    let pending = tokio::spawn(async move { first.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), env.entered.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(env.state.pricebook.epoch(), OLD_EPOCH);
    assert!(
        env.state
            .pricebook
            .swap_if_newer(book(env.case, &env.model, NEW_EPOCH, 10))
    );
    assert_eq!(env.state.pricebook.epoch(), NEW_EPOCH);
    env.release.notify_one();
    let first_id = respond(env, pending.await.unwrap()).await;
    // The following request must independently quote the newly published book.
    env.release.notify_one();
    let second_id = respond(env, request(env).send().await.unwrap()).await;
    let mut errors = vec![];
    check_bill(env, ch, first_id, OLD_EPOCH, 1, &mut errors).await;
    check_bill(env, ch, second_id, NEW_EPOCH, 10, &mut errors).await;
    let expected = INITIAL_BALANCE
        .checked_sub(env.case.price().checked_mul(11).unwrap())
        .unwrap();
    compare(
        &mut errors,
        "Redis wallet",
        Some(
            env.state
                .ledger
                .balance(env.user)
                .await
                .unwrap()
                .as_micros(),
        ),
        Some(expected),
    );
    let pg_balance: i64 = sqlx::query_scalar("SELECT balance_micro FROM users WHERE id=$1")
        .bind(env.user)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    compare(&mut errors, "PG wallet", Some(pg_balance), Some(expected));
    check_totals(env, ch, &mut errors).await;
    assert!(errors.is_empty(), "{:?}:\n{}", env.case, errors.join("\n"));
}

async fn check_totals(env: &Env, ch: &okapi_store::ChClient, errors: &mut Vec<String>) {
    let total = env.case.price().checked_mul(11).unwrap();
    let events = sqlx::query("SELECT count(*) AS n,coalesce(sum(delta_micro),0)::bigint AS delta FROM billing_events WHERE user_id=$1 AND request_id IS NOT NULL AND event_type IN ('commit','refund')")
        .bind(env.user).fetch_one(&env.pg).await.unwrap();
    assert_eq!(events.get::<i64, _>("n"), 2);
    compare(
        errors,
        "PG events delta",
        Some(events.get("delta")),
        Some(total.saturating_neg()),
    );
    let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE user_id=$1")
        .bind(env.user)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    compare(errors, "API key consumption", Some(used), Some(total));
    let rows = ch.query_with_params("SELECT countMerge(requests) AS requests,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount,sumMerge(original) AS original,sumMerge(discount) AS discount,sumMerge(upstream_cost) AS cost,sumMerge(errors) AS errors FROM mv_user_day WHERE user_id={user:Int64}", &[("user",&env.user.to_string())]).await.unwrap();
    assert_eq!(rows.len(), 1);
    for (field, expected) in [
        ("requests", 2),
        ("tokens", 0),
        ("amount", total),
        ("original", total),
        ("discount", 0),
        ("cost", if env.case.is_pass() { 0 } else { total }),
        (
            "errors",
            if matches!(env.case, Case::PassFailure) {
                2
            } else {
                0
            },
        ),
    ] {
        compare(
            errors,
            &format!("CH user totals {field}"),
            number(&rows[0][field]),
            Some(expected),
        );
    }
}

async fn verify(case: Case) {
    let mut env = setup(case).await;
    let database = format!("okapi_price_epoch_{}", Uuid::new_v4().simple());
    let url = std::env::var("OKAPI_CLICKHOUSE_URL").expect("isolated CH required");
    let ch = okapi_store::ChClient::new(&url, &database).unwrap();
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(exercise(&mut env, &ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn speech_ratio_preserves_quote_epoch_across_publication() {
    verify(Case::SpeechRatio).await;
}
#[tokio::test]
async fn speech_per_call_preserves_quote_epoch_across_publication() {
    verify(Case::SpeechPerCall).await;
}
#[tokio::test]
async fn transcription_preserves_quote_epoch_across_publication() {
    verify(Case::Transcription).await;
}
#[tokio::test]
async fn translation_preserves_quote_epoch_across_publication() {
    verify(Case::Translation).await;
}
#[tokio::test]
async fn video_preserves_quote_epoch_and_seconds_across_publication() {
    verify(Case::Video).await;
}
#[tokio::test]
async fn custom_pass_preserves_quote_epoch_across_publication() {
    verify(Case::Pass).await;
}
#[tokio::test]
async fn failed_custom_pass_refunds_and_records_zero_consumption() {
    verify(Case::PassFailure).await;
}
