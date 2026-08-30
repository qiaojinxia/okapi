use super::sched_redis::SchedulerRedis;
use moka::future::Cache;
use okapi_ledger::BalanceLedger;
use okapi_pricing::PriceBookHandle;
use okapi_providers::{AnthropicUpstream, GeminiUpstream, OpenAiUpstream, PassUpstream};
use okapi_store::ChClient;
use okapi_store::channels::{ChannelCandidate, ResolvedModel};
use sqlx::PgPool;
use std::sync::Arc;

/// gateway 共享状态：热路径只碰 Redis 与上游（IMPLEMENTATION §2.1）。
/// 鉴权缓存在 Redis（docs/database.md §2.1 auth:key:*，console 跨进程失效）；
/// 下面两个进程内短缓存消除热路径 PG 读，console 写路径主动失效（同进程即时，
/// 多副本靠 TTL 收敛）。
#[derive(Clone)]
pub struct AppState {
    pub pg: PgPool,
    pub ledger: BalanceLedger,
    /// 调度/鉴权 Redis：会话粘性 + 并发信号量 + auth:key 缓存。
    pub sched: SchedulerRedis,
    pub pricebook: Arc<PriceBookHandle>,
    /// 模型解析缓存（canonical + max_output，60s）。
    pub model_cache: Cache<String, Arc<Option<ResolvedModel>>>,
    /// 渠道候选缓存（model|groups → 候选行，5s；渠道状态分钟级变化可容忍）。
    pub cand_cache: Cache<String, Arc<Vec<ChannelCandidate>>>,
    pub upstream: OpenAiUpstream,
    /// Anthropic 原生上游（/v1/messages）。
    pub anthropic: AnthropicUpstream,
    /// Gemini 原生上游（generateContent）。
    pub gemini: GeminiUpstream,
    /// custom_pass 透传传输。
    pub pass: PassUpstream,
    pub node: Arc<str>,
    /// 统计查询（console 门户/管理用）；None = 统计接口 fail-closed 501。
    pub ch: Option<ChClient>,
    /// NATS（epoch 广播等）；None = 单机形态走轮询/直连。
    pub nats: Option<async_nats::Client>,
    /// 信封加密主密钥（hex）；None = TOTP 注册不可用（fail-closed）。
    pub master_key: Option<std::sync::Arc<str>>,
    /// settings 表热路径缓存（60s TTL；用户×模型限流等低频配置）。
    pub settings_cache: moka::future::Cache<String, std::sync::Arc<Option<serde_json::Value>>>,
    /// 结算写入闸（压测驱动修正 #3，docs/perf-report.md）：
    /// 后台结算任务先过信号量再碰 PG，把 pool 竞争者钳制住——
    /// 高 RPS 下等待发生在信号量（无超时）而非 pool acquire（5s 超时丢账）。
    pub settle_gate: std::sync::Arc<tokio::sync::Semaphore>,
}

impl AppState {
    /// 结算记账统一入口：信号量准入 + 瞬时失败退避重试（200ms/800ms/3.2s），
    /// 三试仍败才 ERROR 留给对账兜底（把"对账修复"从常态变成极端态）。
    pub async fn settle_write(&self, input: okapi_ledger::SettlementInput<'_>) {
        // 信号量关闭不可能（进程生命周期内不 close）；acquire 失败按直写降级
        let _permit = self.settle_gate.acquire().await;
        let mut delay = std::time::Duration::from_millis(200);
        for attempt in 0..3u8 {
            match okapi_ledger::record_settlement(&self.pg, input.clone()).await {
                Ok(()) => return,
                Err(err) if attempt < 2 => {
                    tracing::warn!(request_id = %input.request_id, error = %err, attempt, "记账失败，退避重试");
                    tokio::time::sleep(delay).await;
                    delay *= 4;
                }
                Err(err) => {
                    tracing::error!(request_id = %input.request_id, error = %err, "记账三试失败（对账修复）");
                }
            }
        }
    }

    /// settings 点查（60s 进程缓存；miss 回源 PG，读失败按 None 缓存防打穿）。
    pub async fn setting_cached(&self, key: &str) -> std::sync::Arc<Option<serde_json::Value>> {
        if let Some(hit) = self.settings_cache.get(key).await {
            return hit;
        }
        let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = $1"#, key)
            .fetch_optional(&self.pg)
            .await
            .ok()
            .flatten();
        let value = std::sync::Arc::new(value);
        self.settings_cache
            .insert(key.to_owned(), std::sync::Arc::clone(&value))
            .await;
        value
    }

    /// 路由类缓存失效（渠道/模型/别名/分组绑定/settings 变更后调用）。
    pub fn invalidate_routing_caches(&self) {
        self.model_cache.invalidate_all();
        self.cand_cache.invalidate_all();
    }
}
