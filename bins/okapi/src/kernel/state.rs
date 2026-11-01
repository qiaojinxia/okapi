//! 各角色共用的状态（IMPLEMENTATION §11.58）：连接（PG / 余额账本 / 调度 Redis / ClickHouse / NATS）、
//! 价簿、节点名、主密钥、settings 缓存、下线跟踪、在途量与看板查询缓存。
//!
//! gateway 的 `AppState` 在它之上挂数据面的分组（上游客户端、路由缓存、健康标记、流控、媒体），
//! 并 `Deref` 到它——`state.pg`、`state.sched` 这类访问在三个角色里照旧可用。只用到这里的函数
//! 收 `&CoreState`，调用方传 `&AppState` 时自动解引用。
use super::error::AppError;
use super::inflight::InFlightGauge;
use super::sched_redis::SchedulerRedis;
use moka::future::Cache;
use okapi_ledger::BalanceLedger;
use okapi_pricing::PriceBookHandle;
use okapi_store::ChClient;
use sqlx::PgPool;
use std::sync::Arc;

/// 控制台看板查询缓存：SQL + 绑定参数 → 结果行。
pub(crate) type QueryCache = Cache<String, Arc<Vec<serde_json::Value>>>;

pub(crate) fn build_query_cache() -> QueryCache {
    Cache::builder()
        .max_capacity(16 * 1024 * 1024)
        .weigher(|key: &String, rows: &Arc<Vec<serde_json::Value>>| {
            let bytes = serde_json::to_vec(rows.as_ref()).map_or(usize::MAX, |v| v.len());
            u32::try_from(key.len().saturating_add(bytes)).unwrap_or(u32::MAX)
        })
        // 运营概览每分钟自动刷新一次：15s 时每次都落空，多个管理员 / 标签页也各算一遍
        .time_to_live(std::time::Duration::from_mins(1))
        .build()
}

#[derive(Clone)]
pub struct CoreState {
    pub pg: PgPool,
    pub ledger: BalanceLedger,
    /// 调度/鉴权 Redis：会话粘性 + 并发信号量 + auth:key 缓存。
    pub sched: SchedulerRedis,
    pub pricebook: Arc<PriceBookHandle>,
    pub node: Arc<str>,
    /// 统计查询（console 门户/管理用）；None = 统计接口 fail-closed 501。
    pub ch: Option<ChClient>,
    /// 控制台看板查询的可选缓存（1 分钟 TTL、按字节限容、同键单飞；读写在 `console::stats_cache`）。
    pub stats_query_cache: QueryCache,
    /// NATS（epoch 广播等）；None = 单机形态走轮询/直连。
    pub nats: Option<async_nats::Client>,
    /// 信封加密主密钥（hex）；None = TOTP 注册不可用（fail-closed）。
    pub master_key: Option<Arc<str>>,
    /// settings 表热路径缓存（60s TTL；用户×模型限流等低频配置）。
    pub settings_cache: Cache<String, Arc<Option<serde_json::Value>>>,
    /// 本进程在途数据面请求数（surge 规则的负载输入，DESIGN §3.4）。
    /// 计数覆盖响应体流完为止；仅在价簿含 surge 规则时才挂计数中间件。
    pub in_flight: InFlightGauge,
    /// 后台结算任务计数（响应先行、结算后台的两条 chat 路径）；优雅下线等它归零（§14.3）。
    pub settlements: crate::shutdown::Pending,
}

impl CoreState {
    /// 是否记录请求来源 IP（`settings.record_ip_log`）。缺省 true——此前一直在记，
    /// 缺省关掉会让存量站点的日志无声地少一列。只认显式的 `false`。
    pub async fn record_ip_log(&self) -> bool {
        self.setting_cached("record_ip_log")
            .await
            .as_ref()
            .as_ref()
            .and_then(serde_json::Value::as_bool)
            != Some(false)
    }

    /// Typed settings read: absence is cacheable; database failure is not absence.
    pub async fn try_setting_cached(
        &self,
        key: &str,
    ) -> Result<Arc<Option<serde_json::Value>>, AppError> {
        if let Some(hit) = self.settings_cache.get(key).await {
            return Ok(hit);
        }
        let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = $1"#, key)
            .fetch_optional(&self.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
        let value = Arc::new(value);
        self.settings_cache
            .insert(key.to_owned(), Arc::clone(&value))
            .await;
        Ok(value)
    }

    /// Best-effort reads are reserved for non-security display/telemetry defaults.
    pub async fn setting_cached(&self, key: &str) -> Arc<Option<serde_json::Value>> {
        match self.try_setting_cached(key).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(key, error_code = error.code, "settings read unavailable");
                Arc::new(None)
            }
        }
    }
}
