mod settlement;
use moka::future::Cache;
use okapi_kernel::state::CoreState;
use okapi_providers::{
    AnthropicUpstream, AzureUpstream, BedrockUpstream, OpenAiUpstream, PassUpstream, VertexUpstream,
};
use okapi_store::channels::{ChannelCandidate, ResolvedModel};
pub use settlement::SettlementBatcher;
use std::sync::Arc;

/// 路由/配置缓存失效广播主题（与 `pricing.epoch` 同一条 NATS 通道）。
pub const ROUTING_INVALIDATE_SUBJECT: &str = "routing.invalidate";

/// `routing.invalidate` 广播的作用域：按变更域定向失效，避免改一条无关设置（如公告、存档策略）
/// 或单次毛利熔断把全集群所有网关的 `cand_cache` / `model_cache` / `settings_cache` 一锅端。
///
/// 线协议兼容旧副本：旧版本发 `"1"` 或空串一律解析为 [`Self::All`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheInvalidation {
    /// 全量失效（兼容旧协议 `"1"` / `""`、运维手动清缓存、NATS 断流重连补刷）。
    All,
    /// 渠道 / 渠道 key / 出口代理 / OAuth 凭据 / 冷却或余额状态变更。
    Channels,
    /// 模型 / 别名变更。
    Models,
    /// 渠道池 / 分组绑定 / 池策略变更。
    Pools,
    /// 负毛利熔断状态变更。
    Margin,
    /// 单条 `settings` 键变更（定向失效该键，并在命中 `request_policy` 时同步清策略缓存）。
    Setting(String),
}

impl CacheInvalidation {
    #[must_use]
    pub fn to_wire(&self) -> String {
        match self {
            Self::All => "all".to_owned(),
            Self::Channels => "channels".to_owned(),
            Self::Models => "models".to_owned(),
            Self::Pools => "pools".to_owned(),
            Self::Margin => "margin".to_owned(),
            Self::Setting(key) => format!("setting:{key}"),
        }
    }

    #[must_use]
    pub fn into_wire(self) -> String {
        self.to_wire()
    }

    #[must_use]
    pub fn from_wire(raw: &str) -> Self {
        let trimmed = raw.trim();
        match trimmed {
            "" | "1" | "all" => Self::All,
            "channels" => Self::Channels,
            "models" => Self::Models,
            "pools" => Self::Pools,
            "margin" => Self::Margin,
            other => other
                .strip_prefix("setting:")
                .filter(|k| !k.is_empty())
                .map_or(Self::All, |k| Self::Setting(k.to_owned())),
        }
    }
}

/// 候选筛空时的错误码：被负毛利熔断筛空回 `margin_blocked`——回 `no_available_channel`
/// 会让人以为渠道全挂了（§11.34）。
#[must_use]
pub fn no_candidates_code(margin_removed: usize) -> &'static str {
    if margin_removed > 0 {
        okapi_api::codes::MARGIN_BLOCKED
    } else {
        okapi_api::codes::NO_AVAILABLE_CHANNEL
    }
}

/// [`AppState::retain_margin_ok_ordered`] 筛完之后的结果。
pub struct Retained {
    /// 被负毛利熔断摘掉的条数（见 [`no_candidates_code`]）。
    pub margin_removed: usize,
    /// 被上游额度挡下的账号里最早的窗口重置时刻（Unix 秒）。
    pub next_quota_reset: Option<i64>,
}

/// gateway 共享状态：热路径只碰 Redis 与上游（IMPLEMENTATION §2.1）。
/// 鉴权缓存在 Redis（docs/database.md §2.1 auth:key:*，console 跨进程失效）；
/// 进程内短缓存消除热路径 PG 读，console 写路径主动失效（同进程即时，多副本靠 TTL 收敛）。
///
/// 按用途分组（§11.58）：各角色共用的部分在 `core`（`kernel::state::CoreState`，本类型 `Deref` 到它，
/// `state.pg` / `state.sched` 等照旧可用）；其余是数据面的——上游客户端、路由缓存、健康标记、
/// 准入与结算的流控、媒体端点的闸与对象存储。
#[derive(Clone)]
pub struct AppState {
    pub core: CoreState,
    pub upstreams: Upstreams,
    pub routing: RoutingCaches,
    pub health: HealthMarks,
    pub flow: FlowControl,
    pub media: Media,
    pub data_plane: Option<Arc<dyn crate::bridge::DataPlaneBridge>>,
}

impl std::ops::Deref for AppState {
    type Target = CoreState;
    fn deref(&self) -> &CoreState {
        &self.core
    }
}

impl std::ops::DerefMut for AppState {
    fn deref_mut(&mut self) -> &mut CoreState {
        &mut self.core
    }
}

/// 上游客户端：各家协议的连接池、签名与鉴权，以及订阅凭证刷新的单飞闸。
#[derive(Clone)]
pub struct Upstreams {
    /// OpenAI 方言上游（openai / openai_compat / codex 等；Azure 与它共用连接池）。
    pub openai: OpenAiUpstream,
    pub inference: okapi_providers::inference::Registry,
    /// Azure OpenAI 上游（部署 URL + api-key 头 + api-version；与 `openai` 共用连接池）。
    pub azure: AzureUpstream,
    /// Anthropic 原生上游（/v1/messages）。
    pub anthropic: AnthropicUpstream,
    /// Amazon Bedrock（InvokeModel，Anthropic 方言 + SigV4 / API key；§11.35）。
    pub bedrock: BedrockUpstream,
    /// Google Vertex AI（服务账号 OAuth；Claude rawPredict / Gemini generateContent；§11.35）。
    pub vertex: VertexUpstream,
    /// custom_pass 透传传输。
    pub pass: PassUpstream,
    /// 订阅 OAuth 凭证刷新的进程内单飞闸（§11.38，四步锁第一步）。
    pub refresh_gate: Arc<super::oauth_cred::RefreshGate>,
}

/// 选路用的进程内短缓存（渠道 / 模型 / 别名 / 分组绑定变更后由 `invalidate_routing_caches` 一并失效）。
#[derive(Clone)]
pub struct RoutingCaches {
    /// 模型解析缓存（canonical + max_output，60s）。
    pub model_cache: Cache<String, Arc<Option<ResolvedModel>>>,
    /// 渠道候选缓存（model|groups → 候选行，5s；渠道状态分钟级变化可容忍）。
    pub cand_cache: Cache<String, Arc<Vec<ChannelCandidate>>>,
    /// 渠道相对成本系数缓存（channel_id → 千分比，60s；结算路径折算上游成本用）。
    pub channel_cost_cache: Cache<i64, i64>,
    pub channel_control_cache: Cache<i64, Arc<super::account_control::policy::Policy>>,
    /// 负毛利熔断表进程缓存（§11.34）：单键 → 此刻生效的 `<group>|<channel_id>` 集合，10s；
    /// 一次 HGETALL 供所有请求共用，HASH 为空时只是一次空往返。
    pub margin_cache: Cache<u8, Arc<std::collections::HashSet<String>>>,
    /// 请求策略（`settings.request_policy`）的解析结果，10s；写设置时随路由缓存一起失效。
    pub request_policy: Cache<u8, Arc<okapi_kernel::policies::RequestPolicy>>,
    /// 渠道池策略文档缓存（pool_code → StoredPoolPolicy，5s；改池策略走 `routing.invalidate`，不再全站 auth_flush）。
    pub pool_policies: Cache<String, Arc<okapi_store::pool_policy::StoredPoolPolicy>>,
    /// 启用中模型目录缓存（`GET /v1/models`、`GET /v1beta/models` 与批量模型清单共用，60s；由 `Models` 作用域即时失效）。
    pub active_models_cache: Cache<u8, Arc<Vec<okapi_store::pricing::ActiveModelCard>>>,
}

/// 渠道 key 与出口代理的健康标记：本地标记加 5s 集群脏集缓存，成功时才回 PG 清零。
#[derive(Clone)]
pub struct HealthMarks {
    /// 本进程登记过瞬态失败的渠道 key（24h）：该 key 下一次成功才回 PG 清零连续失败计数
    /// （`key_health`），健康 key 的成功请求零额外 IO。
    pub key_failures: Cache<i64, ()>,
    /// 本进程登记过连接失败的出口代理（24h，§11.41）：同上，下一次经它成功才回 PG 清零。
    pub egress_failures: Cache<i64, ()>,
    /// 一分钟内后台核实过的出口代理（§11.41）：分不清是代理还是目标的连接失败，同一代理只核实一次。
    pub egress_verifying: Cache<i64, ()>,
    /// 跨副本共享的失败渠道 key 集合缓存（5s，源自 Redis `health:dirty:ck`）：
    /// 副本 A 登记失败后，副本 B 成功请求也可在零额外热路径 IO 下感知并清零 PG 计数。
    pub dirty_keys_cache: Cache<u8, Arc<std::collections::HashSet<i64>>>,
    /// 跨副本共享的失败出口代理集合缓存（5s，源自 Redis `health:dirty:egress`）。
    pub dirty_egress_cache: Cache<u8, Arc<std::collections::HashSet<i64>>>,
}

/// 准入与结算的流控：按用户排队、结算写入闸与积压泄压。
#[derive(Clone)]
pub struct FlowControl {
    /// 同一用户的预扣先在进程内排队，再拿 PG 连接与用户锁（`user_turns`）。
    pub admission_turns: super::user_turns::UserTurns,
    /// 同一用户的结算另排一队：等全局结算闸的结算不能挡住该用户新请求的准入。
    pub settlement_turns: super::user_turns::UserTurns,
    /// 结算写入闸（压测驱动修正 #3，docs/perf-report.md）：
    /// 后台结算任务先过信号量再碰 PG，把 pool 竞争者钳制住——
    /// 高 RPS 下等待发生在信号量（无超时）而非 pool acquire（5s 超时丢账）。
    pub settle_gate: Arc<tokio::sync::Semaphore>,
    /// 正在 `settle_write` 里排队或写 PG 的结算数（与 `settlements` 不同：不含仍在流式
    /// 泵送的请求）。PG 记账速率是它的唯一出口，进量持续高于出口就无界增长——
    /// 超过 `settle_backlog_max` 时数据面泄压（§12.2）。
    pub settle_backlog: Arc<std::sync::atomic::AtomicUsize>,
    /// 结算积压上界（`OKAPI_SETTLE_BACKLOG_MAX`，缺省 20000；0 = 不设限，压测网关自身开销用）。
    pub settle_backlog_max: usize,
    /// 当前是否处于泄压态——只在状态翻转时各记一条 WARN，不按请求刷日志。
    pub settle_shedding: Arc<std::sync::atomic::AtomicBool>,
    /// 普通成功结算的进程内微批组提交器（每批最多 64 笔 / 2ms 聚合窗口，显著降低 PG WAL 与渠道热点行锁竞争）。
    pub settle_batcher: Arc<settlement::SettlementBatcher>,
    /// Unit tests must not expose their settlement journal to a live worker.
    #[cfg(test)]
    pub(crate) settlement_journal_tag: Arc<str>,
}

/// 媒体端点：图片下载与批量提交的并发闸、私有对象存储。
#[derive(Clone)]
pub struct Media {
    /// Image download buffers stay bounded until each response body finishes or is dropped.
    pub image_download_gate: Arc<tokio::sync::Semaphore>,
    pub batch_submit_gate: Arc<tokio::sync::Semaphore>,
    /// Private image object stores; credentials stay in process configuration.
    pub image_storage: Arc<super::images::tasks::objects::Storage>,
}

/// `settle_backlog` 的进出计数：Drop 归还，覆盖提前 return 与任务被取消的路径。
struct BacklogGuard(Arc<std::sync::atomic::AtomicUsize>);

impl BacklogGuard {
    fn enter(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(Arc::clone(counter))
    }
}

impl Drop for BacklogGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl AppState {
    /// 数据面准入（IMPLEMENTATION §12.2）：后台结算积压超过上界即 503 `overloaded`。
    /// 放在鉴权之前——被拒的请求不预扣、不碰上游，连 Redis 鉴权往返也省掉。
    /// 带滞回：超过上界进入泄压，回落到上界的 3/4 以下才恢复——不然积压在阈值附近
    /// 每落几笔就放几笔，状态每秒翻转十来次，翻转日志本身就成了刷屏。
    pub fn check_settle_backlog(&self) -> Result<(), okapi_kernel::error::AppError> {
        use std::sync::atomic::Ordering;
        let max = self.flow.settle_backlog_max;
        if max == 0 {
            return Ok(());
        }
        let pending = self.flow.settle_backlog.load(Ordering::Relaxed);
        let was_shedding = self.flow.settle_shedding.load(Ordering::Relaxed);
        let shedding = if was_shedding {
            pending > max - max / 4
        } else {
            pending > max
        };
        if shedding != was_shedding
            && self
                .flow
                .settle_shedding
                .compare_exchange(was_shedding, shedding, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            if shedding {
                tracing::warn!(
                    pending,
                    max,
                    "结算积压超过上界，数据面开始泄压（503 overloaded）"
                );
            } else {
                tracing::warn!(pending, max, "结算积压回落，数据面恢复");
            }
        }
        if shedding {
            return Err(okapi_kernel::error::AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                okapi_api::codes::OVERLOADED,
            )
            .with_param(pending.to_string()));
        }
        Ok(())
    }

    /// 预扣准入：无限额 key 优先走零 PG Redis 快路径（无进程内锁排队、无 PG 连接占用）；
    /// 快路径租约缺失/过期或限额 key 再在进程内按用户排队，轮到了才拿 PG 连接与用户锁（见 `user_turns`）。
    /// 所有计费端点都经这里，单个高并发用户不能把全站连接池耗在等锁上。
    pub async fn reserve_for_key(
        &self,
        limited: bool,
        request: okapi_ledger::ReserveRequest,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<okapi_ledger::ReserveOutcome, okapi_ledger::LedgerError> {
        let outcome =
            if !limited && let Some(outcome) = self.ledger.try_reserve_fast(request, now).await? {
                outcome
            } else {
                let _turn = self
                    .flow
                    .admission_turns
                    .wait(request.user_id, super::user_turns::ADMISSION_WAIT)
                    .await?;
                self.ledger
                    .reserve_for_key(&self.pg, limited, request, now)
                    .await?
            };
        // 热账本丢失自愈：余额不足且这个用户的余额键根本不存在，说明 Redis 丢了数据
        // （重启未持久化 / 淘汰 / FLUSHDB），不是用户没钱。按 PG 账本重建一次再准入，
        // 不必等每日全量对账和人工修复。重建总会写回余额字段，同一用户最多触发一次。
        if matches!(outcome, okapi_ledger::ReserveOutcome::Insufficient { .. })
            && matches!(
                self.ledger.balance_present(request.user_id).await,
                Ok(false)
            )
        {
            tracing::warn!(
                user_id = request.user_id,
                "hot balance missing; rebuilding from the ledger"
            );
            match okapi_kernel::repair::repair_balance(&self.pg, &self.ledger, request.user_id)
                .await
            {
                Ok(Some(_)) => {
                    return self
                        .ledger
                        .reserve_for_key(&self.pg, limited, request, now)
                        .await;
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(user_id = request.user_id, %error, "hot balance rebuild failed");
                }
            }
        }
        Ok(outcome)
    }

    /// 结算记账统一入口：信号量准入 + 瞬时失败退避重试（200ms/800ms/3.2s），
    /// PG 写失败保留 Redis 重试日志，worker 幂等补写；两个后端均故障时保留任务。
    pub fn settle_write<'a>(
        &'a self,
        input: okapi_ledger::SettlementInput<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(self.settle_write_inner(input))
    }

    async fn settle_write_inner(&self, mut input: okapi_ledger::SettlementInput<'_>) {
        let backlog = BacklogGuard::enter(&self.flow.settle_backlog);
        self.prepare_settlement(&mut input).await;
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
        let permit = self.flow.settle_gate.acquire().await;
        let journaled = super::settlement_retry::save_in_flight(self, &input)
            .await
            .is_ok();
        let mut delay = std::time::Duration::from_millis(200);
        for attempt in 0..3u8 {
            match okapi_ledger::record_settlement(&self.pg, input.clone()).await {
                Ok(()) => {
                    let _ = super::settlement_retry::remove(self, input.request_id).await;
                    return;
                }
                Err(err) if attempt < 2 => {
                    tracing::warn!(request_id = %input.request_id, error = %err, attempt, "记账失败，退避重试");
                    tokio::time::sleep(delay).await;
                    delay *= 4;
                }
                Err(err) => {
                    tracing::error!(request_id = %input.request_id, error = %err, "settlement deferred to retry journal");
                }
            }
        }
        drop(permit);
        if journaled {
            let _ = super::settlement_retry::due_now(self, input.request_id).await;
        } else {
            let state = self.clone();
            let owned = okapi_ledger::pg::OwnedSettlementInput::from(input);
            self.settlements.spawn(async move {
                let _backlog = backlog;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let persisted = {
                        let _permit = state.flow.settle_gate.acquire().await;
                        okapi_ledger::record_settlement(&state.pg, owned.as_input())
                            .await
                            .is_ok()
                    };
                    if persisted
                        || super::settlement_retry::save(&state, &owned.as_input())
                            .await
                            .is_ok()
                    {
                        return;
                    }
                }
            });
        }
    }

    /// 启用中模型目录卡片（60s 进程缓存，同键 singleflight 防击穿，由 `CacheInvalidation::Models` 跨副本即时失效）。
    pub async fn active_model_cards(
        &self,
    ) -> Result<Arc<Vec<okapi_store::pricing::ActiveModelCard>>, okapi_kernel::error::AppError> {
        if let Some(hit) = self.routing.active_models_cache.get(&0).await {
            return Ok(hit);
        }
        Box::pin(self.routing.active_models_cache.try_get_with(0, async {
            okapi_store::pricing::list_active_model_cards(&self.pg)
                .await
                .map(Arc::new)
                .map_err(okapi_kernel::error::AppError::from)
        }))
        .await
        .map_err(|err| (*err).clone())
    }

    /// 渠道相对成本系数（60s 进程缓存，同键 singleflight 防击穿；缺省 1000）。
    /// None = 渠道不存在或 PG 读失败（不缓存，不猜成本：记录留 None，毛利口径里按"成本未知"处理）。
    pub async fn channel_cost_milli(&self, channel_id: i64) -> Option<i64> {
        if let Some(hit) = self.routing.channel_cost_cache.get(&channel_id).await {
            return Some(hit);
        }
        Box::pin(
            self.routing
                .channel_cost_cache
                .optionally_get_with(channel_id, async {
                    okapi_store::admin::channel_cost_milli(&self.pg, channel_id)
                        .await
                        .ok()
                        .flatten()
                }),
        )
        .await
    }

    /// 此刻生效的请求策略（10s 进程缓存，同键 singleflight）：没配 = 缺省（现行行为）；
    /// 配坏了 = 缺省并留日志，不让一个写坏的值把重试行为悄悄变掉。
    pub async fn request_policy(&self) -> Arc<okapi_kernel::policies::RequestPolicy> {
        use okapi_kernel::policies::{REQUEST_POLICY_SETTING, RequestPolicy};
        if let Some(hit) = self.routing.request_policy.get(&0).await {
            return hit;
        }
        Box::pin(self.routing.request_policy.get_with(0, async {
            let setting = self.setting_cached(REQUEST_POLICY_SETTING).await;
            let policy = match setting.as_ref() {
                None => RequestPolicy::default(),
                Some(value) => RequestPolicy::parse(value).unwrap_or_else(|| {
                    tracing::warn!("invalid request_policy setting; defaults in effect");
                    RequestPolicy::default()
                }),
            };
            Arc::new(policy)
        }))
        .await
    }

    /// 此刻生效的负毛利熔断对（`<group>|<channel_id>`，10s 进程缓存，同键 singleflight；§11.34）。
    /// Redis 故障按空集处理（fail-open：熔断是止损手段，不该因缓存抖动把请求拒掉）。
    pub async fn margin_blocked_fields(&self) -> Arc<std::collections::HashSet<String>> {
        if let Some(hit) = self.routing.margin_cache.get(&0).await {
            return hit;
        }
        Box::pin(self.routing.margin_cache.get_with(0, async {
            let now = chrono::Utc::now().timestamp();
            let set: std::collections::HashSet<String> = self
                .sched
                .margin_blocks()
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, e)| e.blocks_at(now))
                .map(|(k, _)| k)
                .collect();
            Arc::new(set)
        }))
        .await
    }

    /// 把该分组当前被熔断的渠道从候选里摘掉；返回摘掉的条数，供调用方区分
    /// "被熔断筛空"（`margin_blocked`）与"本来就没候选"（`no_available_channel`）。
    pub async fn retain_margin_ok(
        &self,
        group: &str,
        candidates: &mut impl super::scheduler::CandidateSet,
    ) -> usize {
        self.retain_margin_ok_ordered(
            group,
            candidates,
            super::account_control::QuotaOrder::HeadroomTiers,
            None,
        )
        .await
        .margin_removed
    }

    /// 同 [`Self::retain_margin_ok`]，额度观测按池策略参与层内排序（见 `QuotaOrder`）；
    /// `shape` 给出请求的形状时，额度预判据此评估每个订阅候选（见 `account_control::forecast`）。
    pub async fn retain_margin_ok_ordered(
        &self,
        group: &str,
        candidates: &mut impl super::scheduler::CandidateSet,
        order: super::account_control::QuotaOrder,
        shape: Option<&super::account_control::forecast::RequestShape>,
    ) -> Retained {
        let next_quota_reset =
            super::account_control::retain_available(self, candidates, order, shape).await;
        let blocked = self.margin_blocked_fields().await;
        if blocked.is_empty() {
            return Retained {
                margin_removed: 0,
                next_quota_reset,
            };
        }
        let before = candidates.len();
        candidates.retain(|c| !blocked.contains(&okapi_kernel::margin::field(group, c.channel_id)));
        Retained {
            margin_removed: before - candidates.len(),
            next_quota_reset,
        }
    }

    /// 按作用域广播路由/配置缓存失效。
    /// 非 `Setting` 作用域均只需同步清理本地缓存并 fire-and-forget 广播给其它副本；
    /// 单条 `settings` 键请调用 [`Self::invalidate_setting_cache`]。
    pub fn invalidate_cache(&self, scope: CacheInvalidation) {
        self.invalidate_cache_local_sync(&scope);
        self.publish_invalidation(scope);
    }

    /// 定向失效单条 `settings` 键缓存（命中 `request_policy` 时一并清策略缓存），
    /// 并经 `routing.invalidate` 广播 `"setting:<key>"` 到集群内其它副本。
    pub async fn invalidate_setting_cache(&self, key: &str) {
        let scope = CacheInvalidation::Setting(key.to_owned());
        self.invalidate_cache_local(&scope).await;
        self.publish_invalidation(scope);
    }

    /// 全量路由缓存失效（兼容旧调用点、运维手动清缓存、回滚定价等跨域变更）。
    /// 失效本进程全部路由/设置缓存，并广播给其它实例。
    pub fn invalidate_routing_caches(&self) {
        self.invalidate_cache(CacheInvalidation::All);
    }

    fn publish_invalidation(&self, scope: CacheInvalidation) {
        if let Some(nats) = self.nats.clone() {
            okapi_kernel::metrics::global().record_routing_invalidation(true);
            let payload = scope.into_wire();
            // detach 说明：fire-and-forget 广播，失败由 TTL 兜底
            tokio::spawn(async move {
                if let Err(err) = nats
                    .publish(ROUTING_INVALIDATE_SUBJECT, payload.into())
                    .await
                {
                    tracing::debug!(error = %err, "路由缓存失效广播发送失败（TTL 兜底）");
                }
            });
        }
    }

    /// 为共享同一进程运行时（`okapi all`）衍生一份隔离的 PostgreSQL 连接池、结算计数与流控闸；
    /// PriceBook、路由缓存、settings 缓存、健康标记、上游连接池、OAuth 单飞闸、Redis/NATS/CH 共用同一份实例。
    #[must_use]
    pub fn with_isolated_pg(&self, pg: sqlx::PgPool, pg_pool_max: u32) -> Self {
        Self {
            core: CoreState {
                pg,
                ledger: self.core.ledger.clone(),
                sched: self.core.sched.clone(),
                pricebook: Arc::clone(&self.core.pricebook),
                node: Arc::clone(&self.core.node),
                ch: self.core.ch.clone(),
                stats_query_cache: self.core.stats_query_cache.clone(),
                nats: self.core.nats.clone(),
                master_key: self.core.master_key.clone(),
                settings_cache: self.core.settings_cache.clone(),
                in_flight: self.core.in_flight.clone(),
                settlements: okapi_kernel::shutdown::Pending::default(),
            },
            upstreams: self.upstreams.clone(),
            routing: self.routing.clone(),
            health: self.health.clone(),
            flow: super::flow_control_for_pool(pg_pool_max),
            media: self.media.clone(),
            data_plane: self.data_plane.clone(),
        }
    }

    /// 同一进程里共用 `self` 这份 PG 池的另一个角色（`okapi all` 的控制台与 worker）：连同结算 / 准入流控
    /// 一起共用——两份流控各把闸开到池的一半，合起来就占满整个池，鉴权回源、维护任务取不到连接；
    /// 在途结算计数各自一份，各角色下线时只等自己的。
    #[must_use]
    pub fn sharing_pg(&self) -> Self {
        let mut sibling = self.clone();
        sibling.core.settlements = okapi_kernel::shutdown::Pending::default();
        sibling
    }

    /// Attach the in-process data-plane bridge used by control-plane relay endpoints.
    #[must_use]
    pub fn with_data_plane(mut self, bridge: Arc<dyn crate::bridge::DataPlaneBridge>) -> Self {
        self.data_plane = Some(bridge);
        self
    }

    /// Borrow the in-process data-plane bridge, failing closed with 503 if unattached.
    pub fn data_plane(
        &self,
    ) -> Result<&dyn crate::bridge::DataPlaneBridge, okapi_kernel::error::AppError> {
        self.data_plane.as_deref().ok_or_else(|| {
            okapi_kernel::error::AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                okapi_api::codes::OVERLOADED,
            )
            .with_param("data_plane_unavailable")
        })
    }

    /// 读取渠道池生效策略快照（5s 进程缓存，同键 singleflight 防击穿，由 `routing.invalidate` 跨副本即时失效；
    /// 查不到行或瞬态 DB 异常时安全回退默认策略且不缓存错误，不阻断热路径）。
    pub async fn load_pool_policy(
        &self,
        pool_code: &str,
    ) -> Arc<okapi_store::pool_policy::StoredPoolPolicy> {
        if let Some(hit) = self.routing.pool_policies.get(pool_code).await {
            return hit;
        }
        match Box::pin(
            self.routing
                .pool_policies
                .try_get_with(pool_code.to_owned(), async {
                    let loaded =
                        match okapi_store::pool_policy::load_pool_policy(&self.pg, pool_code)
                            .await?
                        {
                            Some(policy) => policy,
                            None => okapi_store::pool_policy::StoredPoolPolicy::fallback_default(
                                pool_code,
                            ),
                        };
                    Ok::<_, okapi_store::StoreError>(Arc::new(loaded))
                }),
        )
        .await
        {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(
                    pool_code,
                    %error,
                    "load_pool_policy failed; falling back to default policy"
                );
                Arc::new(
                    okapi_store::pool_policy::StoredPoolPolicy::fallback_default(pool_code),
                )
            }
        }
    }

    /// 按作用域只失效本进程缓存（广播接收端调用，避免收到自己的广播再转发打环）。
    pub async fn invalidate_cache_local(&self, scope: &CacheInvalidation) {
        match scope {
            CacheInvalidation::Setting(key) => {
                okapi_kernel::metrics::global().record_routing_invalidation(false);
                self.settings_cache.invalidate(key).await;
                if key == okapi_kernel::policies::REQUEST_POLICY_SETTING {
                    self.routing.request_policy.invalidate_all();
                }
            }
            other => self.invalidate_cache_local_sync(other),
        }
    }

    fn invalidate_cache_local_sync(&self, scope: &CacheInvalidation) {
        okapi_kernel::metrics::global().record_routing_invalidation(false);
        match scope {
            CacheInvalidation::All => {
                self.routing.model_cache.invalidate_all();
                self.routing.active_models_cache.invalidate_all();
                self.routing.cand_cache.invalidate_all();
                self.routing.channel_cost_cache.invalidate_all();
                self.routing.channel_control_cache.invalidate_all();
                self.routing.margin_cache.invalidate_all();
                self.routing.request_policy.invalidate_all();
                self.routing.pool_policies.invalidate_all();
                self.settings_cache.invalidate_all();
            }
            CacheInvalidation::Channels => {
                self.routing.cand_cache.invalidate_all();
                self.routing.channel_cost_cache.invalidate_all();
                self.routing.channel_control_cache.invalidate_all();
            }
            CacheInvalidation::Models => {
                self.routing.model_cache.invalidate_all();
                self.routing.active_models_cache.invalidate_all();
                self.routing.cand_cache.invalidate_all();
            }
            CacheInvalidation::Pools => {
                self.routing.cand_cache.invalidate_all();
                self.routing.pool_policies.invalidate_all();
            }
            CacheInvalidation::Margin => {
                self.routing.margin_cache.invalidate_all();
            }
            CacheInvalidation::Setting(key) => {
                self.settings_cache.invalidate_all();
                if key == okapi_kernel::policies::REQUEST_POLICY_SETTING {
                    self.routing.request_policy.invalidate_all();
                }
            }
        }
    }

    /// 只失效本进程全部路由缓存（兼容测试与 NATS 断流重连补刷）。
    pub fn invalidate_routing_caches_local(&self) {
        self.invalidate_cache_local_sync(&CacheInvalidation::All);
    }

    /// 采集当前角色的实时运行时水位，供 Prometheus `/metrics` 端点导出。
    #[must_use]
    pub fn runtime_metrics_snapshot(
        &self,
        role: &'static str,
    ) -> okapi_kernel::metrics::RuntimeSnapshot {
        use std::sync::atomic::Ordering;
        okapi_kernel::metrics::RuntimeSnapshot {
            role,
            pg_pool_size: self.pg.size(),
            pg_pool_idle: self.pg.num_idle(),
            pricebook_epoch: self.pricebook.epoch(),
            settle_backlog: self.flow.settle_backlog.load(Ordering::Relaxed),
            settle_in_flight: self.core.settlements.in_flight(),
            settle_shedding: self.flow.settle_shedding.load(Ordering::Relaxed),
            cache_models: self.routing.model_cache.entry_count(),
            cache_candidates: self.routing.cand_cache.entry_count(),
            cache_channel_cost: self.routing.channel_cost_cache.entry_count(),
            cache_channel_control: self.routing.channel_control_cache.entry_count(),
            cache_pool_policies: self.routing.pool_policies.entry_count(),
            cache_settings: self.settings_cache.entry_count(),
        }
    }

    /// 启动本进程的运维指标 Redis 心跳（每 10 秒上报一次结构化快照，供多副本控制台聚合）。
    pub fn spawn_metrics_publisher(&self, role: &'static str) {
        let state = self.clone();
        okapi_kernel::ops::metrics_sync::spawn_publisher(
            self.sched.client().clone(),
            (*self.node).to_owned(),
            role,
            move || state.runtime_metrics_snapshot(role),
        );
    }

    /// 标准 Prometheus `/metrics` 处理器（gateway 与 console 共用）。
    pub fn metrics_response(
        &self,
        role: &'static str,
        headers: &axum::http::HeaderMap,
    ) -> axum::response::Response {
        use axum::response::IntoResponse as _;
        if let Err(status) = okapi_kernel::metrics::check_scrape_access(headers) {
            return status.into_response();
        }
        let body =
            okapi_kernel::metrics::global().render_prometheus(&self.runtime_metrics_snapshot(role));
        (
            [(
                axum::http::header::CONTENT_TYPE,
                okapi_kernel::metrics::PROMETHEUS_CONTENT_TYPE,
            )],
            body,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app_state() -> AppState {
        let pg = sqlx::PgPool::connect_lazy("postgres://127.0.0.1:5432/okapi_gw")
            .expect("lazy pool parses");
        let redis = fred::prelude::Builder::from_config(fred::prelude::Config::default())
            .build()
            .expect("redis client builds without connect");
        let sched = okapi_kernel::sched_redis::SchedulerRedis::new(redis.clone());
        let ledger = okapi_ledger::BalanceLedger::new(redis);
        let in_flight = okapi_kernel::inflight::InFlightGauge::new(sched.clone(), "test-node");
        let resources = okapi_providers::inference::Resources::new().expect("resources");
        let inference =
            okapi_providers::inference::Registry::builtin(&resources).expect("inference registry");
        let openai = resources.openai.clone();

        AppState {
            core: CoreState {
                pg,
                ledger,
                sched,
                pricebook: Arc::new(okapi_pricing::PriceBookHandle::new(
                    okapi_pricing::book::compile(okapi_pricing::PriceBookSource {
                        epoch: 0,
                        models: Vec::new(),
                        groups: Vec::new(),
                        overrides: Vec::new(),
                        rules: Vec::new(),
                    })
                    .expect("empty pricebook compiles"),
                )),
                node: Arc::from("test-node"),
                ch: None,
                stats_query_cache: okapi_kernel::state::build_query_cache(),
                nats: None,
                master_key: None,
                settings_cache: Cache::builder().max_capacity(16).build(),
                in_flight,
                settlements: okapi_kernel::shutdown::Pending::default(),
            },
            upstreams: Upstreams {
                azure: okapi_providers::AzureUpstream::new(openai.clone()),
                openai,
                inference,
                anthropic: resources.anthropic,
                bedrock: resources.bedrock,
                vertex: resources.vertex,
                pass: okapi_providers::PassUpstream::new().expect("pass client"),
                refresh_gate: Arc::default(),
            },
            routing: RoutingCaches {
                model_cache: Cache::builder().max_capacity(16).build(),
                cand_cache: Cache::builder().max_capacity(16).build(),
                channel_cost_cache: Cache::builder().max_capacity(16).build(),
                channel_control_cache: Cache::builder().max_capacity(16).build(),
                margin_cache: Cache::builder().max_capacity(1).build(),
                request_policy: Cache::builder().max_capacity(1).build(),
                pool_policies: Cache::builder().max_capacity(16).build(),
                active_models_cache: Cache::builder().max_capacity(1).build(),
            },
            health: HealthMarks {
                key_failures: Cache::builder().max_capacity(16).build(),
                egress_failures: Cache::builder().max_capacity(16).build(),
                egress_verifying: Cache::builder().max_capacity(16).build(),
                dirty_keys_cache: Cache::builder().max_capacity(1).build(),
                dirty_egress_cache: Cache::builder().max_capacity(1).build(),
            },
            flow: crate::flow_control_for_pool(16),
            media: Media {
                image_download_gate: Arc::new(tokio::sync::Semaphore::new(4)),
                batch_submit_gate: Arc::new(tokio::sync::Semaphore::new(2)),
                image_storage: Arc::new(
                    crate::images::tasks::objects::Storage::from_env().expect("storage"),
                ),
            },
            data_plane: None,
        }
    }

    #[tokio::test]
    async fn isolated_pg_shares_runtime_caches_and_isolates_pool_flow() {
        let bg_pg = sqlx::PgPool::connect_lazy("postgres://127.0.0.1:5432/okapi_bg")
            .expect("lazy pool parses");
        let gw = test_app_state();

        let bg = gw.with_isolated_pg(bg_pg, 4);
        assert!(Arc::ptr_eq(&gw.core.pricebook, &bg.core.pricebook));
        assert!(Arc::ptr_eq(
            &gw.upstreams.refresh_gate,
            &bg.upstreams.refresh_gate
        ));
        assert_eq!(gw.flow.settle_gate.available_permits(), 8);
        assert_eq!(bg.flow.settle_gate.available_permits(), 2);
        // 同一后台池上的第二个角色：同一道闸（合起来仍是池的一半），在途结算各算各的
        let sibling = bg.sharing_pg();
        assert!(Arc::ptr_eq(&bg.flow.settle_gate, &sibling.flow.settle_gate));

        gw.routing
            .model_cache
            .insert("gpt-5".into(), Arc::new(None))
            .await;
        gw.routing
            .margin_cache
            .insert(0, Arc::new(["default|1".to_owned()].into()))
            .await;
        assert!(bg.routing.model_cache.get("gpt-5").await.is_some());
        assert!(bg.routing.margin_cache.get(&0).await.is_some());
        bg.invalidate_routing_caches_local();
        assert!(gw.routing.model_cache.get("gpt-5").await.is_none());
        assert!(gw.routing.margin_cache.get(&0).await.is_none());

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        gw.core.settlements.spawn(async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
        });
        started_rx.await.expect("settlement task started");
        assert_eq!(gw.core.settlements.in_flight(), 1);
        assert_eq!(bg.core.settlements.in_flight(), 0);
        sibling.core.settlements.spawn(std::future::pending::<()>());
        assert_eq!(sibling.core.settlements.in_flight(), 1);
        assert_eq!(bg.core.settlements.in_flight(), 0);
        let _ = release_tx.send(());
        gw.core
            .settlements
            .wait_idle(std::time::Duration::from_secs(2))
            .await;
    }

    #[test]
    fn cache_invalidation_wire_roundtrip_and_legacy_compat() {
        for scope in [
            CacheInvalidation::All,
            CacheInvalidation::Channels,
            CacheInvalidation::Models,
            CacheInvalidation::Pools,
            CacheInvalidation::Margin,
            CacheInvalidation::Setting("site_notices".to_owned()),
            CacheInvalidation::Setting("request_policy".to_owned()),
        ] {
            let wire = scope.to_wire();
            assert_eq!(CacheInvalidation::from_wire(&wire), scope);
        }
        assert_eq!(CacheInvalidation::from_wire("1"), CacheInvalidation::All);
        assert_eq!(CacheInvalidation::from_wire(""), CacheInvalidation::All);
        assert_eq!(
            CacheInvalidation::from_wire("setting:"),
            CacheInvalidation::All
        );
        assert_eq!(
            CacheInvalidation::from_wire("unknown_future_scope"),
            CacheInvalidation::All
        );
    }

    #[tokio::test]
    async fn scoped_cache_invalidation_preserves_unrelated_caches() {
        let state = test_app_state();

        let seed_all = || async {
            state
                .routing
                .model_cache
                .insert("gpt-5".into(), Arc::new(None))
                .await;
            state
                .routing
                .active_models_cache
                .insert(0, Arc::new(Vec::new()))
                .await;
            state
                .routing
                .cand_cache
                .insert("gpt-5|default".into(), Arc::new(Vec::new()))
                .await;
            state.routing.channel_cost_cache.insert(1, 1000).await;
            state
                .routing
                .margin_cache
                .insert(0, Arc::new(["default|1".to_owned()].into()))
                .await;
            state
                .routing
                .request_policy
                .insert(0, Arc::new(okapi_kernel::policies::RequestPolicy::default()))
                .await;
            state
                .routing
                .pool_policies
                .insert(
                    "default".into(),
                    Arc::new(okapi_store::pool_policy::StoredPoolPolicy::fallback_default("default")),
                )
                .await;
            state
                .settings_cache
                .insert("site_notices".into(), Arc::new(None))
                .await;
            state
                .settings_cache
                .insert("request_policy".into(), Arc::new(None))
                .await;
        };

        // 1. Margin scope only clears margin_cache
        seed_all().await;
        state.invalidate_cache_local(&CacheInvalidation::Margin).await;
        assert!(state.routing.margin_cache.get(&0).await.is_none());
        assert!(state.routing.cand_cache.get("gpt-5|default").await.is_some());
        assert!(state.routing.model_cache.get("gpt-5").await.is_some());
        assert!(state.settings_cache.get("site_notices").await.is_some());

        // 2. Setting("site_notices") only clears that single settings key
        seed_all().await;
        state
            .invalidate_cache_local(&CacheInvalidation::Setting("site_notices".to_owned()))
            .await;
        assert!(state.settings_cache.get("site_notices").await.is_none());
        assert!(state.settings_cache.get("request_policy").await.is_some());
        assert!(state.routing.request_policy.get(&0).await.is_some());
        assert!(state.routing.cand_cache.get("gpt-5|default").await.is_some());

        // 3. Setting("request_policy") clears that key + request_policy cache only
        seed_all().await;
        state
            .invalidate_cache_local(&CacheInvalidation::Setting("request_policy".to_owned()))
            .await;
        assert!(state.settings_cache.get("request_policy").await.is_none());
        assert!(state.routing.request_policy.get(&0).await.is_none());
        assert!(state.settings_cache.get("site_notices").await.is_some());
        assert!(state.routing.cand_cache.get("gpt-5|default").await.is_some());

        // 4. Channels scope preserves model_cache / pool_policies / settings_cache
        seed_all().await;
        state
            .invalidate_cache_local(&CacheInvalidation::Channels)
            .await;
        assert!(state.routing.cand_cache.get("gpt-5|default").await.is_none());
        assert!(state.routing.channel_cost_cache.get(&1).await.is_none());
        assert!(state.routing.model_cache.get("gpt-5").await.is_some());
        assert!(state.routing.active_models_cache.get(&0).await.is_some());
        assert!(state.routing.pool_policies.get("default").await.is_some());
        assert!(state.settings_cache.get("site_notices").await.is_some());

        // 5. Models scope clears model_cache + active_models_cache + cand_cache, preserves channel_cost + pool_policies
        seed_all().await;
        state
            .invalidate_cache_local(&CacheInvalidation::Models)
            .await;
        assert!(state.routing.model_cache.get("gpt-5").await.is_none());
        assert!(state.routing.active_models_cache.get(&0).await.is_none());
        assert!(state.routing.cand_cache.get("gpt-5|default").await.is_none());
        assert!(state.routing.channel_cost_cache.get(&1).await.is_some());
        assert!(state.routing.pool_policies.get("default").await.is_some());

        // 6. Pools scope clears cand_cache + pool_policies, preserves model_cache + active_models_cache + channel_cost
        seed_all().await;
        state
            .invalidate_cache_local(&CacheInvalidation::Pools)
            .await;
        assert!(state.routing.cand_cache.get("gpt-5|default").await.is_none());
        assert!(state.routing.pool_policies.get("default").await.is_none());
        assert!(state.routing.model_cache.get("gpt-5").await.is_some());
        assert!(state.routing.active_models_cache.get(&0).await.is_some());
        assert!(state.routing.channel_cost_cache.get(&1).await.is_some());
    }

    #[tokio::test]
    async fn moka_try_get_with_coalesces_concurrent_misses_and_never_caches_errors() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache: Cache<String, Arc<usize>> = Cache::builder().max_capacity(8).build();
        let loads = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(8));

        // First wave: 8 concurrent callers fail — error is coalesced and NOT cached.
        let mut failing = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let loads = Arc::clone(&loads);
            let barrier = Arc::clone(&barrier);
            failing.push(tokio::spawn(async move {
                barrier.wait().await;
                cache
                    .try_get_with("k".to_owned(), async {
                        loads.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        Err::<Arc<usize>, &'static str>("db_down")
                    })
                    .await
            }));
        }
        for handle in failing {
            assert!(handle.await.expect("join").is_err());
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        // Second wave: 8 concurrent callers succeed — singleflight executes loader once.
        let mut succeeding = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let loads = Arc::clone(&loads);
            let barrier = Arc::clone(&barrier);
            succeeding.push(tokio::spawn(async move {
                barrier.wait().await;
                cache
                    .try_get_with("k".to_owned(), async {
                        let n = loads.fetch_add(1, Ordering::SeqCst) + 1;
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        Ok::<Arc<usize>, &'static str>(Arc::new(n))
                    })
                    .await
            }));
        }
        for handle in succeeding {
            assert_eq!(*handle.await.expect("join").expect("ok"), 2);
        }
        assert_eq!(loads.load(Ordering::SeqCst), 2);
    }
}
