use super::*;
use gateway::images::tasks::objects::{Storage, StorageConfig};
use std::collections::HashMap;
use std::sync::{Mutex, atomic::AtomicBool};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nprivate-fixture";
const VERSION: &str = "version/+=";

struct TempDb {
    admin: sqlx::PgPool,
    name: String,
}
impl TempDb {
    async fn env(config: Value) -> (Env, Self) {
        let url = std::env::var("DATABASE_URL").unwrap();
        let admin = okapi_store::connect_pg(&url).await.unwrap();
        let name = format!("okapi_img_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await
            .unwrap();
        let database = format!("{}/{}", url.rsplit_once('/').unwrap().0, name);
        let pg = okapi_store::connect_pg(&database).await.unwrap();
        okapi_store::run_migrations(&pg).await.unwrap();
        // Fresh databases share Redis; IDs must not collide with other test fixtures.
        let seed = i64::try_from(Uuid::new_v4().as_u128() % 1_000_000_000_000).unwrap()
            + 1_000_000_000_000;
        for table in ["users", "api_keys", "channels", "channel_keys", "models"] {
            sqlx::query("SELECT setval(pg_get_serial_sequence($1,'id'),$2,false)")
                .bind(table)
                .bind(seed)
                .execute(&pg)
                .await
                .unwrap();
        }
        pg.close().await;
        let storage = Arc::new(
            Storage::new(serde_json::from_value::<StorageConfig>(config).unwrap()).unwrap(),
        );
        let env = setup_at(&database, Some(storage)).await;
        env.state
            .settings_cache
            .insert("image_tasks_enabled".into(), Arc::new(Some(json!(true))))
            .await;
        (env, Self { admin, name })
    }
    async fn close(self, env: Env) {
        env.state.pg.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE \"{}\" WITH (FORCE)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}

#[derive(Default)]
struct Bucket {
    data: Mutex<HashMap<String, Vec<u8>>>,
    puts: AtomicUsize,
    gets: AtomicUsize,
    deletes: AtomicUsize,
    fail_put: AtomicBool,
    fail_delete: AtomicBool,
    corrupt: AtomicBool,
    pause: AtomicBool,
    started: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
async fn bucket() -> (SocketAddr, Arc<Bucket>) {
    let state = Arc::new(Bucket::default());
    let handler = state.clone();
    let address = serve(Router::new().fallback(move |req: Request| {
        let state = handler.clone();
        async move {
            let method = req.method().clone();
            let uri = req.uri().clone();
            let headers = req.headers().clone();
            let auth = headers["authorization"].to_str().unwrap();
            assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=test-access/"));
            assert!(!headers.contains_key("cookie"));
            let key = uri.path().to_owned();
            if method == axum::http::Method::PUT {
                state.puts.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers["if-none-match"], "*");
                let body = axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
                    .await
                    .unwrap()
                    .to_vec();
                assert_eq!(
                    headers["x-amz-content-sha256"],
                    okapi_providers::aws_sigv4::payload_hash(&body)
                );
                {
                    let mut data = state.data.lock().unwrap();
                    if data.contains_key(&key) {
                        return Response::builder().status(412).body(Body::empty()).unwrap();
                    }
                    data.insert(key, body);
                }
                if state.pause.load(Ordering::SeqCst) {
                    state.started.notify_one();
                    state.resume.notified().await;
                }
                if state.fail_put.swap(false, Ordering::SeqCst) {
                    return Response::builder().status(503).body(Body::empty()).unwrap();
                }
                Response::builder()
                    .header("x-amz-version-id", VERSION)
                    .body(Body::empty())
                    .unwrap()
            } else if method == axum::http::Method::DELETE {
                state.deletes.fetch_add(1, Ordering::SeqCst);
                let parsed = reqwest::Url::parse(&format!("http://mock{uri}")).unwrap();
                assert!(
                    parsed
                        .query_pairs()
                        .any(|(k, v)| k == "versionId" && v == VERSION)
                );
                if state.fail_delete.load(Ordering::SeqCst) {
                    return Response::builder().status(503).body(Body::empty()).unwrap();
                }
                state.data.lock().unwrap().remove(&key);
                Response::builder().status(204).body(Body::empty()).unwrap()
            } else {
                state.gets.fetch_add(1, Ordering::SeqCst);
                let body = state.data.lock().unwrap().get(&key).cloned();
                match body {
                    None => Response::builder().status(404).body(Body::empty()).unwrap(),
                    Some(body) => Response::builder()
                        .header("x-amz-version-id", VERSION)
                        .body(if method == axum::http::Method::HEAD {
                            Body::empty()
                        } else if state.corrupt.load(Ordering::SeqCst) {
                            Body::from("damaged")
                        } else {
                            Body::from(body)
                        })
                        .unwrap(),
                }
            }
        }
    }))
    .await;
    (address, state)
}
fn config(address: SocketAddr) -> Value {
    json!({"active":"assets-v1","stores":[{"id":"assets-v1","endpoint":format!("http://{address}"),"bucket":"private-images","region":"us-east-1","access_key_id":"test-access","secret_access_key":"test-secret","session_token":"test-session","allow_http":true}]})
}
async fn complete(env: &mut Env) -> Value {
    let task = submit(env, env.body(1), &Uuid::new_v4().to_string()).await;
    let work = worker(env);
    env.peer().await.raw(
        200,
        json!({"data":[{"b64_json":base64::prelude::BASE64_STANDARD.encode(PNG)}]}).to_string(),
    );
    join(work).await;
    assert_eq!(poll(env, &task).await["status"], "completed");
    task
}
async fn download(env: &Env, task: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .get(url(
            env,
            &format!("{}/content/0", task["poll_url"].as_str().unwrap()),
        ))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap()
}
async fn expire_result(env: &Env, task: &Value) {
    sqlx::query("UPDATE image_tasks SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(id(task))
        .execute(&env.state.pg)
        .await
        .unwrap();
}

#[tokio::test]
async fn storage_empty_base64_is_rejected_without_billing() {
    let (address, bucket) = bucket().await;
    let (mut env, db) = TempDb::env(config(address)).await;
    let task = submit(&env, env.body(1), &Uuid::new_v4().to_string()).await;
    let work = worker(&env);
    env.peer().await.raw(
        200,
        json!({"data":[{"b64_json":"","url":"https://image.invalid/unused"}]}).to_string(),
    );
    join(work).await;
    assert_eq!(poll(&env, &task).await["status"], "failed");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM image_task_artifacts")
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(bucket.puts.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
    db.close(env).await;
}

#[tokio::test]
async fn storage_empty_legacy_artifacts_do_not_stall_workers() {
    let (address, bucket) = bucket().await;
    let (mut env, db) = TempDb::env(config(address)).await;
    let legacy = complete(&mut env).await;
    // Reproduce a zero-byte artifact persisted by the old base64 result path.
    sqlx::query("UPDATE image_task_artifacts SET content=$2 WHERE task_id=$1")
        .bind(id(&legacy))
        .bind(Vec::<u8>::new())
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert!(!run_one(&env.state).await.unwrap());
    let next = complete(&mut env).await;
    assert!(run_one(&env.state).await.unwrap());
    assert_eq!(bucket.puts.load(Ordering::SeqCst), 1);
    assert_eq!(
        download(&env, &next).await.bytes().await.unwrap().as_ref(),
        PNG
    );
    expire_result(&env, &legacy).await;
    store::cleanup(&env.state.pg).await.unwrap();
    let retained: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM image_tasks WHERE id=$1)")
        .bind(id(&legacy))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert!(!retained);
    env.assert_money(PRICE * 2, 2).await;
    db.close(env).await;
}

#[tokio::test]
async fn s3_offload_uses_private_downloads_validates_content_and_deletes_the_version() {
    let (address, bucket) = bucket().await;
    let (mut env, db) = TempDb::env(config(address)).await;
    let task = complete(&mut env).await;
    assert!(run_one(&env.state).await.unwrap());
    let cleared:bool=sqlx::query_scalar("SELECT content IS NULL AND object_id IS NOT NULL FROM image_task_artifacts WHERE task_id=$1").bind(id(&task)).fetch_one(&env.state.pg).await.unwrap();
    assert!(cleared);
    let saved = poll(&env, &task).await.to_string();
    assert!(
        !saved.contains("test-secret")
            && !saved.contains("test-access")
            && !saved.contains("private-images")
    );
    assert_eq!(
        download(&env, &task).await.bytes().await.unwrap().as_ref(),
        PNG
    );
    bucket.corrupt.store(true, Ordering::SeqCst);
    assert_eq!(download(&env, &task).await.status(), 502);
    bucket.corrupt.store(false, Ordering::SeqCst);
    env.assert_money(PRICE, 1).await;
    expire_result(&env, &task).await;
    store::cleanup(&env.state.pg).await.unwrap();
    assert_eq!(bucket.data.lock().unwrap().len(), 1);
    assert!(run_one(&env.state).await.unwrap());
    store::cleanup(&env.state.pg).await.unwrap();
    assert!(bucket.data.lock().unwrap().is_empty());
    assert_eq!(bucket.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(download(&env, &task).await.status(), 404);
    db.close(env).await;
}

#[tokio::test]
async fn uncertain_put_keeps_pg_readable_and_retries_without_overwrite_or_extra_billing() {
    let (address, bucket) = bucket().await;
    bucket.fail_put.store(true, Ordering::SeqCst);
    let (mut env, db) = TempDb::env(config(address)).await;
    let task = complete(&mut env).await;
    assert!(run_one(&env.state).await.unwrap());
    assert_eq!(
        download(&env, &task).await.bytes().await.unwrap().as_ref(),
        PNG
    );
    sqlx::query("UPDATE image_task_objects SET retry_at=now() WHERE task_id=$1")
        .bind(id(&task))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert!(run_one(&env.state).await.unwrap());
    assert_eq!(bucket.puts.load(Ordering::SeqCst), 2);
    assert_eq!(bucket.data.lock().unwrap().len(), 1);
    assert_eq!(
        download(&env, &task).await.bytes().await.unwrap().as_ref(),
        PNG
    );
    env.assert_money(PRICE, 1).await;
    db.close(env).await;
}

#[tokio::test]
async fn upload_intent_survives_crash_and_concurrent_workers_recover_once() {
    let (address, bucket) = bucket().await;
    bucket.pause.store(true, Ordering::SeqCst);
    let (mut env, db) = TempDb::env(config(address)).await;
    let task = complete(&mut env).await;
    let upload = worker(&env);
    timeout(WAIT, bucket.started.notified()).await.unwrap();
    upload.abort();
    assert!(upload.await.unwrap_err().is_cancelled());
    bucket.pause.store(false, Ordering::SeqCst);
    bucket.resume.notify_one();
    assert_eq!(
        download(&env, &task).await.bytes().await.unwrap().as_ref(),
        PNG
    );
    sqlx::query(
        "UPDATE image_task_objects SET lease_until=now()-interval '1 second' WHERE task_id=$1",
    )
    .bind(id(&task))
    .execute(&env.state.pg)
    .await
    .unwrap();
    let works: Vec<_> = (0..4).map(|_| worker(&env)).collect();
    for work in works {
        timeout(WAIT, work).await.unwrap().unwrap().unwrap();
    }
    assert_eq!(bucket.puts.load(Ordering::SeqCst), 2);
    assert_eq!(bucket.data.lock().unwrap().len(), 1);
    assert_eq!(
        download(&env, &task).await.bytes().await.unwrap().as_ref(),
        PNG
    );
    env.assert_money(PRICE, 1).await;
    db.close(env).await;
}

#[tokio::test]
async fn failed_object_delete_retains_durable_state_until_cleanup_can_retry() {
    let (address, bucket) = bucket().await;
    let (mut env, db) = TempDb::env(config(address)).await;
    let task = complete(&mut env).await;
    assert!(run_one(&env.state).await.unwrap());
    bucket.fail_delete.store(true, Ordering::SeqCst);
    expire_result(&env, &task).await;
    assert!(run_one(&env.state).await.unwrap());
    store::cleanup(&env.state.pg).await.unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM image_tasks WHERE id=$1)")
        .bind(id(&task))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert!(exists);
    bucket.fail_delete.store(false, Ordering::SeqCst);
    sqlx::query("UPDATE image_task_objects SET retry_at=now() WHERE task_id=$1")
        .bind(id(&task))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert!(run_one(&env.state).await.unwrap());
    store::cleanup(&env.state.pg).await.unwrap();
    assert!(bucket.data.lock().unwrap().is_empty());
    env.assert_money(PRICE, 1).await;
    db.close(env).await;
}

#[tokio::test]
async fn expired_uncertain_upload_discovers_and_deletes_its_version() {
    let (address, bucket) = bucket().await;
    bucket.fail_put.store(true, Ordering::SeqCst);
    let (mut env, db) = TempDb::env(config(address)).await;
    let task = complete(&mut env).await;
    assert!(run_one(&env.state).await.unwrap());
    expire_result(&env, &task).await;
    sqlx::query("UPDATE image_task_objects SET retry_at=now() WHERE task_id=$1")
        .bind(id(&task))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert!(run_one(&env.state).await.unwrap());
    assert_eq!(bucket.gets.load(Ordering::SeqCst), 1); // HEAD recovers the version lost with the PUT reply.
    assert_eq!(bucket.deletes.load(Ordering::SeqCst), 1);
    assert!(bucket.data.lock().unwrap().is_empty());
    store::cleanup(&env.state.pg).await.unwrap();
    env.assert_money(PRICE, 1).await;
    db.close(env).await;
}

#[tokio::test]
async fn retired_stores_remain_readable_and_reusing_an_id_cannot_redirect_private_reads() {
    let (address, bucket) = bucket().await;
    let (mut env, db) = TempDb::env(config(address)).await;
    let task = complete(&mut env).await;
    assert!(run_one(&env.state).await.unwrap());
    let other = format!("sk-other-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(
        &env.state.pg,
        env.user,
        &hex::encode(Sha256::digest(other.as_bytes())),
        "other",
    )
    .await
    .unwrap();
    let path = format!("{}/content/0", task["poll_url"].as_str().unwrap());
    assert_eq!(
        reqwest::Client::new()
            .get(url(&env, &path))
            .bearer_auth(other)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(bucket.gets.load(Ordering::SeqCst), 0);
    let mut retired = config(address);
    retired["active"] = Value::Null;
    env.state.image_storage =
        Arc::new(Storage::new(serde_json::from_value(retired.clone()).unwrap()).unwrap());
    env.address = serve(gateway::router(env.state.clone())).await;
    assert_eq!(
        download(&env, &task).await.bytes().await.unwrap().as_ref(),
        PNG
    );
    retired["stores"][0]["bucket"] = json!("another-bucket");
    env.state.image_storage =
        Arc::new(Storage::new(serde_json::from_value(retired).unwrap()).unwrap());
    env.address = serve(gateway::router(env.state.clone())).await;
    let response = download(&env, &task).await;
    assert_eq!(response.status(), 502);
    let body = response.text().await.unwrap();
    assert!(!body.contains("test-secret"));
    assert_eq!(bucket.gets.load(Ordering::SeqCst), 1);
    env.assert_money(PRICE, 1).await;
    db.close(env).await;
}

#[tokio::test]
async fn remote_image_urls_and_data_urls_become_private_persisted_bytes() {
    let hits = Arc::new(AtomicUsize::new(0));
    let observed = hits.clone();
    let source = serve(Router::new().fallback(move |request: Request| {
        let hits = observed.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            assert!(!request.headers().contains_key("authorization"));
            assert!(!request.headers().contains_key("cookie"));
            if request.uri().path() == "/redirect" {
                Response::builder()
                    .status(302)
                    .header("location", "/asset?temporary-secret=one")
                    .body(Body::empty())
                    .unwrap()
            } else {
                Response::builder()
                    .header("content-type", "image/png")
                    .body(Body::from(PNG))
                    .unwrap()
            }
        }
    }))
    .await;
    let (mut env, db) = TempDb::env(
        json!({"copy_urls":true,"fetch":{"trusted_origins":[format!("http://{source}")]}}),
    )
    .await;
    for source_url in [
        format!("http://{source}/redirect"),
        format!(
            "data:image/png;base64,{}",
            base64::prelude::BASE64_STANDARD.encode(PNG)
        ),
    ] {
        let task = submit(&env, env.body(1), &Uuid::new_v4().to_string()).await;
        let work = worker(&env);
        env.peer().await.raw(
            200,
            json!({"data":[{"url":source_url,"revised_prompt":"retained"}]}).to_string(),
        );
        join(work).await;
        let result = poll(&env, &task).await;
        assert_eq!(result["status"], "completed");
        assert_eq!(result["result"]["data"][0]["revised_prompt"], "retained");
        assert!(!result.to_string().contains("temporary-secret"));
        assert_eq!(
            download(&env, &task).await.bytes().await.unwrap().as_ref(),
            PNG
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    env.assert_money(PRICE * 2, 2).await;
    db.close(env).await;
}

#[tokio::test]
async fn unsafe_or_invalid_remote_results_fail_without_billing_or_replaying_generation() {
    let trapped = Arc::new(AtomicUsize::new(0));
    let seen = trapped.clone();
    let trap = serve(Router::new().fallback(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        async { "private" }
    }))
    .await;
    let source = serve(Router::new().fallback(move |request: Request| async move {
        match request.uri().path() {
            "/redirect" => Response::builder()
                .status(302)
                .header("location", format!("http://{trap}/secret"))
                .body(Body::empty())
                .unwrap(),
            "/large" => Response::builder()
                .header("content-length", 64 * 1024 * 1024 + 1)
                .body(Body::from_stream(futures::stream::once(async {
                    Ok::<_, std::convert::Infallible>(Bytes::from_static(PNG))
                })))
                .unwrap(),
            _ => Response::builder()
                .header("content-type", "image/png")
                .body(Body::from("<html>not an image</html>"))
                .unwrap(),
        }
    }))
    .await;
    let (mut env, db) = TempDb::env(
        json!({"copy_urls":true,"fetch":{"trusted_origins":[format!("http://{source}")]}}),
    )
    .await;
    let sources = [
        format!("https://127.0.0.1:{}/secret", trap.port()),
        format!("https://localhost:{}/secret", trap.port()),
        format!("http://user:password@{source}/asset"),
        format!("http://{source}/redirect"),
        format!("http://{source}/large"),
        format!("http://{source}/html"),
    ];
    for source_url in sources {
        let task = submit(&env, env.body(1), &Uuid::new_v4().to_string()).await;
        let work = worker(&env);
        env.peer()
            .await
            .raw(200, json!({"data":[{"url":source_url}]}).to_string());
        join(work).await;
        let saved = poll(&env, &task).await;
        assert_eq!(saved["status"], "failed");
        assert!(!saved.to_string().contains("password"));
    }
    assert_eq!(trapped.load(Ordering::SeqCst), 0);
    assert_eq!(env.hits.load(Ordering::SeqCst), 6);
    env.assert_money(0, 0).await;
    db.close(env).await;
}
