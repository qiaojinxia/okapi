use chrono::{DateTime, Utc};
use fred::{clients::Client, interfaces::HashesInterface};
use okapi_domain::{BillingState, Money, TokenUsage};
use okapi_ledger::{
    BalanceLedger, LimitCaps, Pool, ReserveRequest, SettlementInput,
    holds::{self, Admission, Hold, Reserve, UserGuard},
    pg::UsageDimensions,
};
use okapi_pricing::{PricingSnapshot, RatioFp};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::collections::BTreeMap;
use uuid::Uuid;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const PROOF: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[derive(Clone)]
pub struct Bed {
    pub pg: PgPool,
    pub redis: Client,
    pub ledger: BalanceLedger,
    pub uid: i64,
    pub kid: i64,
    pub pricing: PricingSnapshot,
    pub now: DateTime<Utc>,
}
impl Bed {
    pub async fn new() -> TestResult<Self> {
        Self::with_pool(4).await
    }
    pub async fn with_pool(size: u32) -> TestResult<Self> {
        dotenvy::dotenv().ok();
        let pg = PgPoolOptions::new()
            .max_connections(size)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(&std::env::var("DATABASE_URL")?)
            .await?;
        okapi_store::run_migrations(&pg).await?;
        let suffix = Uuid::new_v4().simple().to_string();
        let uid = okapi_store::provision::create_user(&pg, &format!("hold-{suffix}")).await?;
        let kid =
            okapi_store::provision::create_api_key(&pg, uid, &suffix.repeat(2), "sk-hold-test")
                .await?;
        let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?;
        let ledger = BalanceLedger::new(redis.clone());
        ledger.credit(uid, Money::from_micros(10_000)).await?;
        okapi_ledger::pg::record_credit(
            &pg,
            uid,
            Money::from_micros(10_000),
            "adjust",
            "test:hold",
            json!({}),
        )
        .await?;
        Ok(Self {
            pg,
            redis,
            ledger,
            uid,
            kid,
            now: Utc::now(),
            pricing: PricingSnapshot {
                epoch: 7,
                input_unit: None,
                input_characters: None,
                base_price_per_1m_usd: None,
                mode: "per_call",
                model_ratio: None,
                completion_ratio: None,
                cache_ratio: None,
                cache_write_ratio: None,
                audio_ratio: None,
                audio_completion_ratio: None,
                image_ratio: None,
                modality_ratios: None,
                cache_read_modalities: None,
                cache_write_modalities: None,
                image_completion_tokens: None,
                per_call_price_usd: Some(Money::from_micros(500)),
                service_tier: None,
                tier_ratio: None,
                group: "default".into(),
                group_ratio: RatioFp::ONE,
                user_multiplier: RatioFp::ONE,
                rules: vec![],
                media_units: Some(3),
                final_unit_price_input_per_1m_usd: None,
            },
        })
    }
    pub fn request(&self, id: Uuid, maximum: i64) -> Reserve<'_> {
        Reserve {
            id,
            user_id: self.uid,
            api_key_id: self.kid,
            model: "batch-image",
            request_hash: PROOF,
            maximum: Money::from_micros(maximum),
            pricing: &self.pricing,
        }
    }
    pub async fn reserve(&self, id: Uuid) -> TestResult<Hold> {
        match holds::reserve(&self.pg, &self.ledger, self.request(id, 1_500), self.now).await? {
            Admission::Held { hold, .. } => Ok(hold),
            other => Err(format!("unexpected admission: {other:?}").into()),
        }
    }
    pub fn bill(&self, id: Uuid, amount: i64) -> TestResult<SettlementInput<'static>> {
        let mut pricing = self.pricing.clone();
        pricing.media_units = Some(2);
        Ok(SettlementInput {
            source_window: None,
            dimensions: UsageDimensions::new(
                "batch-image",
                "upstream-image",
                "/v1/batches",
                "/batches",
            ),
            request_id: id,
            log_type: 2,
            user_id: self.uid,
            api_key_id: self.kid,
            group_code: "default",
            model_name: "batch-image",
            channel_id: None,
            channel_key_id: None,
            state: BillingState::Committed,
            usage: TokenUsage::default(),
            amount: Money::from_micros(amount),
            original: Money::from_micros(amount),
            discount: Money::ZERO,
            list_price: Money::from_micros(amount),
            upstream_cost: Some(Money::from_micros(600)),
            pricing_epoch: Some(7),
            pricing_snapshot: Some(serde_json::to_value(pricing)?),
            latency_ms: 1_000,
            ttft_ms: None,
            is_stream: false,
            retry_count: 0,
            failover_count: 0,
            upstream_status: Some(200),
            error_code: None,
            upstream_request_id: Some("batch-result"),
            node: "test-hold",
            sticky_layer: 0,
            client_type: "test",
            client_ip: None,
            delta_micro: 999,
            balance_after: Some(Money::from_micros(999)),
            event_type: "ignored",
            pool: Pool::Wallet,
        })
    }
    pub fn regular(&self, id: Uuid) -> ReserveRequest {
        ReserveRequest {
            user_id: self.uid,
            api_key_id: self.kid,
            request_id: id,
            est: Money::from_micros(1_000),
            est_tokens: 0,
            caps: LimitCaps::default(),
        }
    }
    pub fn balance_key(&self) -> String {
        format!("bal:{{{}}}", self.uid)
    }
    pub fn receipt_key(&self, id: Uuid) -> String {
        format!("hold:{{{}}}:{id}", self.uid)
    }
    pub async fn snapshot(&self) -> TestResult<BTreeMap<String, String>> {
        Ok(self.redis.hgetall(self.balance_key()).await?)
    }
    pub async fn wallet(&self) -> TestResult<i64> {
        Ok(self.ledger.balance(self.uid).await?.as_micros())
    }
    pub async fn sub(&self) -> TestResult<i64> {
        Ok(self.ledger.sub_balance(self.uid).await?.0.as_micros())
    }
    pub async fn row(&self, id: Uuid) -> TestResult<Hold> {
        Ok(sqlx::query_as("SELECT * FROM balance_holds WHERE id=$1")
            .bind(id)
            .fetch_one(&self.pg)
            .await?)
    }
    pub async fn event_total(&self, pool: i16) -> TestResult<i64> {
        Ok(sqlx::query_scalar("SELECT COALESCE(SUM(delta_micro),0)::bigint FROM billing_events WHERE user_id=$1 AND pool=$2")
            .bind(self.uid).bind(pool).fetch_one(&self.pg).await?)
    }
    pub async fn repair(&self) -> TestResult<holds::Repaired> {
        let mut guard = UserGuard::acquire(&self.pg, self.uid).await?;
        let mut totals = [Money::ZERO; 2];
        for (pool, total) in [0_i16, 1].into_iter().zip(totals.iter_mut()) {
            *total = Money::from_micros(sqlx::query_scalar("SELECT COALESCE(SUM(delta_micro),0)::bigint FROM billing_events WHERE user_id=$1 AND pool=$2")
                .bind(self.uid).bind(pool).fetch_one(guard.connection()).await?);
        }
        Ok(guard.repair(&self.ledger, totals[0], totals[1]).await?)
    }
    pub async fn evidence(&self, id: Uuid, amount: i64) -> TestResult {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE request_id=$1")
                .bind(id)
                .fetch_one(&self.pg)
                .await?;
        assert_eq!(count, 1);
        let rec: (i64,i64,i64,Option<i64>) = sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro FROM billing_records WHERE request_id=$1")
            .bind(id).fetch_one(&self.pg).await?;
        assert_eq!(rec, (amount, amount, 0, Some(600)));
        let outbox: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1",
        )
        .bind(id.to_string())
        .fetch_all(&self.pg)
        .await?;
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0]["amount_micro"], amount);
        let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE id=$1")
            .bind(self.kid)
            .fetch_one(&self.pg)
            .await?;
        assert_eq!(used, amount);
        Ok(())
    }
    pub async fn subscription(
        &self,
    ) -> TestResult<(
        okapi_store::subscriptions::SubPlan,
        okapi_store::subscriptions::Subscription,
    )> {
        let code = format!("hold-{}", Uuid::new_v4().simple());
        sqlx::query("INSERT INTO plans(plan_code,display_name,grant_micro,kind,period,duration_days) VALUES($1,'Hold test',2000,1,1,30)")
            .bind(&code).execute(&self.pg).await?;
        let plan = okapi_store::subscriptions::find_sub_plan(&self.pg, &code)
            .await?
            .ok_or("missing plan")?;
        let grant = okapi_ledger::subscriptions::grant(
            &self.pg,
            &self.ledger,
            self.uid,
            &plan,
            "test:hold",
            "test:hold",
        )
        .await?;
        Ok((plan, grant.subscription().clone()))
    }
}
