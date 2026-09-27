//! Admission failures must not call providers, charge funds or leak reservations.
use axum::response::IntoResponse;
use axum::{Router, extract::State, routing::post};
use fred::{
    clients::Client,
    interfaces::{HashesInterface, KeysInterface},
};
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[path = "support/gateway_settlement_atomicity.rs"]
mod settlement;

#[derive(Default)]
struct SettlementGate {
    mode: AtomicU8,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct Bed {
    pg: sqlx::PgPool,
    redis: Client,
    ledger: okapi_ledger::BalanceLedger,
    uid: i64,
    kid: i64,
    model: String,
    token: String,
    address: SocketAddr,
    hits: Arc<AtomicUsize>,
    gate: Arc<SettlementGate>,
    pending: okapi::shutdown::Pending,
}

async fn upstream(
    State((hits, gate)): State<(Arc<AtomicUsize>, Arc<SettlementGate>)>,
) -> axum::response::Response {
    hits.fetch_add(1, Ordering::SeqCst);
    let mode = gate.mode.load(Ordering::SeqCst);
    if mode != 0 {
        gate.entered.notify_one();
        gate.release.notified().await;
    }
    if mode == 2 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(
                json!({"error":{"type":"invalid_request_error","message":"test rejection"}}),
            ),
        )
            .into_response();
    }
    if mode == 3 {
        let chunk = json!({"id":"stream-test","object":"chat.completion.chunk",
            "choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]});
        let usage = json!({"id":"stream-test","object":"chat.completion.chunk",
            "choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}});
        return (
            [("content-type", "text/event-stream")],
            format!("data: {chunk}\n\ndata: {usage}\n\ndata: [DONE]\n\n"),
        )
            .into_response();
    }
    axum::Json(json!({"id":"cmpl","object":"chat.completion",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":10,"completion_tokens":2}}))
    .into_response()
}

async fn serve(app: Router) -> TestResult<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("test server");
    });
    Ok(address)
}

impl Bed {
    async fn new(subscription: bool) -> TestResult<Self> {
        use sha2::{Digest, Sha256};
        dotenvy::dotenv().ok();
        let database = std::env::var("DATABASE_URL")?;
        let redis_url = std::env::var("OKAPI_REDIS_URL")?;
        let pg = okapi_store::connect_pg(&database).await?;
        okapi_store::run_migrations(&pg).await?;
        let tag = Uuid::new_v4().simple().to_string();
        let uid = okapi_store::provision::create_user(&pg, &format!("atomic-{tag}")).await?;
        let token = format!("sk-okapi-atomic-{tag}");
        let kid = okapi_store::provision::create_api_key(
            &pg,
            uid,
            &hex::encode(Sha256::digest(token.as_bytes())),
            "sk-atomic",
        )
        .await?;
        let model = format!("atomic-{tag}");
        okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1").await?;
        let hits = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SettlementGate::default());
        let mock = serve(
            Router::new()
                .route("/v1/chat/completions", post(upstream))
                .route("/v1/embeddings", post(upstream))
                .route("/v1/rerank", post(upstream))
                .with_state((hits.clone(), gate.clone())),
        )
        .await?;
        okapi_store::provision::create_channel(
            &pg,
            &model,
            "openai",
            &format!("http://{mock}/v1"),
            "test",
            &[&model],
            false,
            None,
        )
        .await?;
        let state = gateway::build_state(&database, &redis_url, "atomic-test", None, None).await?;
        let ledger = state.ledger.clone();
        let pending = state.settlements.clone();
        ledger.credit(uid, Money::from_micros(10_000_000)).await?;
        if subscription {
            ledger
                .sub_set(
                    uid,
                    Money::from_micros(10_000),
                    chrono::Utc::now().timestamp() + 3600,
                )
                .await?;
        }
        Ok(Self {
            pg,
            redis: okapi_store::connect_redis(&redis_url).await?,
            ledger,
            uid,
            kid,
            model,
            token,
            address: serve(gateway::router(state)).await?,
            hits,
            gate,
            pending,
        })
    }

    async fn chat(&self) -> TestResult<(u16, Value)> {
        self.generate("/v1/chat/completions").await
    }

    async fn generate(&self, endpoint: &str) -> TestResult<(u16, Value)> {
        let mut body = match endpoint {
            "/v1/embeddings" => json!({"input":"hello"}),
            "/v1/rerank" => json!({"query":"hello", "documents":["hello"]}),
            _ => json!({"max_tokens":16,"messages":[{"role":"user","content":"hello"}]}),
        };
        body["model"] = json!(self.model);
        let response = reqwest::Client::new()
            .post(format!("http://{}{endpoint}", self.address))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        Ok((response.status().as_u16(), response.json().await?))
    }

    fn counter_keys(&self, axis: &str) -> TestResult<Vec<String>> {
        let now = chrono::Utc::now();
        let prefix = format!("rl:{{{}}}:k:{}", self.uid, self.kid);
        Ok(match axis {
            "conc" => vec![format!("conc:{{{}}}:k:{}", self.uid, self.kid)],
            "rpd" => [-1, 0, 1]
                .into_iter()
                .map(|d| {
                    now.checked_add_signed(chrono::TimeDelta::days(d))
                        .map(|t| format!("{prefix}:rpd:{}", t.format("%Y%m%d")))
                        .ok_or_else(|| "test day overflow".into())
                })
                .collect::<TestResult<_>>()?,
            _ => [-1, 0, 1]
                .into_iter()
                .map(|m| format!("{prefix}:{axis}:{}", now.timestamp().div_euclid(60) + m))
                .collect(),
        })
    }

    async fn no_partial_writes(&self) -> TestResult {
        let balance_key = format!("bal:{{{}}}", self.uid);
        let before: std::collections::BTreeMap<String, String> =
            self.redis.hgetall(&balance_key).await?;
        let axes = ["rpm", "tpm", "rpd", "conc"]
            .into_iter()
            .map(|axis| Ok((axis, self.counter_keys(axis)?)))
            .collect::<TestResult<Vec<_>>>()?;
        for (axis, keys) in &axes {
            for key in keys {
                self.redis
                    .hset::<(), _, _>(key, ("invalid", "counter"))
                    .await?;
            }
            let (status, body) = self.chat().await?;
            assert_eq!(status, 500, "{axis}: {body}");
            assert_eq!(body["error"]["code"], "internal_error", "{body}");
            assert_eq!(
                self.hits.load(Ordering::SeqCst),
                0,
                "provider called despite admission failure"
            );
            let after: std::collections::BTreeMap<String, String> =
                self.redis.hgetall(&balance_key).await?;
            assert_eq!(
                after, before,
                "{axis} changed wallet/subscription/reservation"
            );
            for (other, counters) in &axes {
                for key in counters {
                    if other == axis {
                        let value: String = self.redis.hget(key, "invalid").await?;
                        assert_eq!(value, "counter");
                        let ttl: i64 = self.redis.pttl(key).await?;
                        assert_eq!(ttl, -1);
                    } else {
                        let exists: bool = self.redis.exists(key).await?;
                        assert!(!exists, "{axis} partially wrote {key}");
                    }
                }
            }
            self.redis.del::<(), _>(keys.clone()).await?;
        }
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE user_id=$1")
                .bind(self.uid)
                .fetch_one(&self.pg)
                .await?;
        assert_eq!(count, 0, "rejected requests created bills");
        assert!(self.ledger.list_reservations(self.uid).await?.is_empty());
        let (status, body) = self.chat().await?;
        assert_eq!(
            status, 200,
            "valid counters must recover immediately: {body}"
        );
        assert_eq!(self.hits.load(Ordering::SeqCst), 1);
        assert!(self.ledger.list_reservations(self.uid).await?.is_empty());
        let (amount, pool) = self.wait_bill().await?;
        assert!(amount > 0);
        let field = if pool == 1 { "sub" } else { "avail" };
        let available: i64 = self.redis.hget(&balance_key, field).await?;
        assert_eq!(available, before[field].parse::<i64>()? - amount);
        Ok(())
    }

    async fn wait_bill(&self) -> TestResult<(i64, i16)> {
        // Successful HTTP bodies can finish before the settlement task commits.
        for _ in 0..100 {
            let rows: Vec<(i64, i16)> =
                sqlx::query_as("SELECT amount_micro, pool FROM billing_records WHERE user_id=$1")
                    .bind(self.uid)
                    .fetch_all(&self.pg)
                    .await?;
            if let Some(row) = rows.first() {
                assert_eq!(rows.len(), 1, "recovered request must settle exactly once");
                return Ok(*row);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Err("successful request did not settle within five seconds".into())
    }
}

#[tokio::test]
async fn wallet_rejections_do_not_charge_or_call_upstream() -> TestResult {
    Bed::new(false).await?.no_partial_writes().await
}

#[tokio::test]
async fn subscription_rejections_do_not_charge_or_call_upstream() -> TestResult {
    Bed::new(true).await?.no_partial_writes().await
}
