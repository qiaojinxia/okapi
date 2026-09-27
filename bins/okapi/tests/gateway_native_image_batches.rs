//! Real HTTP gateway, PostgreSQL and Redis; deterministic native Gemini peer.
//! Each case owns a temporary database, never resets the developer database.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use okapi::{gateway, gateway::images::batches::run_one};
use okapi_domain::Money;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

const BALANCE: i64 = 1_000_000;
const PRICE: i64 = 40_000;
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnative-private-image";
#[path = "support/native_batch_admission.rs"]
mod admission;
#[path = "support/native_batch_archive.rs"]
mod archive;
#[path = "support/native_batch_cleanup.rs"]
mod cleanup;
#[path = "support/native_batch_concurrency.rs"]
mod concurrency;
#[path = "support/native_batch_filters.rs"]
mod filters;
#[path = "support/native_batch_recovery.rs"]
mod recovery;
#[path = "support/native_batch_statistics.rs"]
mod statistics;
#[path = "support/native_batch_vertex.rs"]
mod vertex;

#[derive(Clone)]
struct RemoteJob {
    file: String,
    display: String,
    model: String,
}
impl RemoteJob {
    fn metadata(&self) -> Value {
        json!({"displayName":self.display,"model":self.model,"inputConfig":{"fileName":self.file}})
    }
}
#[derive(Default)]
struct Peer {
    files: HashMap<String, Bytes>,
    jobs: HashMap<String, RemoteJob>,
    calls: Vec<(Method, String, Value)>,
    mode: String,
    create_status: u16,
    list_pages: Option<Vec<(u16, Value)>>,
    list_queries: Vec<Option<String>>,
    get_metadata: Option<Value>,
    get_status: Option<u16>,
    delete_status_once: Option<u16>,
    delete_file_status_once: Option<u16>,
    chat_gate: Option<Arc<tokio::sync::Semaphore>>,
}
impl Peer {
    fn upload(
        &mut self,
        parts: &axum::http::request::Parts,
        bytes: Bytes,
        value: &Value,
    ) -> Response {
        if parts.headers["x-goog-upload-command"] == "start" {
            let name = value["file"]["name"]
                .as_str()
                .unwrap()
                .strip_prefix("files/")
                .unwrap();
            return Response::builder()
                .header(
                    "x-goog-upload-url",
                    format!(
                        "http://{}/upload/v1beta/files?id={name}",
                        parts.headers["host"].to_str().unwrap()
                    ),
                )
                .body(Body::empty())
                .unwrap();
        }
        let id = parts.uri.query().unwrap().strip_prefix("id=").unwrap();
        let name = format!("files/{id}");
        self.files.insert(name.clone(), bytes);
        axum::Json(json!({"file":{"name":name,"state":"ACTIVE"}})).into_response()
    }
    fn delete_job(&mut self, name: &str) -> Response {
        let exists = self.jobs.remove(name).is_some();
        if let Some(status) = self.delete_status_once.take() {
            return StatusCode::from_u16(status).unwrap().into_response();
        }
        if exists {
            StatusCode::NO_CONTENT.into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }
    fn listing(&mut self, query: Option<&str>) -> Response {
        let url =
            reqwest::Url::parse(&format!("http://peer/?{}", query.unwrap_or_default())).unwrap();
        let cursor = url
            .query_pairs()
            .find(|(k, _)| k == "pageToken")
            .map(|(_, v)| v.into_owned());
        let index = cursor.as_deref().map_or(0, |s| {
            s.strip_prefix("page-").unwrap().parse::<usize>().unwrap()
        });
        self.list_queries.push(cursor);
        if let Some(pages) = &self.list_pages {
            let (status, body) = &pages[index];
            return (
                StatusCode::from_u16(*status).unwrap(),
                axum::Json(body.clone()),
            )
                .into_response();
        }
        let rows: Vec<Value> = self
            .jobs
            .iter()
            .map(|(name, job)| json!({"name":name,"metadata":job.metadata()}))
            .collect();
        axum::Json(json!({"operations":rows})).into_response()
    }
    fn job(&mut self, name: &str) -> Response {
        if name.ends_with(":cancel") {
            self.mode = "cancelled".into();
            return axum::Json(json!({})).into_response();
        }
        if let Some(status) = self.get_status {
            return StatusCode::from_u16(status).unwrap().into_response();
        }
        let Some(job) = self.jobs.get(name) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut metadata = self.get_metadata.clone().unwrap_or_else(|| job.metadata());
        if self.mode == "running" {
            metadata["state"] = json!("JOB_STATE_RUNNING");
            return axum::Json(json!({"name":name,"metadata":metadata})).into_response();
        }
        let cancelled = self.mode.starts_with("cancelled");
        metadata["state"] = json!(if cancelled {
            "JOB_STATE_CANCELLED"
        } else {
            "JOB_STATE_SUCCEEDED"
        });
        if self.mode != "cancelled" {
            let lines = std::str::from_utf8(self.files.get(&job.file).unwrap()).unwrap();
            let mut outputs: Vec<Value> = lines.lines().enumerate().map(|(index,line)|{
                let item:Value=serde_json::from_str(line).unwrap();
                if matches!(self.mode.as_str(), "partial" | "cancelled_partial") && index>0 {
                    json!({"key":item["key"],"error":{"code":3}})
                } else {
                    json!({"key":item["key"],"response":{"candidates":[{"content":{"parts":[{"inlineData":{"mimeType":"image/png","data":base64::prelude::BASE64_STANDARD.encode(PNG)}}]}}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":12,"thoughtsTokenCount":2,"totalTokenCount":22}}})
                }
            }).collect();
            if self.mode == "duplicate" {
                outputs.push(outputs[0].clone());
            }
            if self.mode == "file" {
                let output_name =
                    format!("files/result-{}", name.strip_prefix("batches/").unwrap());
                let mut lines = String::new();
                for output in outputs {
                    writeln!(&mut lines, "{output}").unwrap();
                }
                self.files.insert(output_name.clone(), Bytes::from(lines));
                metadata["output"] = json!({"responsesFile":output_name});
            } else {
                metadata["output"] = json!({"inlinedResponses":{"inlinedResponses":outputs}});
            }
        }
        let mut result = json!({"name":name,"done":true,"metadata":metadata});
        if cancelled {
            result["error"] = json!({"code":1});
        }
        axum::Json(result).into_response()
    }
}
async fn peer(State(peer): State<Arc<Mutex<Peer>>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 128 * 1024 * 1024).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if parts.uri.path().ends_with(":generateContent") {
        return concurrency::chat_peer(&peer, &parts, value).await;
    }
    let mut peer = peer.lock().unwrap();
    let path = parts.uri.path();
    peer.calls
        .push((parts.method.clone(), path.into(), value.clone()));
    if let Some(name) = path
        .strip_prefix("/download/v1beta/")
        .and_then(|p| p.strip_suffix(":download"))
    {
        assert_eq!(parts.method, Method::GET);
        return peer.files.get(name).map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |bytes| bytes.clone().into_response(),
        );
    }
    if path == "/upload/v1beta/files" {
        return peer.upload(&parts, bytes, &value);
    }
    if let Some(name) = path
        .strip_prefix("/v1beta/")
        .filter(|p| p.starts_with("files/"))
    {
        if parts.method == Method::DELETE {
            if let Some(status) = peer.delete_file_status_once.take() {
                return StatusCode::from_u16(status).unwrap().into_response();
            }
            return if peer.files.remove(name).is_some() {
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            };
        }
        return if peer.files.contains_key(name) {
            axum::Json(json!({"name":name,"state":"ACTIVE"})).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        };
    }
    if path.ends_with(":batchGenerateContent") {
        let file = value["batch"]["inputConfig"]["fileName"].as_str().unwrap();
        let name = format!("batches/{}", file.strip_prefix("files/").unwrap());
        peer.jobs.insert(
            name.clone(),
            RemoteJob {
                file: file.into(),
                display: value["batch"]["displayName"].as_str().unwrap().into(),
                model: path
                    .strip_prefix("/v1beta/")
                    .unwrap()
                    .strip_suffix(":batchGenerateContent")
                    .unwrap()
                    .into(),
            },
        );
        if peer.create_status != 200 {
            return Response::builder()
                .status(peer.create_status)
                .body(Body::from("provider-secret-must-not-leak"))
                .unwrap();
        }
        return axum::Json(json!({"name":name,"metadata":{"state":"JOB_STATE_PENDING"}}))
            .into_response();
    }
    if path == "/v1beta/batches" {
        return peer.listing(parts.uri.query());
    }
    if let Some(name) = path
        .strip_prefix("/v1beta/")
        .filter(|p| p.starts_with("batches/"))
    {
        if parts.method == Method::DELETE {
            return peer.delete_job(name);
        }
        return peer.job(name);
    }
    StatusCode::NOT_FOUND.into_response()
}
struct Env {
    admin: sqlx::PgPool,
    database: String,
    name: String,
    state: gateway::state::AppState,
    address: String,
    peer: Arc<Mutex<Peer>>,
    client: reqwest::Client,
    token: String,
    uid: i64,
    kid: i64,
    model: String,
}
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    address
}
impl Env {
    async fn isolate_ids(pg: &sqlx::PgPool) {
        // Databases are isolated; Redis is shared, so serial IDs must also be isolated.
        let seed = i64::try_from(Uuid::new_v4().as_u128() % 1_000_000_000_000).unwrap()
            + 2_000_000_000_000;
        for table in ["users", "api_keys", "channels", "channel_keys", "models"] {
            sqlx::query("SELECT setval(pg_get_serial_sequence($1,'id'),$2,false)")
                .bind(table)
                .bind(seed)
                .execute(pg)
                .await
                .unwrap();
        }
    }
    async fn new() -> Self {
        let base = std::env::var("DATABASE_URL").unwrap();
        let admin = okapi_store::connect_pg(&base).await.unwrap();
        let name = format!("okapi_batch_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await
            .unwrap();
        let database = format!("{}/{}", base.rsplit_once('/').unwrap().0, name);
        let pg = okapi_store::connect_pg(&database).await.unwrap();
        okapi_store::run_migrations(&pg).await.unwrap();
        Self::isolate_ids(&pg).await;
        sqlx::query("INSERT INTO settings(key,value) VALUES('ssrf_policy',$1)")
            .bind(json!({"allow_http":true,"allow_private":true}))
            .execute(&pg)
            .await
            .unwrap();
        let model = format!("batch-image-{}", Uuid::new_v4().simple());
        let uid = okapi_store::provision::create_user(&pg, &model)
            .await
            .unwrap();
        let token = format!("sk-{model}");
        let kid = okapi_store::provision::create_api_key(
            &pg,
            uid,
            &hex::encode(Sha256::digest(token.as_bytes())),
            "batch",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE users SET balance_micro=$2 WHERE id=$1")
            .bind(uid)
            .bind(BALANCE)
            .execute(&pg)
            .await
            .unwrap();
        let mid: i64 = sqlx::query_scalar("INSERT INTO models(model_name) VALUES($1) RETURNING id")
            .bind(&model)
            .fetch_one(&pg)
            .await
            .unwrap();
        sqlx::query("INSERT INTO model_pricing(model_id,pricing_mode,per_call_price_micro) VALUES($1,'per_call',$2)").bind(mid).bind(PRICE).execute(&pg).await.unwrap();
        let peer = Arc::new(Mutex::new(Peer {
            mode: "success".into(),
            create_status: 200,
            ..Peer::default()
        }));
        let upstream = serve(Router::new().fallback(self::peer).with_state(peer.clone())).await;
        okapi_store::provision::create_channel(
            &pg,
            &model,
            "gemini",
            &format!("{upstream}/v1beta"),
            "batch-private-credential",
            &[&model],
            false,
            None,
        )
        .await
        .unwrap();
        pg.close().await;
        let state = gateway::build_state(
            &database,
            &std::env::var("OKAPI_REDIS_URL").unwrap(),
            "batch-test",
            None,
            None,
        )
        .await
        .unwrap();
        state
            .settings_cache
            .insert("image_batches_enabled".into(), Arc::new(Some(json!(true))))
            .await;
        state
            .ledger
            .credit(uid, Money::from_micros(BALANCE))
            .await
            .unwrap();
        let address = serve(gateway::router(state.clone())).await;
        Self {
            admin,
            database,
            name,
            state,
            address,
            peer,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
            token,
            uid,
            kid,
            model,
        }
    }
    fn body(&self, n: u32) -> Value {
        json!({"model":self.model,"task_name":"native test","image_size":"1K","aspect_ratio":"1:1","items":[{"custom_id":"drawing","prompt":"draw a cat","output_count":n}]})
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.address))
            .bearer_auth(&self.token)
    }
    async fn submit(&self, n: u32, idem: &str) -> Value {
        let response = self
            .request(reqwest::Method::POST, "/v1/images/batches")
            .header("idempotency-key", idem)
            .json(&self.body(n))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202, "{}", response.text().await.unwrap());
        response.json().await.unwrap()
    }
    async fn value(&self, method: reqwest::Method, path: &str) -> Value {
        let response = self.request(method, path).send().await.unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
        response.json().await.unwrap()
    }
    async fn step(&self, job: &Value) -> Result<bool, gateway::error::AppError> {
        sqlx::query("UPDATE image_batches SET next_run_at=now() WHERE id=$1")
            .bind(id(job))
            .execute(&self.state.pg)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), run_one(&self.state, Some(id(job))))
            .await
            .unwrap()
    }
    async fn poll(&self, job: &Value) -> Value {
        self.value(reqwest::Method::GET, job["poll_url"].as_str().unwrap())
            .await
    }
    fn creates(&self) -> usize {
        self.peer
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(_, p, _)| p.ends_with(":batchGenerateContent"))
            .count()
    }
    async fn money(&self, job: &Value, amount: i64) -> Value {
        let records: Vec<(i64, Value)> = sqlx::query_as(
            "SELECT amount_micro,pricing_snapshot FROM billing_records WHERE request_id=$1",
        )
        .bind(id(job))
        .fetch_all(&self.state.pg)
        .await
        .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, amount);
        let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE id=$1")
            .bind(self.kid)
            .fetch_one(&self.state.pg)
            .await
            .unwrap();
        assert_eq!(used, amount);
        assert_eq!(
            self.state
                .ledger
                .balance(self.uid)
                .await
                .unwrap()
                .as_micros(),
            BALANCE - amount
        );
        let hold: (String, Option<i64>) =
            sqlx::query_as("SELECT state,actual_micro FROM balance_holds WHERE id=$1")
                .bind(id(job))
                .fetch_one(&self.state.pg)
                .await
                .unwrap();
        assert_eq!(hold, ("closed".into(), Some(amount)));
        records[0].1.clone()
    }
    async fn close(self) {
        self.state.pg.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE \"{}\" WITH (FORCE)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}
fn id(job: &Value) -> Uuid {
    Uuid::parse_str(
        job["id"]
            .as_str()
            .unwrap()
            .strip_prefix("imgbatch_")
            .unwrap(),
    )
    .unwrap()
}
fn path(job: &Value, suffix: &str) -> String {
    format!("{}{suffix}", job["poll_url"].as_str().unwrap())
}

#[tokio::test]
async fn native_lifecycle_preserves_price_and_bills_only_delivered_images() {
    let env = Env::new().await;
    env.peer.lock().unwrap().mode = "partial".into();
    let job = env.submit(3, "once").await;
    assert_eq!(job["status"], "funding");
    assert_eq!(env.submit(3, "once").await["id"], job["id"]);
    assert_eq!(env.creates(), 0);
    env.step(&job).await.unwrap();
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        BALANCE - PRICE * 3 / 2
    );
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "settling");
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&job, "/content/0"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    sqlx::query("UPDATE model_pricing SET per_call_price_micro=999999")
        .execute(&env.state.pg)
        .await
        .unwrap();
    // A new process must use persisted account/price/results.
    let restarted = gateway::build_state(
        &env.database,
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "other-host",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(run_one(&restarted, Some(id(&job))).await.unwrap());
    restarted.pg.close().await;
    assert_eq!(env.poll(&job).await["status"], "partial");
    let pricing = env.money(&job, PRICE / 2).await;
    assert_eq!(pricing["media_units"], 1);
    assert_eq!(pricing["batch_ratio_milli"], 500);
    let items = env.value(reqwest::Method::GET, &path(&job, "/items")).await;
    assert!(items["data"][0]["outputs"][0]["url"].is_string());
    let response = env
        .request(reqwest::Method::GET, &path(&job, "/content/0"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.bytes().await.unwrap().as_ref(), PNG);
    assert!(env.poll(&job).await["downloaded_at"].is_number());
    assert!(!env.step(&job).await.unwrap());
    assert_eq!(env.creates(), 1);
    let exposed = env.poll(&job).await.to_string();
    assert!(!exposed.contains("credential"));
    assert!(!exposed.contains("submit_intent"));
    env.close().await;
}

#[tokio::test]
async fn cancellation_and_revocation_before_dispatch_close_pending_or_frozen_holds() {
    for (freeze, revoke, disable_model) in [
        (false, false, false),
        (false, true, false),
        (true, true, false),
        (true, false, true),
    ] {
        let env = Env::new().await;
        let job = env.submit(1, "cancel").await;
        if freeze {
            env.step(&job).await.unwrap();
        }
        if revoke {
            sqlx::query("UPDATE api_keys SET status=2 WHERE id=$1")
                .bind(env.kid)
                .execute(&env.state.pg)
                .await
                .unwrap();
        } else if disable_model {
            sqlx::query("UPDATE models SET status=2 WHERE model_name=$1")
                .bind(&env.model)
                .execute(&env.state.pg)
                .await
                .unwrap();
        } else {
            env.value(reqwest::Method::POST, &path(&job, "/cancel"))
                .await;
        }
        env.step(&job).await.unwrap();
        env.step(&job).await.unwrap();
        env.money(&job, 0).await;
        assert_eq!(env.creates(), 0);
        assert!(env.peer.lock().unwrap().calls.is_empty());
        env.close().await;
    }
}

#[tokio::test]
async fn malformed_results_stay_private_and_retry_does_not_duplicate_charges() {
    let env = Env::new().await;
    env.peer.lock().unwrap().mode = "duplicate".into();
    let job = env.submit(1, "bad-output").await;
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    assert!(env.step(&job).await.is_err());
    assert_eq!(env.poll(&job).await["status"], "collecting");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE request_id=$1")
        .bind(id(&job))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&job, "/content/0"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    env.peer.lock().unwrap().mode = "success".into();
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    env.money(&job, PRICE / 2).await;
    assert_eq!(env.creates(), 1);
    assert!(!env.step(&job).await.unwrap());
    env.close().await;
}

#[tokio::test]
async fn ambiguous_create_keeps_hold_and_never_resubmits_but_rejection_refunds() {
    for status in [502, 400] {
        let env = Env::new().await;
        env.peer.lock().unwrap().create_status = status;
        let job = env.submit(1, "outcome").await;
        env.step(&job).await.unwrap();
        let result = env.step(&job).await;
        if status == 502 {
            assert!(result.is_err());
            assert_eq!(env.poll(&job).await["status"], "uncertain");
            env.peer.lock().unwrap().list_pages = Some(vec![(200, json!({"operations":[]}))]);
            assert!(env.step(&job).await.unwrap());
            assert_eq!(env.poll(&job).await["status"], "uncertain");
            assert_eq!(env.creates(), 1);
            assert_eq!(
                env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
                BALANCE - PRICE / 2
            );
        } else {
            result.unwrap();
            env.step(&job).await.unwrap();
            env.money(&job, 0).await;
            assert_eq!(env.poll(&job).await["status"], "failed");
        }
        assert!(!env.poll(&job).await.to_string().contains("provider-secret"));
        env.close().await;
    }
}

#[tokio::test]
async fn owner_scope_validation_and_key_budget_are_enforced_before_provider_io() {
    let env = Env::new().await;
    let job = env.submit(1, "owned").await;
    let other = format!("sk-other-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(
        &env.state.pg,
        env.uid,
        &hex::encode(Sha256::digest(other.as_bytes())),
        "other",
    )
    .await
    .unwrap();
    for (method, suffix) in [
        (reqwest::Method::GET, ""),
        (reqwest::Method::GET, "/items"),
        (reqwest::Method::GET, "/content/0"),
        (reqwest::Method::POST, "/cancel"),
        (reqwest::Method::DELETE, ""),
    ] {
        let response = env
            .client
            .request(method, format!("{}{}", env.address, path(&job, suffix)))
            .bearer_auth(&other)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }
    assert_eq!(
        env.request(reqwest::Method::POST, "/v1/images/batches")
            .header("idempotency-key", "owned")
            .json(&env.body(2))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    for body in [
        json!({"model":env.model,"items":[]}),
        json!({"model":env.model,"items":[{"custom_id":"x","prompt":"x","output_count":5}]}),
        json!({"model":env.model,"items":[{"custom_id":"x","prompt":"x"}],"response_mime_type":"image/jpeg"}),
    ] {
        assert_eq!(
            env.request(reqwest::Method::POST, "/v1/images/batches")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    sqlx::query("UPDATE api_keys SET quota_mode=1,quota_micro=$2 WHERE id=$1")
        .bind(env.kid)
        .bind(PRICE / 2)
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(
        env.request(reqwest::Method::POST, "/v1/images/batches")
            .json(&env.body(1))
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(env.submit(1, "owned").await["id"], job["id"]);
    assert_eq!(
        env.request(reqwest::Method::GET, "/v1/images/batches?limit=101")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        env.client
            .post(format!("{}/v1/images/batches", env.address))
            .body("invalid json")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(env.creates(), 0);
    env.close().await;
}

#[tokio::test]
async fn server_pages_twenty_jobs_and_cancellation_waits_for_remote_terminal_state() {
    let env = Env::new().await;
    for i in 0..21 {
        let job = env.submit(1, &format!("page-{i}")).await;
        env.value(reqwest::Method::POST, &path(&job, "/cancel"))
            .await;
        env.step(&job).await.unwrap();
        env.step(&job).await.unwrap();
    }
    let first = env.value(reqwest::Method::GET, "/v1/images/batches").await;
    assert_eq!(first["data"].as_array().unwrap().len(), 20);
    assert_eq!(first["has_more"], true);
    let second = env
        .value(
            reqwest::Method::GET,
            &format!(
                "/v1/images/batches?cursor={}",
                first["next_cursor"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(second["data"].as_array().unwrap().len(), 1);
    assert_eq!(second["has_more"], false);
    let job = env.submit(1, "remote-cancel").await;
    env.peer.lock().unwrap().mode = "running".into();
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    let requested = env
        .value(reqwest::Method::POST, &path(&job, "/cancel"))
        .await;
    assert_eq!(requested["status"], "running");
    assert_eq!(requested["cancel_requested"], true);
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "cancelled");
    env.money(&job, 0).await;
    assert_eq!(env.creates(), 1);
    env.close().await;
}
