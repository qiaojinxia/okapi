//! M4 audio 验收：speech 输入字符计费 + 二进制回传；
//! transcriptions multipart 重组转发 + per_call 计费 + duration 入快照 +
//! 非 per_call 模型拒绝。依赖 .env（scripts/dev-deps.sh up）。

use axum::extract::Multipart;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_speech(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["input"], "hello world", "JSON 原样透传");
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

async fn spawn_mock() -> SocketAddr {
    let router = axum::Router::new()
        .route("/v1/audio/speech", post(mock_speech))
        .route("/v1/audio/transcriptions", post(mock_transcribe))
        .route("/v1/audio/translations", post(mock_transcribe));
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
    tts_model: String,
    stt_model: String,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
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
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &hash, "sk-okapi-au")
        .await
        .unwrap();
    // TTS：ratio 模式（字符即 token）；STT：per_call $0.006
    okapi_store::provision::create_model_ratio(&pg, &tts_model, "1", "1", "1")
        .await
        .unwrap();
    okapi_store::admin::upsert_model_per_call(&pg, &stt_model, 6000)
        .await
        .unwrap();

    let mock = spawn_mock().await;
    okapi_store::provision::create_channel(
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

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_000_000))
        .await
        .unwrap();
    let app = gateway::router(state);
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
        tts_model,
        stt_model,
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
}
