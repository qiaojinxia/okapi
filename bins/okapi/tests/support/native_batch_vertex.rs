use super::*;
use aws_lc_rs::encoding::AsDer as _;
use std::collections::BTreeMap;

const PARENT: &str = "projects/batch-project/locations/us-central1";
const BUCKET: &str = "batch-bucket";
#[path = "native_batch_vertex_cases.rs"]
mod cases;
#[path = "native_batch_vertex_cleanup.rs"]
mod cleanup;

#[derive(Clone)]
struct Object {
    bytes: Bytes,
    generation: String,
}
#[derive(Default, PartialEq, Eq)]
enum DeletionState {
    #[default]
    Pending,
    Complete,
    Failed,
    Missing,
}
#[derive(Default)]
struct Deletion {
    requested: bool,
    gone: bool,
    state: DeletionState,
    status_once: Option<u16>,
}
#[derive(Default)]
struct VertexPeer {
    objects: BTreeMap<String, Object>,
    old_versions: BTreeMap<(String, String), Object>,
    cleanup_pages: Option<Vec<Value>>,
    deletion: Deletion,
    object_delete_status_once: Option<u16>,
    job: Value,
    calls: Vec<(Method, String, BTreeMap<String, String>)>,
    tokens: usize,
    creates: usize,
    create_status: u16,
    state: String,
    list_pages: Option<Vec<Value>>,
    list_prefixes: Vec<String>,
    malformed_page: bool,
    truncated: bool,
    upload_status_once: Option<u16>,
}
impl VertexPeer {
    fn output_dir(&self) -> String {
        self.job["outputInfo"]["gcsOutputDirectory"]
            .as_str()
            .unwrap()
            .strip_prefix(&format!("gs://{BUCKET}/"))
            .unwrap()
            .trim_end_matches('/')
            .to_owned()
            + "/"
    }
    fn input_key(&self) -> String {
        self.job["inputConfig"]["gcsSource"]["uris"][0]
            .as_str()
            .unwrap()
            .strip_prefix(&format!("gs://{BUCKET}/"))
            .unwrap()
            .to_owned()
    }
    fn keys(&self) -> Vec<String> {
        std::str::from_utf8(&self.objects[&self.input_key()].bytes)
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["key"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
    fn put(&mut self, key: String, bytes: impl Into<Bytes>) {
        let generation = (self.objects.len() + 1).to_string();
        self.objects.insert(
            key,
            Object {
                bytes: bytes.into(),
                generation,
            },
        );
    }
    fn row(key: &str, success: bool) -> Value {
        if success {
            json!({"key":key,"status":"","response":{"candidates":[{"content":{"parts":[{"inlineData":{"mimeType":"image/png","data":base64::prelude::BASE64_STANDARD.encode(PNG)}}]}}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":12,"totalTokenCount":20}}})
        } else {
            // Official Vertex batch example: nonempty status and an empty response object.
            json!({"key":key,"status":"Bad Request: private-provider-detail","response":{}})
        }
    }
    fn results(&mut self, successes: usize) {
        for (index, key) in self.keys().iter().enumerate() {
            self.put(
                format!("{}predictions_{index:04}.jsonl", self.output_dir()),
                format!("{}\n", Self::row(key, index < successes)),
            );
        }
    }
    fn object_json(&self, key: &str) -> Value {
        json!({"name":key,"bucket":BUCKET,"generation":self.objects[key].generation})
    }
    fn list(&mut self, query: &BTreeMap<String, String>) -> Response {
        if query["versions"] == "true"
            && let Some(pages) = &self.cleanup_pages
        {
            let index = query
                .get("pageToken")
                .map_or(0, |p| p.parse::<usize>().unwrap());
            return axum::Json(pages[index].clone()).into_response();
        }
        let prefix = query["prefix"].clone();
        self.list_prefixes.push(prefix.clone());
        if self.malformed_page && query.contains_key("pageToken") {
            return axum::Json(json!({"items":"broken"})).into_response();
        }
        let index = query
            .get("pageToken")
            .map_or(0, |s| s.parse::<usize>().unwrap());
        let mut entries: Vec<_> = self
            .objects
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .map(|key| self.object_json(key))
            .collect();
        if query["versions"] == "true" {
            entries.extend(self.old_versions.iter().filter(|((key,_),_)|key.starts_with(&prefix)).map(|((key,generation),_)|json!({"name":key,"bucket":BUCKET,"generation":generation})));
        }
        // One object per page ensures the business path really handles pagination.
        let mut value =
            json!({"items":entries.get(index).map(|v| vec![v.clone()]).unwrap_or_default()});
        if index + 1 < entries.len() {
            value["nextPageToken"] = json!((index + 1).to_string());
        }
        axum::Json(value).into_response()
    }
    fn jobs(
        &mut self,
        method: &Method,
        path: &str,
        query: &BTreeMap<String, String>,
        body: Value,
    ) -> Response {
        let operation = format!("{PARENT}/operations/delete-job");
        if path == format!("/v1/{operation}") {
            assert_eq!(method, Method::GET);
            if self.deletion.state == DeletionState::Missing {
                return StatusCode::NOT_FOUND.into_response();
            }
            if self.deletion.state == DeletionState::Failed {
                return axum::Json(json!({"name":operation,"done":true,"error":{"code":7}}))
                    .into_response();
            }
            if self.deletion.state == DeletionState::Complete {
                self.deletion.gone = true;
            }
            return axum::Json(
                json!({"name":operation,"done":self.deletion.state == DeletionState::Complete}),
            )
            .into_response();
        }
        if method == Method::DELETE {
            self.deletion.requested = true;
            if let Some(status) = self.deletion.status_once.take() {
                self.deletion.gone = true;
                return StatusCode::from_u16(status).unwrap().into_response();
            }
            return axum::Json(json!({"name":operation})).into_response();
        }
        if self.deletion.gone {
            return StatusCode::NOT_FOUND.into_response();
        }
        let jobs_path = format!("/v1/{PARENT}/batchPredictionJobs");
        if path == jobs_path && method == Method::POST {
            self.creates += 1;
            assert_eq!(body["instanceConfig"]["keyField"], "key");
            assert_eq!(body["inputConfig"]["instancesFormat"], "jsonl");
            assert_eq!(body["outputConfig"]["predictionsFormat"], "jsonl");
            self.job = body;
            self.job["name"] = json!(format!("{PARENT}/batchPredictionJobs/remote-job"));
            self.job["outputInfo"] = json!({"gcsOutputDirectory":format!("{}prediction-model-time", self.job["outputConfig"]["gcsDestination"]["outputUriPrefix"].as_str().unwrap())});
            if self.create_status != 200 {
                return StatusCode::from_u16(self.create_status)
                    .unwrap()
                    .into_response();
            }
        } else if path == jobs_path {
            assert_eq!(
                query["filter"],
                format!("displayName={}", self.job["displayName"])
            );
            let index = query
                .get("pageToken")
                .map_or(0, |s| s.parse::<usize>().unwrap());
            let value = self.list_pages.as_ref().map_or_else(
                || json!({"batchPredictionJobs":[self.job]}),
                |v| v[index].clone(),
            );
            return axum::Json(value).into_response();
        } else if path.ends_with(":cancel") {
            self.state = "JOB_STATE_CANCELLED".into();
            return axum::Json(json!({})).into_response();
        }
        let mut job = self.job.clone();
        job["state"] = json!(if method == Method::POST {
            "JOB_STATE_PENDING"
        } else {
            &self.state
        });
        axum::Json(job).into_response()
    }
    fn storage(
        &mut self,
        method: &Method,
        path: &str,
        query: &BTreeMap<String, String>,
        bytes: Bytes,
    ) -> Response {
        if path == format!("/upload/storage/v1/b/{BUCKET}/o") {
            assert_eq!(method, Method::POST);
            assert_eq!(query["ifGenerationMatch"], "0");
            let key = &query["name"];
            if self.objects.contains_key(key) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            self.put(key.clone(), bytes);
            if let Some(status) = self.upload_status_once.take() {
                return StatusCode::from_u16(status).unwrap().into_response();
            }
            return axum::Json(self.object_json(key)).into_response();
        }
        if path == format!("/storage/v1/b/{BUCKET}/o") {
            return self.list(query);
        }
        let encoded = path
            .strip_prefix(&format!("/storage/v1/b/{BUCKET}/o/"))
            .unwrap();
        let key = url_decode(encoded);
        if method == Method::DELETE {
            assert!(
                self.deletion.gone || self.job.is_null(),
                "must delete remote job first"
            );
            let generation = &query["generation"];
            let existed = if self
                .objects
                .get(&key)
                .is_some_and(|o| &o.generation == generation)
            {
                self.objects.remove(&key).is_some()
            } else {
                self.old_versions
                    .remove(&(key.clone(), generation.clone()))
                    .is_some()
            };
            if let Some(status) = self.object_delete_status_once.take() {
                return StatusCode::from_u16(status).unwrap().into_response();
            }
            return if existed {
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            };
        }
        let Some(object) = self.objects.get(&key) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if query.get("alt").map(String::as_str) != Some("media") {
            return axum::Json(self.object_json(&key)).into_response();
        }
        assert_eq!(query["generation"], object.generation);
        if self.truncated {
            let stream = futures::stream::iter([
                Ok(object.bytes.clone()),
                Err(std::io::Error::other("injected-truncation")),
            ]);
            return Response::new(Body::from_stream(stream));
        }
        object.bytes.clone().into_response()
    }
}
fn url_decode(value: &str) -> String {
    reqwest::Url::parse(&format!("http://peer/?v={value}"))
        .unwrap()
        .query_pairs()
        .next()
        .unwrap()
        .1
        .into_owned()
}
async fn vertex_peer(State(peer): State<Arc<Mutex<VertexPeer>>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 128 * 1024 * 1024).await.unwrap();
    let mut peer = peer.lock().unwrap();
    let path = parts.uri.path();
    let query: BTreeMap<String, String> = reqwest::Url::parse(&format!(
        "http://peer/?{}",
        parts.uri.query().unwrap_or_default()
    ))
    .unwrap()
    .query_pairs()
    .into_owned()
    .collect();
    peer.calls
        .push((parts.method.clone(), path.into(), query.clone()));
    if path == "/token" {
        peer.tokens += 1;
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="
        ));
        assert_eq!(
            text.split("assertion=").nth(1).unwrap().split('.').count(),
            3
        );
        return axum::Json(json!({"access_token":format!("vertex-token-{}",peer.tokens),"expires_in":1,"token_type":"Bearer"})).into_response();
    }
    assert_eq!(
        parts.headers["authorization"].to_str().unwrap(),
        format!("Bearer vertex-token-{}", peer.tokens)
    );
    assert!(!parts.headers.contains_key("x-goog-api-key"));
    if path.starts_with("/v1/") {
        peer.jobs(
            &parts.method,
            path,
            &query,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    } else {
        peer.storage(&parts.method, path, &query, bytes)
    }
}
struct VertexEnv {
    env: Env,
    peer: Arc<Mutex<VertexPeer>>,
}
impl VertexEnv {
    async fn new() -> Self {
        let env = Env::new().await;
        let peer = Arc::new(Mutex::new(VertexPeer {
            create_status: 200,
            state: "JOB_STATE_SUCCEEDED".into(),
            ..VertexPeer::default()
        }));
        let base = serve(Router::new().fallback(vertex_peer).with_state(peer.clone())).await;
        let key =
            aws_lc_rs::signature::RsaKeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
        let der = key.as_der().unwrap();
        let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
        for line in base64::prelude::BASE64_STANDARD
            .encode(der.as_ref())
            .as_bytes()
            .chunks(64)
        {
            writeln!(pem, "{}", std::str::from_utf8(line).unwrap()).unwrap();
        }
        pem.push_str("-----END PRIVATE KEY-----\n");
        let credential = json!({"client_email":"batch@example.test","private_key":pem,"token_uri":format!("{base}/token")}).to_string();
        sqlx::query("UPDATE channels SET provider='vertex',api_base=$1")
            .bind(format!("{base}/v1/{PARENT}"))
            .execute(&env.state.pg)
            .await
            .unwrap();
        sqlx::query("UPDATE channel_keys SET credential_ciphertext=$1")
            .bind(credential.into_bytes())
            .execute(&env.state.pg)
            .await
            .unwrap();
        let config = json!({"bucket":BUCKET,"api_base":base});
        sqlx::query("INSERT INTO settings(key,value) VALUES('image_batch_gcs',$1)")
            .bind(&config)
            .execute(&env.state.pg)
            .await
            .unwrap();
        env.state
            .settings_cache
            .insert("image_batch_gcs".into(), Arc::new(Some(config)))
            .await;
        Self { env, peer }
    }
    async fn start(&self, count: u32) -> Value {
        let job = self.env.submit(count, "vertex").await;
        self.env.step(&job).await.unwrap();
        self.env.step(&job).await.unwrap();
        assert_eq!(self.env.poll(&job).await["status"], "running");
        job
    }
    async fn private(&self, job: &Value) {
        assert_eq!(self.env.poll(job).await["status"], "collecting");
        assert_eq!(
            self.env
                .request(reqwest::Method::GET, &path(job, "/content/0"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        let records: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE request_id=$1")
                .bind(id(job))
                .fetch_one(&self.env.state.pg)
                .await
                .unwrap();
        assert_eq!(records, 0);
        let (hold_state, maximum): (String, i64) =
            sqlx::query_as("SELECT state,maximum_micro FROM balance_holds WHERE id=$1")
                .bind(id(job))
                .fetch_one(&self.env.state.pg)
                .await
                .unwrap();
        assert_eq!(hold_state, "held");
        assert_eq!(
            self.env
                .state
                .ledger
                .balance(self.env.uid)
                .await
                .unwrap()
                .as_micros(),
            BALANCE - maximum
        );
        assert_eq!(self.peer.lock().unwrap().creates, 1);
    }
}
