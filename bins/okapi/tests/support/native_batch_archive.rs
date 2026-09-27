use super::*;
use async_zip::base::read::mem::ZipFileReader;

pub(super) async fn create(env: &Env, name: &str, n: u32) -> Value {
    let mut body = env.body(n);
    body["task_name"] = json!(name);
    body["items"][0]["custom_id"] = json!("../outside\\name-图片");
    let response = env
        .request(reqwest::Method::POST, "/v1/images/batches")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    response.json().await.unwrap()
}
pub(super) async fn finish(env: &Env, job: &Value) {
    for _ in 0..4 {
        env.step(job).await.unwrap();
    }
}
async fn unzip(bytes: Vec<u8>) -> HashMap<String, Vec<u8>> {
    let zip = ZipFileReader::new(bytes).await.unwrap();
    let mut files = HashMap::new();
    for (index, entry) in zip.file().entries().iter().enumerate() {
        let name = entry.filename().as_str().unwrap();
        let mut reader = zip.reader_with_entry(index).await.unwrap();
        let mut data = Vec::new();
        reader.read_to_end_checked(&mut data).await.unwrap();
        assert!(files.insert(name.to_owned(), data).is_none());
    }
    files
}
async fn leases(env: &Env, job: &Value) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM image_batch_downloads WHERE batch_id=$1")
        .bind(id(job))
        .fetch_one(&env.state.pg)
        .await
        .unwrap()
}
async fn released(env: &Env, job: &Value) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while leases(env, job).await != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn direct(env: &Env, job: &Value) -> Response {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", env.token).parse().unwrap(),
    );
    gateway::images::batches::download(
        State(env.state.clone()),
        axum::extract::Path(job["id"].as_str().unwrap().to_owned()),
        Method::GET,
        headers,
    )
    .await
}

#[tokio::test]
async fn archive_http_contains_only_successful_images_and_safe_manifest_without_rebilling() {
    let env = Env::new().await;
    env.peer.lock().unwrap().mode = "partial".into();
    let job = create(&env, "archive", 3).await;
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&job, "/download"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    finish(&env, &job).await;
    let pricing = env.money(&job, PRICE / 2).await;
    let response = env
        .request(reqwest::Method::GET, &path(&job, "/download"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/zip");
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains(job["id"].as_str().unwrap())
    );
    let bytes = response.bytes().await.unwrap().to_vec();
    std::fs::write("/tmp/okapi-native-batch-archive.zip", &bytes).unwrap();
    let files = unzip(bytes).await;
    assert_eq!(files.len(), 2);
    assert_eq!(files["images/0000.png"], PNG);
    let manifest: Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
    assert_eq!(manifest["outputs"].as_array().unwrap().len(), 3);
    assert_eq!(manifest["outputs"][0]["custom_id"], "../outside\\name-图片");
    assert_eq!(manifest["outputs"][1]["file"], Value::Null);
    assert_eq!(manifest["outputs"][1]["status"], "failed");
    for secret in [
        "batch-private-credential",
        "provider_job_name",
        "submit_intent",
        "input_ref",
        "output_ref",
        "draw a cat",
    ] {
        assert!(!manifest.to_string().contains(secret));
    }
    released(&env, &job).await;
    assert_eq!(env.money(&job, PRICE / 2).await, pricing);
    assert!(!env.poll(&job).await["downloaded_at"].is_null());
    let other = format!("sk-other-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(
        &env.state.pg,
        env.uid,
        &hex::encode(Sha256::digest(other.as_bytes())),
        "other",
    )
    .await
    .unwrap();
    assert_eq!(
        env.client
            .get(format!("{}{}", env.address, path(&job, "/download")))
            .bearer_auth(other)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    sqlx::query("UPDATE image_batches SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(id(&job))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&job, "/download"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    env.close().await;
}

#[tokio::test]
async fn archive_backpressure_keeps_cleanup_safe_without_holding_database_connections() {
    let env = Env::new().await;
    let job = create(&env, "large", 1).await;
    finish(&env, &job).await;
    let mut data = vec![42; 1024 * 1024];
    data[..PNG.len()].copy_from_slice(PNG);
    sqlx::query("UPDATE image_batch_outputs SET content=$2,content_hash=$3 WHERE batch_id=$1")
        .bind(id(&job))
        .bind(&data)
        .bind(hex::encode(Sha256::digest(&data)))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut responses = Vec::new();
    for _ in 0..4 {
        let r = direct(&env, &job).await;
        assert_eq!(r.status(), 200);
        responses.push(r);
    }
    assert_eq!(leases(&env, &job).await, 4);
    assert_eq!(direct(&env, &job).await.status(), 429);
    assert_eq!(env.state.image_download_gate.available_permits(), 0);
    tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query("SELECT 1").execute(&env.state.pg),
    )
    .await
    .unwrap()
    .unwrap();
    env.value(reqwest::Method::DELETE, &path(&job, "")).await;
    assert!(!env.cleanup(&job).await.unwrap());
    let response = responses.pop().unwrap();
    drop(responses);
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(unzip(bytes.to_vec()).await["images/0000.png"], data);
    released(&env, &job).await;
    assert_eq!(env.state.image_download_gate.available_permits(), 4);
    assert!(env.cleanup(&job).await.unwrap());
    assert!(env.cleaned(&job).await);
    env.money(&job, PRICE / 2).await;
    env.close().await;
}

#[tokio::test]
async fn archive_corruption_fails_the_body_and_releases_capacity() {
    let env = Env::new().await;
    let job = create(&env, "corruption", 1).await;
    finish(&env, &job).await;
    sqlx::query("UPDATE image_batch_outputs SET content=$2 WHERE batch_id=$1")
        .bind(id(&job))
        .bind(b"changed".as_slice())
        .execute(&env.state.pg)
        .await
        .unwrap();
    let response = direct(&env, &job).await;
    assert_eq!(response.status(), 200);
    assert!(
        axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .is_err()
    );
    released(&env, &job).await;
    assert_eq!(env.state.image_download_gate.available_permits(), 4);
    env.money(&job, PRICE / 2).await;
    env.close().await;
}

#[tokio::test]
async fn archive_requires_published_outputs_but_does_not_require_remaining_balance() {
    let env = Env::new().await;
    sqlx::query("UPDATE users SET balance_micro=$2 WHERE id=$1")
        .bind(env.uid)
        .bind(PRICE / 2)
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state
        .ledger
        .repair(
            env.uid,
            Money::from_micros(PRICE / 2),
            okapi_ledger::Pool::Wallet,
        )
        .await
        .unwrap();
    let job = create(&env, "last balance", 1).await;
    finish(&env, &job).await;
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        0
    );
    assert_eq!(
        env.poll(&job).await["download_url"],
        path(&job, "/download")
    );
    let response = env
        .request(reqwest::Method::GET, &path(&job, "/download"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        unzip(response.bytes().await.unwrap().to_vec()).await["images/0000.png"],
        PNG
    );
    released(&env, &job).await;
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        0
    );
    let failed = create(&env, "no balance", 1).await;
    env.step(&failed).await.unwrap();
    env.step(&failed).await.unwrap();
    assert!(env.poll(&failed).await["download_url"].is_null());
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&failed, "/download"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    env.close().await;
}
