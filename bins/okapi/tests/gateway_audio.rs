//! M4 audio 验收：speech 输入字符计费 + 二进制回传；
//! transcriptions multipart 重组转发 + per_call 计费 + duration 入快照 +
//! 非 per_call 模型拒绝。依赖 .env（scripts/dev-deps.sh up）。

use axum::extract::{Multipart, State};
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row as _};
use std::net::SocketAddr;
use uuid::Uuid;

#[path = "support/published_pricing.rs"]
mod published_pricing;

async fn mock_speech(
    State(expected): State<String>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    if req["input"] == "upstream-unavailable" {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    assert_eq!(req["input"], expected, "JSON 原样透传");
    if expected == "test-upstream-failure" {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    }
    (
        [(axum::http::header::CONTENT_TYPE, "audio/mpeg")],
        vec![0xFFu8, 0xFB, 0x90, 0x00], // 假 mp3 头
    )
        .into_response()
}

async fn mock_transcribe(mut multipart: Multipart) -> axum::response::Response {
    let mut saw_model = String::new();
    let mut file_len = 0usize;
    let mut filename = String::new();
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            filename = field.file_name().unwrap_or_default().to_owned();
            file_len = field.bytes().await.unwrap().len();
        } else if name == "model" {
            saw_model = String::from_utf8(field.bytes().await.unwrap().to_vec()).unwrap();
        } else {
            let _ = field.bytes().await;
        }
    }
    assert!(
        saw_model.starts_with("stt-"),
        "model part 应为上游名：{saw_model}"
    );
    assert_eq!(filename, "clip.wav", "文件名必须保留");
    assert_eq!(file_len, 16, "文件字节必须完整");
    axum::Json(json!({"text": "hello", "duration": 3.4})).into_response()
}

async fn spawn_mock(expected_input: &str) -> SocketAddr {
    let router = axum::Router::new()
        .route("/v1/audio/speech", post(mock_speech))
        .route("/v1/audio/transcriptions", post(mock_transcribe))
        .route("/v1/audio/translations", post(mock_transcribe))
        .with_state(expected_input.to_owned());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    state: gateway::state::AppState,
    tts_model: String,
    stt_model: String,
    channel_key: i64,
}

async fn setup() -> TestEnv {
    setup_input("hello world").await
}

async fn setup_input(expected_input: &str) -> TestEnv {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let tts_model = format!("tts-{}", &suffix[..10]);
    let stt_model = format!("stt-{}", &suffix[..10]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("au-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-au-{suffix}");
    let hash = { hex::encode(Sha256::digest(token.as_bytes())) };
    okapi_store::provision::create_api_key(&pg, user_id, &hash, "sk-okapi-au")
        .await
        .unwrap();
    // TTS：ratio 模式（字符单独计价）；STT：per_call $0.006
    okapi_store::provision::create_model_ratio(&pg, &tts_model, "1", "1", "1")
        .await
        .unwrap();
    okapi_store::admin::upsert_model_per_call(&pg, &stt_model, 6000)
        .await
        .unwrap();

    let mock = spawn_mock(expected_input).await;
    let (_, channel_key) = okapi_store::provision::create_channel(
        &pg,
        &format!("au-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[tts_model.as_str(), stt_model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    published_pricing::publish(&pg, user_id).await;
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_000_000))
        .await
        .unwrap();
    let app = gateway::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        gateway: addr,
        token,
        user_id,
        state,
        tts_model,
        stt_model,
        channel_key,
    }
}

async fn wait_record(pg: &PgPool, user_id: i64, model: &str) -> (i64, Option<Value>) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT amount_micro, pricing_snapshot FROM billing_records
               WHERE user_id = $1 AND model_name = $2 AND log_type = 2"#,
            user_id,
            model
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.amount_micro, r.pricing_snapshot);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

/// speech：11 字符 × ratio1 × $2/1M = 22 micro；二进制原样回传。
#[tokio::test]
async fn speech_bills_by_characters() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/speech", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.tts_model, "input": "hello world", "voice": "alloy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("audio/mpeg")
    );
    let audio = resp.bytes().await.unwrap();
    assert_eq!(audio.as_ref(), &[0xFFu8, 0xFB, 0x90, 0x00], "二进制原样");

    let (amount, _) = wait_record(&env.pg, env.user_id, &env.tts_model).await;
    assert_eq!(amount, 22, "11 chars × 1 × $2/1M");
    assert_speech_units(&env, 11).await;
}

async fn assert_speech_units(env: &TestEnv, characters: i64) {
    let row = sqlx::query("SELECT prompt_tokens, completion_tokens, usage_details, pricing_snapshot FROM billing_records WHERE user_id=$1 AND model_name=$2 AND log_type=2")
        .bind(env.user_id).bind(&env.tts_model).fetch_one(&env.pg).await.unwrap();
    assert_eq!(
        row.get::<i32, _>("prompt_tokens"),
        0,
        "characters must not enter Token totals"
    );
    assert_eq!(row.get::<i32, _>("completion_tokens"), 0);
    let units: Value = row.get("usage_details");
    assert_eq!(units["input_unit"], "characters");
    assert_eq!(units["input_characters"], characters);
    let snapshot: Value = row.get("pricing_snapshot");
    assert_eq!(snapshot["input_unit"], "characters");
    assert_eq!(snapshot["input_characters"], characters);
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=(SELECT request_id::text FROM billing_records WHERE user_id=$1 AND model_name=$2 AND log_type=2)")
        .bind(env.user_id).bind(&env.tts_model).fetch_one(&env.pg).await.unwrap();
    assert_eq!(payload["input_unit"], "characters");
    assert_eq!(payload["input_characters"], characters);
    assert_eq!(payload["prompt_tokens"], 0);
    assert_eq!(payload["completion_tokens"], 0);
}

#[tokio::test]
async fn speech_empty_input_preserves_an_explicit_zero_character_quantity() {
    let env = setup_input("").await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/speech", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.tts_model,"input":"","voice":"alloy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let (amount, _) = wait_record(&env.pg, env.user_id, &env.tts_model).await;
    assert_eq!(amount, 0);
    assert_speech_units(&env, 0).await;
}

#[tokio::test]
async fn speech_units_and_four_amounts_reach_clickhouse_and_both_console_logs() {
    use futures::FutureExt as _;
    let env = setup().await;
    let url = std::env::var("OKAPI_CLICKHOUSE_URL").expect("isolated ClickHouse required");
    let database = format!("okapi_speech_units_{}", Uuid::new_v4().simple());
    let ch = okapi_store::ChClient::new(&url, &database).unwrap();
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_console_pipeline(&env, &ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_console_pipeline(env: &TestEnv, ch: &okapi_store::ChClient) {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/speech", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.tts_model,"input":"hello world","voice":"alloy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    wait_record(&env.pg, env.user_id, &env.tts_model).await;
    assert_speech_units(env, 11).await;
    let row=sqlx::query("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,request_id FROM billing_records WHERE user_id=$1 AND model_name=$2 AND log_type=2")
        .bind(env.user_id).bind(&env.tts_model).fetch_one(&env.pg).await.unwrap();
    assert_eq!(row.get::<i64, _>("amount_micro"), 22);
    assert_eq!(row.get::<i64, _>("original_amount_micro"), 22);
    assert_eq!(row.get::<i64, _>("discount_micro"), 0);
    assert_eq!(row.get::<Option<i64>, _>("upstream_cost_micro"), Some(22));
    let request_id = row.get::<Uuid, _>("request_id");
    let mut payload: Value =
        sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1")
            .bind(request_id.to_string())
            .fetch_one(&env.pg)
            .await
            .unwrap();
    payload["ts"] = json!(
        chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string()
    );
    assert_eq!(payload["amount_micro"], 22);
    assert_eq!(payload["original_amount_micro"], 22);
    assert_eq!(payload["discount_micro"], 0);
    assert_eq!(payload["upstream_cost_micro"], 22);
    let value = okapi::worker::chsink::js_payload_to_ch_row(&payload);
    for _ in 0..2 {
        ch.insert_json_each_row(
            "request_log_raw",
            std::slice::from_ref(&value),
            &request_id.to_string(),
        )
        .await
        .unwrap();
    }
    let rows=ch.query_with_params("SELECT prompt_tokens,completion_tokens,input_unit,input_characters,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro FROM request_log_raw WHERE request_id={id:String}",&[("id",&request_id.to_string())]).await.unwrap();
    assert_eq!(rows.len(), 1, "replay must not duplicate requests or units");
    assert_eq!(rows[0]["prompt_tokens"], 0);
    assert_eq!(rows[0]["completion_tokens"], 0);
    assert_eq!(rows[0]["input_unit"], "characters");
    assert_eq!(rows[0]["input_characters"], 11);
    for field in [
        "amount_micro",
        "original_amount_micro",
        "upstream_cost_micro",
    ] {
        assert_eq!(
            rows[0][field]
                .as_str()
                .map(|s| s.parse::<i64>().unwrap())
                .or_else(|| rows[0][field].as_i64()),
            Some(22)
        );
    }
    assert_eq!(
        rows[0]["discount_micro"]
            .as_str()
            .map(|s| s.parse::<i64>().unwrap())
            .or_else(|| rows[0]["discount_micro"].as_i64()),
        Some(0)
    );
    let (addr, admin) = spawn_console(env, ch).await;
    check_console_exports(env, addr, &admin).await;
}

async fn spawn_console(env: &TestEnv, ch: &okapi_store::ChClient) -> (SocketAddr, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&env.pg, &format!("tts-admin-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(admin_id)
        .execute(&env.pg)
        .await
        .unwrap();
    let token = format!("sk-tts-admin-{suffix}");
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    okapi_store::provision::create_api_key(&env.pg, admin_id, &hash, "tts-admin")
        .await
        .unwrap();
    let mut state = env.state.clone();
    state.ch = Some(ch.clone());
    let app = okapi::console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, token)
}

async fn console_get(addr: SocketAddr, token: &str, path: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{path}: {body}");
    body
}

async fn check_console_exports(env: &TestEnv, addr: SocketAddr, admin: &str) {
    for (token, path) in [
        (env.token.as_str(), "/api/me/logs?scope=user".to_owned()),
        (admin, format!("/admin/logs?user_id={}", env.user_id)),
    ] {
        let body = console_get(addr, token, &path).await;
        let usage = &body["data"][0]["usage"];
        assert_eq!(usage["prompt_tokens"], 0);
        assert_eq!(usage["completion_tokens"], 0);
        assert_eq!(usage["input_unit"], "characters");
        assert_eq!(usage["input_characters"], 11);
    }
    let portal = console_get(addr, &env.token, "/api/me/logs/stat?scope=user").await;
    assert_eq!(portal["prompt_tokens"], 0);
    assert_eq!(portal["input_units"]["characters"], 11);
    assert_eq!(portal["amount_micro"], 22);
    let logs = console_get(
        addr,
        admin,
        &format!("/admin/logs/stat?user_id={}", env.user_id),
    )
    .await;
    assert_eq!(logs["tokens"], 0);
    assert_eq!(logs["input_units"]["characters"], 11);
    assert_eq!(logs["amount_micro"], 22);
    let trend = console_get(
        addr,
        admin,
        &format!("/admin/stats/trend?days=2&user_id={}", env.user_id),
    )
    .await;
    assert_eq!(trend["total"]["tokens"], 0);
    assert_eq!(trend["total"]["input_units"]["characters"], 11);
    assert_eq!(trend["total"]["amount_micro"], 22);
}

#[tokio::test]
async fn speech_characters_do_not_consume_token_tpm_or_monthly_tokens() {
    let env = setup().await;
    enable_volume_rule(&env);
    sqlx::query("UPDATE api_keys SET tpm_limit=1 WHERE user_id=$1")
        .bind(env.user_id)
        .execute(&env.pg)
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/speech", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.tts_model,"input":"hello world","voice":"alloy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "characters must not occupy the one-Token TPM limit"
    );
    let (amount, _) = wait_record(&env.pg, env.user_id, &env.tts_model).await;
    assert_eq!(amount, 22);
    assert_speech_units(&env, 11).await;
    env.state
        .settlements
        .wait_idle(std::time::Duration::from_secs(5))
        .await;
    assert_eq!(env.state.settlements.in_flight(), 0);
    // The same counters feed volume discounts and Token limits.
    assert_eq!(env.state.sched.monthly_tokens_get(env.user_id).await, 0);
}

fn enable_volume_rule(env: &TestEnv) {
    use okapi_domain::{GroupCode, ModelCode};
    use okapi_pricing::{
        GroupEntry, ModelEntry, PriceBookSource, PricingMode, PricingRule, RatioFp, RuleKind,
        RuleScope, Stacking, book,
    };
    let mode = PricingMode::Ratio {
        model_ratio: RatioFp::ONE,
        completion_ratio: RatioFp::ONE,
        cache_ratio: RatioFp::ONE,
        cache_write_ratio: RatioFp::ONE,
        audio_ratio: RatioFp::ONE,
        audio_completion_ratio: RatioFp::ONE,
        image_ratio: RatioFp::ONE,
        modality_ratios: okapi_pricing::ModalityRatios::default(),
    };
    let book = book::compile(PriceBookSource {
        epoch: env.state.pricebook.epoch(),
        models: vec![ModelEntry {
            model: ModelCode::from(env.tts_model.as_str()),
            pricing: mode,
            tier_ratios: vec![],
        }],
        groups: vec![GroupEntry {
            group: GroupCode::from("default"),
            ratio: RatioFp::ONE,
        }],
        overrides: vec![],
        rules: vec![PricingRule {
            code: "test-characters-volume".into(),
            kind: RuleKind::Volume {
                min_monthly_tokens: 100,
                min_monthly_spend_micro: 0,
            },
            multiplier: "0.5".parse().unwrap(),
            scope: RuleScope::default(),
            priority: 0,
            stacking: Stacking::Stackable,
            valid_from: None,
            valid_to: None,
        }],
    })
    .unwrap();
    assert!(book.has_volume_rules());
    env.state.pricebook.replace(book);
}

#[tokio::test]
async fn speech_unicode_uses_codepoints_not_utf8_bytes_as_the_existing_tariff() {
    let input = "你好🦀e\u{301}";
    assert_eq!(input.chars().count(), 5);
    let env = setup_input(input).await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/speech", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.tts_model,"input":input,"voice":"alloy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        &[0xFF, 0xFB, 0x90, 0x00]
    );
    let (amount, _) = wait_record(&env.pg, env.user_id, &env.tts_model).await;
    assert_eq!(
        amount, 10,
        "five codepoints, rather than bytes or grapheme clusters"
    );
    assert_speech_units(&env, 5).await;
}

/// transcriptions：multipart 重组（文件名/字节/上游模型名）+ per_call 计费 +
/// duration 秒入快照；ratio 模型拒绝。
#[tokio::test]
async fn transcriptions_per_call_with_multipart() {
    let env = setup().await;
    let file_part = reqwest::multipart::Part::bytes(vec![7u8; 16])
        .file_name("clip.wav")
        .mime_str("audio/wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("model", env.stt_model.clone())
        .text("response_format", "verbose_json")
        .part("file", file_part);
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/transcriptions", env.gateway))
        .bearer_auth(&env.token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["text"], "hello");

    let (amount, snapshot) = wait_record(&env.pg, env.user_id, &env.stt_model).await;
    assert_eq!(amount, 6000, "per_call $0.006");
    let snapshot = snapshot.expect("必须携带快照");
    assert_eq!(snapshot["media_units"], 4, "duration 3.4s 向上取整入快照");
    let details:Value=sqlx::query_scalar("SELECT usage_details FROM billing_records WHERE user_id=$1 AND model_name=$2 AND log_type=2")
        .bind(env.user_id).bind(&env.stt_model).fetch_one(&env.pg).await.unwrap();
    assert_eq!(
        details["input_unit"], "",
        "duration-priced audio has no observed Token unit"
    );
    assert!(details["input_characters"].is_null());

    // ratio 模型走 transcriptions：400 拒绝（时长无法本地解码）
    let form = reqwest::multipart::Form::new()
        .text("model", env.tts_model.clone())
        .part(
            "file",
            reqwest::multipart::Part::bytes(vec![1u8; 4]).file_name("x.wav"),
        );
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/transcriptions", env.gateway))
        .bearer_auth(&env.token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"]["param"],
        "transcriptions_requires_per_call_model"
    );
}

/// /v1/audio/translations（老 ok-api 面核对补）：与 transcriptions 同构 per_call。
///
/// 此前名为"按次计费"，却只断言了 200——计费和返回内容都没验。逐接口端到端探针
/// （把这个接口的 2xx 响应体换掉）实测全绿，是个名实不符的空壳。现按 transcriptions 同一契约
/// 核对：上游返回体原样透给客户端，按 per_call × 时长单位计费并进快照。
#[tokio::test]
async fn translations_bills_per_call() {
    let env = setup().await;
    let form = reqwest::multipart::Form::new()
        .text("model", env.stt_model.clone())
        .part(
            "file",
            reqwest::multipart::Part::bytes(vec![7u8; 16])
                .file_name("clip.wav")
                .mime_str("audio/wav")
                .unwrap(),
        );
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/translations", env.gateway))
        .bearer_auth(&env.token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "translations 应可用");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["text"], "hello", "上游返回体应原样透给客户端：{body}");

    let (amount, snapshot) = wait_record(&env.pg, env.user_id, &env.stt_model).await;
    assert_eq!(amount, 6000, "与 transcriptions 同构：per_call $0.006");
    let snapshot = snapshot.expect("计费快照必须存在");
    assert_eq!(snapshot["media_units"], 4, "duration 3.4s 向上取整入快照");
}

#[tokio::test]
async fn speech_failure_refunds_and_records_a_failed_terminal() {
    let env = setup_input("test-upstream-failure").await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/audio/speech", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.tts_model,"input":"test-upstream-failure","voice":"alloy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    for _ in 0..50 {
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1 AND log_type=5 AND amount_micro=0 AND status=40").bind(env.user_id).fetch_one(&env.pg).await.unwrap();
        if count == 1 {
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user_id)
                    .await
                    .unwrap()
                    .as_micros(),
                1_000_000
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("missing failed speech audit");
}

/// 语音端点也参与渠道 key 健康反馈：上游 503 记一次连续失败，下一次成功清零。
/// 此前 audio 两个端点对 key 健康完全不做反馈。
#[tokio::test]
async fn speech_reports_key_health_both_ways() {
    let env = setup().await;
    let speak = |input: &'static str| {
        reqwest::Client::new()
            .post(format!("http://{}/v1/audio/speech", env.gateway))
            .bearer_auth(&env.token)
            .json(&json!({"model": env.tts_model, "input": input, "voice": "alloy"}))
            .send()
    };
    let failed_count = || async {
        sqlx::query_scalar::<_, i32>("SELECT failed_count FROM channel_keys WHERE id = $1")
            .bind(env.channel_key)
            .fetch_one(&env.pg)
            .await
            .unwrap()
    };
    assert_ne!(speak("upstream-unavailable").await.unwrap().status(), 200);
    assert_eq!(
        failed_count().await,
        1,
        "a 503 counts as a transient failure"
    );
    assert_eq!(speak("hello world").await.unwrap().status(), 200);
    assert_eq!(failed_count().await, 0, "the next success clears it");
}
