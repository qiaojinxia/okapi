use super::sched_redis::SchedulerRedis;
use moka::future::Cache;
use okapi_ledger::BalanceLedger;
use okapi_pricing::PriceBookHandle;
use okapi_providers::{AnthropicUpstream, GeminiUpstream, OpenAiUpstream, PassUpstream};
use okapi_store::ChClient;
use okapi_store::channels::{ChannelCandidate, ResolvedModel};
use sqlx::PgPool;
use std::sync::Arc;

/// 路由/配置缓存失效广播主题（与 `pricing.epoch` 同一条 NATS 通道）。
pub const ROUTING_INVALIDATE_SUBJECT: &str = "routing.invalidate";

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
    /// 本进程在途数据面请求数（surge 规则的负载输入，DESIGN §3.4）。
    /// 计数覆盖响应体流完为止；仅在价簿含 surge 规则时才挂计数中间件。
    pub in_flight: Arc<std::sync::atomic::AtomicI64>,
    /// 上次把在途数写进 Redis 量表的时刻（unix ms）——上报节流用，见 `rule_inputs`。
    pub surge_reported_at: Arc<std::sync::atomic::AtomicI64>,
    /// 渠道相对成本系数缓存（channel_id → 千分比，60s；结算路径折算上游成本用）。
    pub channel_cost_cache: Cache<i64, i64>,
}

impl AppState {
    /// 结算记账统一入口：信号量准入 + 瞬时失败退避重试（200ms/800ms/3.2s），
    /// 三试仍败才 ERROR 留给对账兜底（把"对账修复"从常态变成极端态）。
    pub async fn settle_write(&self, mut input: okapi_ledger::SettlementInput<'_>) {
        // 来源 IP 记录开关（settings.record_ip_log，缺省 true）。收口在这里而非各端点：
        // 七个计费端点全部经 settle_write，关一处即全站不落 IP（PG 列与 CH 列一起）。
        // docs/database.md 早写着「记录与否走 settings.record_ip_log」，但此前全仓无人读它，
        // 站长关不掉——属隐私合规缺口而非功能缺失。
        if input.client_ip.is_some() && !self.record_ip_log().await {
            input.client_ip = None;
        }
        // 上游成本（§11.18）统一在此折算而非各端点：官方价 × 渠道相对成本系数。
        // 只有成功计费且选中了渠道的记录才有成本；失败 / 退款记 None（CH 侧 0）。
        if input.upstream_cost.is_none()
            && input.log_type == 2
            && input.list_price.as_micros() >= 0
            && let Some(channel_id) = input.channel_id
            && let Some(cost_milli) = self.channel_cost_milli(channel_id).await
        {
            input.upstream_cost = Some(okapi_domain::Money::from_micros(
                i64::try_from(
                    i128::from(input.list_price.as_micros()) * i128::from(cost_milli) / 1000,
                )
                .unwrap_or(i64::MAX),
            ));
        }
        // 实时 KPI 挂在这里而非各计费端点：七个端点（chat/embeddings/images/
        // audio/videos/realtime/custom_pass）全部经此收口，加一处即全覆盖，
        // 且本函数已在结算后台任务内，不占客户端可见路径。
        // 只计数据面请求（2 消费 / 5 错误）——退款与管理事件是账务更正，
        // 计进 QPS 会凭空抬高流量读数。
        if matches!(input.log_type, 2 | 5) {
            self.sched
                .kpi_record(
                    input.usage.total_raw(),
                    input.amount.as_micros(),
                    input.log_type == 5,
                )
                .await;
        }
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

    /// 渠道相对成本系数（60s 进程缓存；缺省 1000）。None = 渠道不存在或 PG 读失败
    /// （不猜成本：记录留 None，毛利口径里按"成本未知"处理）。
    pub async fn channel_cost_milli(&self, channel_id: i64) -> Option<i64> {
        if let Some(hit) = self.channel_cost_cache.get(&channel_id).await {
            return Some(hit);
        }
        let value = okapi_store::admin::channel_cost_milli(&self.pg, channel_id)
            .await
            .ok()
            .flatten()?;
        self.channel_cost_cache.insert(channel_id, value).await;
        Some(value)
    }

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
    /// 失效本进程的路由缓存，并广播给其它实例。
    ///
    /// 只失效本地是不够的：管理面有多个副本、数据面更多，管理员在 console-A 上禁掉一条
    /// 出问题的渠道，页面显示已生效，而其余 pod 还会照着各自缓存往那条渠道打——候选集 5s、
    /// 模型 60s、settings 60s。应急操作等一分钟不可接受。定价早就有 `pricing.epoch` 广播，
    /// 这里复用同一条 NATS 通道加一个主题；没有 NATS 的单机部署行为不变（本地失效 + TTL 兜底）。
    ///
    /// 发布是 spawn 出去的：调用点有近二十处且都在同步上下文里，为一次 fire-and-forget
    /// 的通知把它们全改成 async 不值当。丢广播由各自 TTL 兜底，与 epoch 同一套容错口径。
    pub fn invalidate_routing_caches(&self) {
        self.invalidate_routing_caches_local();
        if let Some(nats) = self.nats.clone() {
            // detach 说明：fire-and-forget 广播，失败由 TTL 兜底
            tokio::spawn(async move {
                if let Err(err) = nats.publish(ROUTING_INVALIDATE_SUBJECT, "1".into()).await {
                    tracing::debug!(error = %err, "路由缓存失效广播发送失败（TTL 兜底）");
                }
            });
        }
    }

    /// 只失效本进程（广播的接收端用；避免收到自己的广播再转发出去打环）。
    pub fn invalidate_routing_caches_local(&self) {
        self.model_cache.invalidate_all();
        self.cand_cache.invalidate_all();
        self.channel_cost_cache.invalidate_all();
        self.settings_cache.invalidate_all();
    }
}
