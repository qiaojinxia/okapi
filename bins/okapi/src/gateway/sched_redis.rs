//! 调度用 Redis：L2 会话粘性 + 渠道 key 并发信号量（IMPLEMENTATION §3.2/§3.5）。
//!
//! L1 response_id 绑定随 Responses API 于 M3 接入；L3 打分器数据面（EMA）为 M3 项，
//! 当前层内为权重随机。粘性键带哈希版本号 v1，算法升级走双写双读预案。

use axum::http::HeaderMap;
use fred::clients::Client;
use fred::interfaces::{KeysInterface, LuaInterface, SortedSetsInterface};
use fred::types::Expiration;
use okapi_store::AuthedKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 会话亲和 TTL（滑动续期）。
const SESSION_TTL_SECS: i64 = 3600;
/// 信号量泄漏保护 TTL（崩溃后最迟 1h 自愈；正常路径显式 release）。
const SLOT_TTL_SECS: i64 = 3600;

#[derive(Clone)]
pub struct SchedulerRedis {
    client: Client,
}

impl SchedulerRedis {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    fn sess_key(user_id: i64, hash: &str) -> String {
        format!("stick:sess:{{{user_id}}}:v1:{hash}")
    }

    fn slot_key(channel_key_id: i64) -> String {
        format!("conc:ck:{channel_key_id}")
    }

    /// 读会话粘性映射并滑动续期（GET + EXPIRE 两步；续期非关键路径无需原子）。
    pub async fn sticky_get(&self, user_id: i64, session_hash: &str) -> Option<i64> {
        let key = Self::sess_key(user_id, session_hash);
        let value: Option<String> = self.client.get(&key).await.ok()?;
        let value = value?;
        let _: Result<bool, _> = self.client.expire(&key, SESSION_TTL_SECS, None).await;
        value.parse().ok()
    }

    /// 建立/刷新会话粘性映射（尽力而为：失败仅降低 cache 命中，不影响正确性）。
    pub async fn sticky_set(&self, user_id: i64, session_hash: &str, channel_key_id: i64) {
        let result: Result<(), _> = self
            .client
            .set(
                Self::sess_key(user_id, session_hash),
                channel_key_id.to_string(),
                Some(Expiration::EX(SESSION_TTL_SECS)),
                None,
                false,
            )
            .await;
        if let Err(err) = result {
            tracing::debug!(error = %err, "sticky_set 失败（忽略）");
        }
    }

    /// 渠道 key 并发信号量 acquire。cap 为 None/<=0 时不限。
    /// Redis 故障时放行：并发上限是保护性尽力语义，账本侧 fail-closed 已由 reserve 兜底。
    pub async fn acquire_slot(&self, channel_key_id: i64, cap: Option<i32>) -> bool {
        let Some(cap) = cap.filter(|c| *c > 0) else {
            return true;
        };
        let key = Self::slot_key(channel_key_id);
        let count: i64 = match self.client.incr(&key).await {
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(error = %err, "并发信号量 INCR 失败，放行");
                return true;
            }
        };
        if count == 1 {
            let _: Result<bool, _> = self.client.expire(&key, SLOT_TTL_SECS, None).await;
        }
        if count > i64::from(cap) {
            let _: Result<i64, _> = self.client.decr(&key).await;
            return false;
        }
        true
    }

    // ---- 鉴权缓存（docs/database.md §2.1 auth:key:<sha256>，60s TTL）----
    // 失效模型：全局版本键 auth:ver，值内嵌写入时版本；INCR 即 O(1) 全量失效，
    // 跨进程立即生效（满足 §2.4 console 精确撤销语义），另有 60s TTL 兜底。

    /// 读鉴权缓存（版本不匹配视为 miss）。
    pub async fn auth_get(&self, key_hash: &str) -> Option<AuthedKey> {
        let keys = vec![format!("auth:key:{key_hash}"), "auth:ver".to_owned()];
        let values: Vec<Option<String>> = self.client.mget(keys).await.ok()?;
        let payload = values.first()?.clone()?;
        let current_ver = values
            .get(1)
            .and_then(|v| v.as_deref())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let entry: AuthCacheEntry = serde_json::from_str(&payload).ok()?;
        if entry.ver != current_ver {
            return None;
        }
        Some(entry.key)
    }

    /// 写鉴权缓存（携带当前版本）。
    pub async fn auth_set(&self, key_hash: &str, key: &AuthedKey) {
        let ver: i64 = self
            .client
            .get::<Option<String>, _>("auth:ver")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let entry = AuthCacheEntry {
            ver,
            key: key.clone(),
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            let result: Result<(), _> = self
                .client
                .set(
                    format!("auth:key:{key_hash}"),
                    json,
                    Some(Expiration::EX(60)),
                    None,
                    false,
                )
                .await;
            if let Err(err) = result {
                tracing::debug!(error = %err, "auth_set 失败（忽略）");
            }
        }
    }

    /// 全量失效（角色/分组等用户级变更后调用；跨进程生效）。
    pub async fn auth_flush(&self) {
        let result: Result<i64, _> = self.client.incr("auth:ver").await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "auth_flush 失败（依赖 60s TTL 兜底）");
        }
    }

    /// 单 key 精确失效（key 禁用/删除场景）。
    pub async fn auth_del(&self, key_hash: &str) {
        let _: Result<i64, _> = self.client.del(format!("auth:key:{key_hash}")).await;
    }

    /// 释放信号量（下限 0 防漂移）。
    pub async fn release_slot(&self, channel_key_id: i64, cap: Option<i32>) {
        if cap.is_none_or(|c| c <= 0) {
            return;
        }
        let key = Self::slot_key(channel_key_id);
        if let Ok(n) = self.client.decr::<i64, _>(&key).await
            && n < 0
        {
            let _: Result<(), _> = self
                .client
                .set(&key, "0", Some(Expiration::EX(SLOT_TTL_SECS)), None, false)
                .await;
        }
    }
}

#[derive(Serialize, Deserialize)]
struct AuthCacheEntry {
    ver: i64,
    key: AuthedKey,
}

const WEB_SESSION_TTL_SECS: i64 = 7 * 24 * 3600;

impl SchedulerRedis {
    /// web 会话（/auth/* 自助面专用，§6.4）：7d 滑动过期。
    pub async fn web_session_set(&self, sid: &str, user_id: i64) {
        let result: Result<(), _> = self
            .client
            .set(
                format!("sess:web:{sid}"),
                user_id.to_string(),
                Some(Expiration::EX(WEB_SESSION_TTL_SECS)),
                None,
                false,
            )
            .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "web_session_set 失败");
        }
    }

    pub async fn web_session_get(&self, sid: &str) -> Option<i64> {
        let key = format!("sess:web:{sid}");
        let value: Option<String> = self.client.get(&key).await.ok()?;
        let value = value?;
        let _: Result<bool, _> = self.client.expire(&key, WEB_SESSION_TTL_SECS, None).await;
        value.parse().ok()
    }

    pub async fn web_session_del(&self, sid: &str) {
        let _: Result<i64, _> = self.client.del(format!("sess:web:{sid}")).await;
    }

    /// 用户×模型 RPM（§11.1 new-api 吸收；INCR 尽力语义，Redis 故障放行）。
    pub async fn model_rate_ok(&self, user_id: i64, model: &str, limit: i64) -> bool {
        let minute = chrono::Utc::now().timestamp() / 60;
        let key = format!("rl:{{{user_id}}}:m:{model}:rpm:{minute}");
        let count: i64 = match self.client.incr(&key).await {
            Ok(n) => n,
            Err(err) => {
                tracing::debug!(error = %err, "model_rate incr 失败（放行）");
                return true;
            }
        };
        if count == 1 {
            let _: Result<bool, _> = self.client.expire(&key, 120, None).await;
        }
        count <= limit
    }

    /// 渠道 key 级 RPM 闸（`channel_keys.rpm_limit`）。
    ///
    /// 超限返回 false，调用方把该 key 摘出候选而不是拒绝整个请求——同渠道其它 key
    /// 仍可承接。Redis 故障时放行：宁可短暂超上游限速，也不因缓存抖动打挂全站。
    pub async fn channel_key_rate_ok(&self, channel_key_id: i64, limit: i64) -> bool {
        let minute = chrono::Utc::now().timestamp() / 60;
        let key = format!("rpm:ck:{channel_key_id}:{minute}");
        let count: i64 = match self.client.incr(&key).await {
            Ok(n) => n,
            Err(err) => {
                tracing::debug!(error = %err, "channel_key_rate incr 失败（放行）");
                return true;
            }
        };
        if count == 1 {
            let _: Result<bool, _> = self.client.expire(&key, 120, None).await;
        }
        count <= limit
    }

    /// 渠道 key 当日累计消费（micro）。读不到按 0 处理 = 不拦。
    pub async fn channel_key_spend_get(&self, channel_key_id: i64) -> i64 {
        let key = Self::channel_key_spend_key(channel_key_id);
        self.client
            .get::<Option<i64>, _>(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// 结算后累加渠道 key 当日消费。软实时：先花后记，可能略超上限。
    pub async fn channel_key_spend_add(&self, channel_key_id: i64, amount_micro: i64) {
        let key = Self::channel_key_spend_key(channel_key_id);
        if let Err(err) = self.client.incr_by::<i64, _>(&key, amount_micro).await {
            tracing::debug!(error = %err, "channel_key_spend 累加失败");
            return;
        }
        let _: Result<bool, _> = self.client.expire(&key, 172_800, None).await;
    }

    fn channel_key_spend_key(channel_key_id: i64) -> String {
        let day = chrono::Utc::now().format("%Y%m%d");
        format!("spend:ck:{channel_key_id}:{day}")
    }

    /// 渠道 key 时延 EWMA（毫秒）。无样本返回 None，调用方按中位数处理，
    /// 避免新 key 因"没有历史"被永久冷落，也避免被误判为最快而被灌流。
    pub async fn channel_key_latency(&self, channel_key_id: i64) -> Option<u32> {
        self.client
            .get::<Option<u32>, _>(&format!("lat:ck:{channel_key_id}"))
            .await
            .ok()
            .flatten()
    }

    /// 更新时延 EWMA：`new = old * 0.7 + sample * 0.3`（整数运算，非计费路径）。
    /// 权重偏向历史，单次抖动不足以改变选路；10min TTL 让长期不用的 key 自然回到无样本。
    pub async fn channel_key_latency_record(&self, channel_key_id: i64, sample_ms: u32) {
        let key = format!("lat:ck:{channel_key_id}");
        let next = match self.client.get::<Option<u32>, _>(&key).await {
            Ok(Some(old)) => (u64::from(old) * 7 + u64::from(sample_ms) * 3) / 10,
            _ => u64::from(sample_ms),
        };
        let next = u32::try_from(next).unwrap_or(u32::MAX);
        if let Err(err) = self
            .client
            .set::<(), _, _>(&key, next, None, None, false)
            .await
        {
            tracing::debug!(error = %err, "时延 EWMA 写入失败");
            return;
        }
        let _: Result<bool, _> = self.client.expire(&key, 600, None).await;
    }

    /// 团成员本月消费计数（软实时限额语义，IMPLEMENTATION §6.1）。
    pub async fn member_spend_get(&self, team: i64, member: i64) -> i64 {
        let key = Self::member_spend_key(team, member);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 结算后累加（40d TTL 覆盖整月 + 复核余量）。
    pub async fn member_spend_add(&self, team: i64, member: i64, amount_micro: i64) {
        if amount_micro <= 0 {
            return;
        }
        let key = Self::member_spend_key(team, member);
        let incr: Result<i64, _> = self.client.incr_by(&key, amount_micro).await;
        if incr.is_ok() {
            let _: Result<bool, _> = self.client.expire(&key, 40 * 24 * 3600, None).await;
        }
    }

    fn member_spend_key(team: i64, member: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("spend:tm:{team}:{member}:{month}")
    }

    /// 用户本月累计 token（volume 规则唯一输入，docs/database.md §2.1）。
    /// 读失败按 0 返回——量级折扣宁可不打，也不能因 Redis 抖动错算。
    pub async fn monthly_tokens_get(&self, user_id: i64) -> u64 {
        let key = Self::monthly_tokens_key(user_id);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 结算后累加实际 usage 总量（40d TTL 覆盖整月 + 复核余量）。
    pub async fn monthly_tokens_add(&self, user_id: i64, tokens: u64) {
        let Ok(delta) = i64::try_from(tokens) else {
            return;
        };
        if delta <= 0 {
            return;
        }
        let key = Self::monthly_tokens_key(user_id);
        let incr: Result<i64, _> = self.client.incr_by(&key, delta).await;
        if incr.is_ok() {
            let _: Result<bool, _> = self.client.expire(&key, 40 * 24 * 3600, None).await;
        }
    }

    fn monthly_tokens_key(user_id: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("tok:{{{user_id}}}:{month}")
    }

    /// 用户本月累计消费 micro（volume 规则消费额轴输入；语义与 tok 计数同构：
    /// 结算后累加、报价前读取，读失败按 0 = 不打折，宁少算不错算）。
    pub async fn monthly_spend_get(&self, user_id: i64) -> u64 {
        let key = Self::monthly_spend_key(user_id);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 结算后累加实付 micro（40d TTL 覆盖整月 + 复核余量）。
    pub async fn monthly_spend_add(&self, user_id: i64, amount_micro: i64) {
        if amount_micro <= 0 {
            return;
        }
        let key = Self::monthly_spend_key(user_id);
        let incr: Result<i64, _> = self.client.incr_by(&key, amount_micro).await;
        if incr.is_ok() {
            let _: Result<bool, _> = self.client.expire(&key, 40 * 24 * 3600, None).await;
        }
    }

    fn monthly_spend_key(user_id: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("usd:{{{user_id}}}:{month}")
    }

    /// 关键接口每 IP 固定窗计数（60s；对齐 new-api rc.24 关键路由限流）。
    /// 返回窗口内计数；Redis 故障返回 0（放行，与其余限流失败语义一致）。
    pub async fn crit_rate_incr(&self, scope: &str, ip: &str) -> i64 {
        let key = format!("crl:{scope}:{ip}");
        let count: Result<i64, _> = self.client.incr(&key).await;
        let Ok(count) = count else {
            return 0;
        };
        if count == 1 {
            let _: Result<bool, _> = self.client.expire(&key, 60, None).await;
        }
        count
    }

    /// 兑换码批次 × IP 核销计数 +1（7d 窗口；#1790-5 max_per_ip 闸）。
    pub async fn redeem_ip_incr(&self, batch: uuid::Uuid, ip: &str) -> i64 {
        let key = format!("redeem:ip:{batch}:{ip}");
        let count: Result<i64, _> = self.client.incr(&key).await;
        let Ok(count) = count else {
            return 1; // Redis 故障放行（限制是风控增强，不阻核销主流程）
        };
        if count == 1 {
            let _: Result<bool, _> = self.client.expire(&key, 7 * 24 * 3600, None).await;
        }
        count
    }

    /// 核销失败回退计数（预查通过但翻转竞争失败时）。
    pub async fn redeem_ip_decr(&self, batch: uuid::Uuid, ip: &str) {
        let key = format!("redeem:ip:{batch}:{ip}");
        let _: Result<i64, _> = self.client.decr(&key).await;
    }

    /// videos 任务 → 渠道 key 映射写入（48h；键含 user_id 天然租户隔离）。
    pub async fn video_task_set(&self, user_id: i64, task_id: &str, channel_key_id: i64) {
        let key = format!("video:task:{{{user_id}}}:{task_id}");
        let _: Result<(), _> = self
            .client
            .set(
                &key,
                channel_key_id.to_string(),
                Some(Expiration::EX(48 * 3600)),
                None,
                false,
            )
            .await;
    }

    /// videos 任务映射读取（None = 未知任务/已过期/非本用户）。
    pub async fn video_task_get(&self, user_id: i64, task_id: &str) -> Option<i64> {
        let key = format!("video:task:{{{user_id}}}:{task_id}");
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok())
    }

    fn ws_lease_key(key_id: i64) -> String {
        format!("ws:lease:k:{key_id}")
    }

    /// Realtime WS per-key 连接租约获取（§14.4；docs/database.md §2.1 ws:lease:k:*）。
    /// ZSET 成员 = 连接 id，score = 租约到期毫秒：先清过期再计数，原子准入。
    /// 崩溃的连接不续期即自然滚出窗口，无泄漏。Redis 故障放行（与其余限流一致）。
    pub async fn ws_lease_acquire(&self, key_id: i64, conn_id: &str, limit: i64) -> bool {
        const LUA: &str = r"
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
            if redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[2]) then return 0 end
            redis.call('ZADD', KEYS[1], tonumber(ARGV[1]) + 60000, ARGV[3])
            redis.call('PEXPIRE', KEYS[1], 6 * 3600 * 1000)
            return 1
        ";
        let now_ms = chrono::Utc::now().timestamp_millis();
        let result: Result<i64, _> = self
            .client
            .eval(
                LUA,
                vec![Self::ws_lease_key(key_id)],
                vec![now_ms.to_string(), limit.to_string(), conn_id.to_owned()],
            )
            .await;
        result.map_or(true, |v| v == 1)
    }

    /// 租约续期（会话泵内每 20s；60s 窗口容忍两次丢失）。
    // 毫秒时间戳 ~1.7e12 远小于 f64 尾数上限 2^53，转换无损（ZSET score 为 f64 是 Redis 语义）
    #[allow(clippy::cast_precision_loss)]
    pub async fn ws_lease_renew(&self, key_id: i64, conn_id: &str) {
        let expire_at = chrono::Utc::now().timestamp_millis() + 60_000;
        let _: Result<i64, _> = self
            .client
            .zadd(
                Self::ws_lease_key(key_id),
                Some(fred::types::SetOptions::XX),
                None,
                false,
                false,
                (expire_at as f64, conn_id),
            )
            .await;
    }

    /// 连接断开释放租约（断开/失败路径统一走这里）。
    pub async fn ws_lease_release(&self, key_id: i64, conn_id: &str) {
        let _: Result<i64, _> = self.client.zrem(Self::ws_lease_key(key_id), conn_id).await;
    }

    /// OAuth state 一次性键（CSRF 防线）：写入带 TTL。
    pub async fn oauth_state_set(&self, token: &str, ttl_secs: i64) {
        let result: Result<(), _> = self
            .client
            .set(
                format!("oauth:state:{token}"),
                "1",
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "oauth_state_set 失败");
        }
    }

    /// 校验并销毁（DEL 返回 1 = 有效且唯一一次）。
    pub async fn oauth_state_take(&self, token: &str) -> bool {
        let deleted: Result<i64, _> = self.client.del(format!("oauth:state:{token}")).await;
        deleted.is_ok_and(|n| n == 1)
    }

    /// 平台实时 KPI 秒桶累加（docs/database.md §2.1 `kpi:*`）。
    ///
    /// 四个序列各自一个「每秒一键」的计数器，而非设计初稿的 ZSET 滑窗——
    /// ZSET 要为每笔请求存一个成员，10k RPS × 60s = 60 万成员常驻内存；
    /// 秒桶无论多大流量都只有 4 × 120 个小键，且读侧一条 MGET 取满窗口。
    ///
    /// 单条 Lua 完成四路累加 = 一次往返；EXPIRE 只在该秒首次写入时下发
    /// （`INCRBY` 返回值等于增量即首次，与 `crit_rate_incr` 同法）。
    /// 全程 fire-and-forget：**账本原子、统计尽力**（§2.2 末条），
    /// KPI 写失败不得影响结算。
    pub async fn kpi_record(&self, tokens: u64, amount_micro: i64, is_error: bool) {
        const LUA: &str = r"
            local ttl = tonumber(ARGV[1])
            for i = 1, 4 do
                local by = tonumber(ARGV[i + 1])
                if by > 0 and redis.call('INCRBY', KEYS[i], by) == by then
                    redis.call('EXPIRE', KEYS[i], ttl)
                end
            end
            return 1
        ";
        let sec = chrono::Utc::now().timestamp();
        let result: Result<i64, _> = self
            .client
            .eval(
                LUA,
                Self::kpi_keys(sec),
                vec![
                    KPI_TTL_SECS.to_string(),
                    "1".to_owned(),
                    i64::try_from(tokens).unwrap_or(i64::MAX).to_string(),
                    amount_micro.max(0).to_string(),
                    i64::from(is_error).to_string(),
                ],
            )
            .await;
        if let Err(err) = result {
            tracing::debug!(error = %err, "KPI 秒桶累加失败（忽略）");
        }
    }

    /// 读取最近 `window` 个**已完成**秒的 KPI 序列（旧→新）。
    ///
    /// 不含当前这一秒：它还在累加中，读进来会让每次刷新都看到一个偏低的尾点，
    /// 像是流量刚刚掉下去。窗口内四序列同 hash-tag，一条 MGET 取完。
    pub async fn kpi_window(&self, window: i64) -> Vec<KpiSecond> {
        let window = window.clamp(1, KPI_WINDOW_MAX);
        let latest = chrono::Utc::now().timestamp() - 1;
        let seconds: Vec<i64> = ((latest - window + 1)..=latest).collect();
        let keys: Vec<String> = seconds.iter().flat_map(|s| Self::kpi_keys(*s)).collect();
        let values: Vec<Option<i64>> = self.client.mget(keys).await.unwrap_or_default();

        seconds
            .iter()
            .enumerate()
            .map(|(idx, ts)| {
                let at = |offset: usize| {
                    values
                        .get(idx * KPI_SERIES + offset)
                        .copied()
                        .flatten()
                        .unwrap_or(0)
                };
                KpiSecond {
                    ts: *ts,
                    requests: at(0),
                    tokens: at(1),
                    amount_micro: at(2),
                    errors: at(3),
                }
            })
            .collect()
    }

    /// 记录渠道最近一次测活结果（`ch:test:<channel_id>`，30 天 TTL）。
    /// 提示性信息不进 PG：new-api 把 response_time/test_time 存在 channels 表上，
    /// 我们用 Redis——它天然会过期，列表上不会挂着半年前的"200ms"误导人。
    pub async fn channel_test_record(&self, channel_id: i64, result: &serde_json::Value) {
        let key = format!("ch:test:{channel_id}");
        let value = result.to_string();
        if let Err(err) = self
            .client
            .set::<(), _, _>(
                &key,
                value,
                Some(Expiration::EX(30 * 24 * 3600)),
                None,
                false,
            )
            .await
        {
            tracing::debug!(error = %err, "channel_test_record 失败（忽略）");
        }
    }

    /// 批量读最近测活结果（列表页一次 MGET 回填所有行；读失败按空处理）。
    pub async fn channel_test_get_many(
        &self,
        channel_ids: &[i64],
    ) -> std::collections::HashMap<i64, serde_json::Value> {
        if channel_ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let keys: Vec<String> = channel_ids
            .iter()
            .map(|id| format!("ch:test:{id}"))
            .collect();
        let values: Vec<Option<String>> = self.client.mget(keys).await.unwrap_or_default();
        channel_ids
            .iter()
            .zip(values)
            .filter_map(|(id, v)| {
                v.and_then(|s| serde_json::from_str(&s).ok())
                    .map(|parsed| (*id, parsed))
            })
            .collect()
    }

    /// 读某把 key 的限速计数器当前值（本分钟 RPM/TPM、当日 RPD），
    /// 键形态与 reserve Lua 完全一致（docs/database.md §2.1 `rl:{uid}:k:*`）。
    ///
    /// 这是限流器**自己的视角**：RPM 计的是 reserve 通过的请求数、TPM 计的是预扣
    /// 估算 token——正因如此它才能回答"我离限流还有多远"，事后按 usage 算的
    /// 速率答不了这个问题。读失败一律 0（展示用途，不影响任何判定）。
    pub async fn key_rate_snapshot(&self, user_id: i64, key_id: i64) -> (i64, i64, i64) {
        let now = chrono::Utc::now();
        let minute = now.timestamp().div_euclid(60);
        let day = now.format("%Y%m%d");
        let keys = vec![
            format!("rl:{{{user_id}}}:k:{key_id}:rpm:{minute}"),
            format!("rl:{{{user_id}}}:k:{key_id}:tpm:{minute}"),
            format!("rl:{{{user_id}}}:k:{key_id}:rpd:{day}"),
        ];
        let values: Vec<Option<i64>> = self.client.mget(keys).await.unwrap_or_default();
        let at = |i: usize| values.get(i).copied().flatten().unwrap_or(0);
        (at(0), at(1), at(2))
    }

    /// 某一秒的四个序列键。`{kpi}` hash-tag 保证 Cluster 下同槽，
    /// 使跨序列跨秒的 MGET 成立（否则读窗口要退化成 N 次往返）。
    fn kpi_keys(sec: i64) -> Vec<String> {
        ["req", "tok", "amt", "err"]
            .iter()
            .map(|series| format!("kpi:{{kpi}}:{series}:{sec}"))
            .collect()
    }
}

/// KPI 秒桶的序列数（req / tok / amt / err），MGET 结果按此步长切片。
const KPI_SERIES: usize = 4;
/// 秒桶存活时长：覆盖最大查询窗口 + 时钟偏移余量。
const KPI_TTL_SECS: i64 = 360;
/// 实时窗口上限（秒）。超过这个跨度就该看 CH 聚合而非 Redis 秒桶。
pub const KPI_WINDOW_MAX: i64 = 300;

/// 一秒的平台 KPI 采样。
pub struct KpiSecond {
    pub ts: i64,
    pub requests: i64,
    pub tokens: i64,
    pub amount_micro: i64,
    pub errors: i64,
}

/// 会话标识提取（§3.2）：优先客户端会话头（`session_id` / `x-session-id`，
/// Nginx 需 `underscores_in_headers on`），缺省取首两条消息规范化文本哈希。
/// 哈希 = SHA-256 前 8 字节 hex（xxhash 为 M3 性能优化项）。
#[must_use]
pub fn session_hash(headers: &HeaderMap, messages: &[okapi_api::MessageProbe]) -> Option<String> {
    for name in ["session_id", "x-session-id"] {
        if let Some(value) = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Some(short_hash(value.as_bytes()));
        }
    }

    let mut text = String::new();
    for message in messages.iter().take(2) {
        text.push_str(&message.role);
        text.push('\u{0}');
        append_content_text(&mut text, &message.content);
        text.push('\u{0}');
    }
    if text.len() <= messages.len().saturating_mul(2) {
        return None; // 无实际内容
    }
    Some(short_hash(text.as_bytes()))
}

fn append_content_text(out: &mut String, content: &serde_json::Value) {
    match content {
        serde_json::Value::String(s) => out.push_str(s),
        serde_json::Value::Array(parts) => {
            for part in parts {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    out.push_str(t);
                }
            }
        }
        _ => {}
    }
}

fn short_hash(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    hex::encode(&digest[..8])
}
