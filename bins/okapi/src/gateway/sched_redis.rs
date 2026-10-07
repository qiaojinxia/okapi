//! 调度用 Redis：Responses L1 历史绑定、L2 会话亲和与渠道 key 并发信号量。
//!
//! L1 在 response_affinity 中按用户/API key 隔离且不可改绑；L2 只提升缓存命中。
//! 粘性键带版本号，L1 的 v2 哈希原始响应 ID，L2 沿用 v1。

use axum::http::HeaderMap;
use fred::clients::Client;
use fred::interfaces::{
    HashesInterface, KeysInterface, LuaInterface, SetsInterface, SortedSetsInterface,
};
use fred::types::{Expiration, SetOptions};
use okapi_store::AuthedKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod batch_statistics;
pub mod channel_permit;
pub mod response_affinity;
mod responses_ws;
pub(crate) mod token_count;

/// 会话亲和 TTL（滑动续期）。
const SESSION_TTL_SECS: i64 = 3600;

/// 在途量表：实例上报间隔上限 1s，故 10s 没动静即认为该实例已不在（不计入合计）。
const INFLIGHT_STALE_MS: i64 = 10_000;
/// 超过这个年龄的格子直接删掉，免得 pod 反复重建把 hash 撑大（名字随 pod 变）。
const INFLIGHT_EVICT_MS: i64 = 300_000;
/// 集群在途量表键。
const INFLIGHT_KEY: &str = "inflight:gauge";

#[derive(Clone)]
pub struct SchedulerRedis {
    client: Client,
}

impl SchedulerRedis {
    pub(super) fn client(&self) -> &Client {
        &self.client
    }
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    fn sess_key(user_id: i64, hash: &str) -> String {
        format!("stick:sess:{{{user_id}}}:v1:{hash}")
    }

    /// 上报本实例的在途请求数（surge 规则的负载输入）。
    ///
    /// 为什么按实例分格而不是一个全局 INCR/DECR 计数器：pod 崩在请求中间就再也减不回来，
    /// 计数只会单调漂高，surge 会永久卡在加价状态且没人察觉得到。每个实例只写自己那一格
    /// （`node → "<count>|<unix_ms>"`），崩掉的实例最多影响一个陈旧窗口就被读侧排除。
    pub async fn inflight_report(&self, node: &str, count: i64) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let value = format!("{count}|{now_ms}");
        if let Err(err) = self
            .client
            .hset::<(), _, _>(INFLIGHT_KEY, (node.to_owned(), value))
            .await
        {
            tracing::debug!(error = %err, "在途量上报失败");
            return;
        }
        // 整表兜底过期：全站没有网关在跑时不留垃圾键
        let _: Result<bool, _> = self.client.expire(INFLIGHT_KEY, 3600, None).await;
    }

    /// 集群在途请求数合计（陈旧格子不计；顺手清掉早已消失的实例）。
    pub async fn inflight_total(&self) -> i64 {
        let map: std::collections::HashMap<String, String> =
            match self.client.hgetall(INFLIGHT_KEY).await {
                Ok(m) => m,
                Err(err) => {
                    // 读不到就当没有负载：surge 是加价规则，宁可不加也不要凭空加
                    tracing::debug!(error = %err, "在途量读取失败，按零负载处理");
                    return 0;
                }
            };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut total = 0_i64;
        let mut evict: Vec<String> = Vec::new();
        for (node, raw) in map {
            let Some((count, at_ms)) = raw
                .split_once('|')
                .and_then(|(c, t)| Some((c.parse::<i64>().ok()?, t.parse::<i64>().ok()?)))
            else {
                evict.push(node);
                continue;
            };
            let age = now_ms.saturating_sub(at_ms);
            if age > INFLIGHT_EVICT_MS {
                evict.push(node);
            } else if age <= INFLIGHT_STALE_MS {
                total = total.saturating_add(count.max(0));
            }
        }
        if !evict.is_empty() {
            let _: Result<i64, _> = self.client.hdel(INFLIGHT_KEY, evict).await;
        }
        total
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
        // schema 3：条目带登录 key 的 session_hash；旧条目一律当未命中回源
        if entry.ver != current_ver || entry.schema != 3 {
            return None;
        }
        Some(entry.key)
    }

    /// Capture before reading PG; never stamp an old snapshot with a newer version.
    pub async fn auth_version(&self) -> Option<i64> {
        self.client
            .get::<Option<i64>, _>("auth:ver")
            .await
            .ok()
            .map(|v| v.unwrap_or(0))
    }

    pub async fn auth_set(&self, key_hash: &str, key: &AuthedKey, ver: i64) {
        let entry = AuthCacheEntry {
            ver,
            schema: 3,
            key: key.clone(),
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            let _: Result<(), _> = self
                .client
                .set(
                    format!("auth:key:{key_hash}"),
                    json,
                    Some(Expiration::EX(60)),
                    None,
                    false,
                )
                .await;
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
        self.auth_flush().await;
        let _: Result<i64, _> = self.client.del(format!("auth:key:{key_hash}")).await;
    }
}

#[derive(Serialize, Deserialize)]
struct AuthCacheEntry {
    ver: i64,
    schema: i16,
    key: AuthedKey,
}

const WEB_SESSION_TTL_SECS: i64 = 7 * 24 * 3600;

/// 门户会话列表行（不含 cookie 原文以外的敏感字段；sid 本身是能力令牌，仅回给属主）。
#[derive(Debug, Clone)]
pub struct WebSessionRow {
    pub sid: String,
    pub ip: Option<String>,
    pub ua: Option<String>,
    /// 建立时刻（unix 秒，展示用）。
    pub created_at: i64,
    /// 建立时刻（unix 毫秒，排序用）：同秒内多次登录要分得出先后，会话上限才能踢对人。
    pub created_ms: i64,
}

impl SchedulerRedis {
    /// web 会话（/auth/* 自助面专用，§6.4）：7d 滑动过期；同步写入用户索引与展示元数据。
    pub async fn web_session_set(
        &self,
        sid: &str,
        user_id: i64,
        ip: Option<&str>,
        ua: Option<&str>,
    ) {
        let web = format!("sess:web:{sid}");
        let idx = format!("sess:idx:{user_id}");
        let meta = format!("sess:meta:{sid}");
        // Index first, valid session last. Lua is atomic but does not roll back
        // command errors: this ordering also handles ACL rejection safely.
        let now = chrono::Utc::now();
        let script = r"
          local idx_type=redis.call('TYPE',KEYS[2]).ok
          local meta_type=redis.call('TYPE',KEYS[3]).ok
          if idx_type~='none' and idx_type~='set' then return redis.error_reply('invalid session index') end
          if meta_type~='none' and meta_type~='hash' then return redis.error_reply('invalid session metadata') end
          redis.call('SADD',KEYS[2],ARGV[1])
          redis.call('EXPIRE',KEYS[2],ARGV[3])
          redis.call('HSET',KEYS[3],'ip',ARGV[4],'ua',ARGV[5],'created_at',ARGV[6],'created_ms',ARGV[7])
          redis.call('EXPIRE',KEYS[3],ARGV[3])
          redis.call('SET',KEYS[1],ARGV[2],'EX',ARGV[3])
          return 1
        ";
        let result: Result<i64, _> = self
            .client
            .eval(
                script,
                vec![web, idx, meta],
                vec![
                    sid.to_owned(),
                    user_id.to_string(),
                    WEB_SESSION_TTL_SECS.to_string(),
                    ip.unwrap_or("").to_owned(),
                    ua.unwrap_or("").to_owned(),
                    now.timestamp().to_string(),
                    now.timestamp_millis().to_string(),
                ],
            )
            .await;
        if let Err(error) = result {
            tracing::warn!(%error, "web session creation failed");
        }
    }

    pub async fn web_session_get(&self, sid: &str) -> Option<i64> {
        let key = format!("sess:web:{sid}");
        let value: Option<String> = self.client.get(&key).await.ok()?;
        let uid: i64 = value?.parse().ok()?;
        // Verify membership and slide all three TTLs atomically with revocation.
        // Re-check the mapping after the initial uid lookup so a replaced or
        // revoked session cannot be authorized by a stale read.
        let script = r"
          if redis.call('GET',KEYS[1]) ~= ARGV[2] then return 0 end
          if redis.call('SISMEMBER',KEYS[2],ARGV[1]) ~= 1 then return 0 end
          redis.call('EXPIRE',KEYS[1],ARGV[3])
          redis.call('EXPIRE',KEYS[2],ARGV[3])
          redis.call('EXPIRE',KEYS[3],ARGV[3])
          return 1
        ";
        let valid: i64 = self
            .client
            .eval(
                script,
                vec![key, format!("sess:idx:{uid}"), format!("sess:meta:{sid}")],
                vec![
                    sid.to_owned(),
                    uid.to_string(),
                    WEB_SESSION_TTL_SECS.to_string(),
                ],
            )
            .await
            .ok()?;
        (valid == 1).then_some(uid)
    }

    pub async fn web_session_del(&self, sid: &str) {
        if let Some(uid) = self.web_session_get_raw(sid).await {
            let _: Result<i64, _> = self.client.srem(format!("sess:idx:{uid}"), sid).await;
        }
        let _: Result<i64, _> = self
            .client
            .del(vec![format!("sess:web:{sid}"), format!("sess:meta:{sid}")])
            .await;
    }

    async fn web_session_get_raw(&self, sid: &str) -> Option<i64> {
        let value: Option<String> = self.client.get(format!("sess:web:{sid}")).await.ok()?;
        value?.parse().ok()
    }

    /// 该用户仍有效的 web 会话（过期成员顺手从索引摘掉）。
    pub async fn web_session_list(&self, user_id: i64) -> Vec<WebSessionRow> {
        let idx = format!("sess:idx:{user_id}");
        let members: Vec<String> = self.client.smembers(&idx).await.unwrap_or_default();
        let mut out = Vec::with_capacity(members.len());
        for sid in members {
            match self.web_session_get_raw(&sid).await {
                Some(uid) if uid == user_id => {
                    let meta: std::collections::HashMap<String, String> = self
                        .client
                        .hgetall(format!("sess:meta:{sid}"))
                        .await
                        .unwrap_or_default();
                    let created_at = meta
                        .get("created_at")
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0);
                    out.push(WebSessionRow {
                        sid,
                        ip: meta.get("ip").filter(|s| !s.is_empty()).cloned(),
                        ua: meta.get("ua").filter(|s| !s.is_empty()).cloned(),
                        created_at,
                        // 旧会话没有毫秒字段：按秒补齐（同秒并列时靠 keep 钉住新会话即可）
                        created_ms: meta
                            .get("created_ms")
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or(created_at.saturating_mul(1000)),
                    });
                }
                _ => {
                    let _: Result<i64, _> = self.client.srem(&idx, sid.as_str()).await;
                }
            }
        }
        out.sort_by_key(|row| std::cmp::Reverse(row.created_ms));
        out
    }

    /// 会话数上限裁剪（§11.37）：按建立时刻（毫秒）降序保留前 `limit` 条，其余删掉；
    /// `keep` 是刚建立的那条，钉在首位永不被踢（毫秒也可能并列，不靠排序保证）。
    /// 返回被踢的 sid 数；`limit <= 0` = 不限。
    pub async fn web_session_trim(&self, user_id: i64, limit: i64, keep: &str) -> usize {
        let Ok(limit) = usize::try_from(limit) else {
            return 0;
        };
        if limit == 0 {
            return 0;
        }
        let mut rows = self.web_session_list(user_id).await;
        if let Some(pos) = rows.iter().position(|r| r.sid == keep) {
            let current = rows.remove(pos);
            rows.insert(0, current);
        }
        let evicted: Vec<WebSessionRow> = rows.drain(limit.min(rows.len())..).collect();
        for row in &evicted {
            self.web_session_del(&row.sid).await;
        }
        evicted.len()
    }

    /// 吊销一条：必须属于该用户。
    pub async fn web_session_revoke(&self, user_id: i64, sid: &str) -> bool {
        match self.web_session_get_raw(sid).await {
            Some(uid) if uid == user_id => {
                self.web_session_del(sid).await;
                true
            }
            _ => false,
        }
    }

    /// 清空该用户全部 web 会话（密码重置 / 封禁 / 删除）。
    pub async fn web_session_revoke_user(&self, user_id: i64) {
        let script = r"
          local members=redis.call('SMEMBERS',KEYS[1])
          for _,sid in ipairs(members) do
            redis.call('DEL','sess:web:'..sid,'sess:meta:'..sid)
          end
          redis.call('DEL',KEYS[1])
          return #members
        ";
        let result: Result<i64, _> = self
            .client
            .eval(
                script,
                vec![format!("sess:idx:{user_id}")],
                Vec::<String>::new(),
            )
            .await;
        if let Err(error) = result {
            tracing::error!(%error,user_id,"session revocation unavailable");
        }
    }

    /// 固定窗计数闸：INCR 后比上限（先计后判的"尽力语义"）；首次写入挂 TTL。
    /// Redis 故障放行——保护性限流宁可短暂失守，也不因缓存抖动打挂全站（账本才 fail-closed）。
    async fn fixed_window_ok(&self, key: &str, ttl_secs: i64, limit: i64) -> bool {
        let count: i64 = match self.client.incr(key).await {
            Ok(n) => n,
            Err(err) => {
                tracing::debug!(error = %err, key, "固定窗计数 incr 失败（放行）");
                return true;
            }
        };
        if count == 1 {
            let _: Result<bool, _> = self.client.expire(key, ttl_secs, None).await;
        }
        count <= limit
    }

    /// 用户×模型 RPM（§11.1 new-api 吸收）。
    pub async fn model_rate_ok(&self, user_id: i64, model: &str, limit: i64) -> bool {
        let minute = chrono::Utc::now().timestamp() / 60;
        let key = format!("rl:{{{user_id}}}:m:{model}:rpm:{minute}");
        self.fixed_window_ok(&key, 120, limit).await
    }

    /// 分组级限流（§11.32）：分组内每用户的分钟 / 小时固定窗。
    /// 返回超限的那一轴（`group_rpm` / `group_rph`）供错误 param；None = 放行。
    /// 未配置的轴不产生 Redis 往返。
    pub async fn group_rate_check(
        &self,
        user_id: i64,
        group: &str,
        rpm: Option<i64>,
        rph: Option<i64>,
    ) -> Option<&'static str> {
        let now = chrono::Utc::now().timestamp();
        if let Some(limit) = rpm {
            let key = format!("rl:{{{user_id}}}:g:{group}:rpm:{}", now / 60);
            if !self.fixed_window_ok(&key, 120, limit).await {
                return Some("group_rpm");
            }
        }
        if let Some(limit) = rph {
            let key = format!("rl:{{{user_id}}}:g:{group}:rph:{}", now / 3600);
            if !self.fixed_window_ok(&key, 7200, limit).await {
                return Some("group_rph");
            }
        }
        None
    }

    /// 渠道 key 级 RPM 闸（`channel_keys.rpm_limit`）。
    ///
    /// 超限返回 false，调用方把该 key 摘出候选而不是拒绝整个请求——同渠道其它 key
    /// 仍可承接。
    pub async fn channel_key_rate_ok(&self, channel_key_id: i64, limit: i64) -> bool {
        let minute = chrono::Utc::now().timestamp() / 60;
        let key = format!("rpm:ck:{channel_key_id}:{minute}");
        self.fixed_window_ok(&key, 120, limit).await
    }

    /// Refresh lease shared by request, worker and manual refresh. Redis failure is fail-closed.
    pub async fn cred_lock_acquire(
        &self,
        channel_key_id: i64,
    ) -> Result<Option<String>, fred::error::Error> {
        let owner = uuid::Uuid::new_v4().to_string();
        let set: Result<Option<String>, _> = self
            .client
            .set(
                format!("lock:cred:{channel_key_id}"),
                owner.clone(),
                Some(Expiration::EX(90)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        set.map(|result| result.map(|_| owner))
    }

    pub async fn cred_lock_release(&self, channel_key_id: i64, owner: &str) {
        let _: Result<i64, _> = self.client.eval(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end",
            vec![format!("lock:cred:{channel_key_id}")],
            vec![owner],
        ).await;
    }

    /// 负毛利熔断表整读（§11.34，`mb:blocks`）；Redis 故障 None = 调用方按不熔处理。
    pub async fn margin_blocks(
        &self,
    ) -> Option<std::collections::HashMap<String, crate::margin::BlockEntry>> {
        crate::margin::load_blocks(&self.client).await
    }

    /// 写一条熔断表条目（管理面解除 / 测试用）。
    pub async fn margin_block_set(
        &self,
        field: &str,
        entry: &crate::margin::BlockEntry,
    ) -> Result<(), fred::error::Error> {
        crate::margin::set_block(&self.client, field, entry).await
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

    /// One Redis round trip for the complete candidate set.
    pub async fn channel_key_latencies(
        &self,
        ids: impl Iterator<Item = i64>,
    ) -> std::collections::HashMap<i64, u32> {
        let ids: Vec<_> = ids.collect();
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let keys: Vec<_> = ids.iter().map(|id| format!("lat:ck:{id}")).collect();
        let values: Vec<Option<u32>> = self.client.mget(keys).await.unwrap_or_default();
        ids.into_iter()
            .zip(values)
            .filter_map(|(id, value)| value.map(|ms| (id, ms)))
            .collect()
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
        self.incr_with_ttl(&key, 60).await.unwrap_or(0)
    }

    /// 计数 +1 并保证键带过期，INCR 与 EXPIRE 在同一条脚本里原子完成。分两步写时，两步之间
    /// 进程崩溃或 EXPIRE 失败会留下永不过期的计数键，该 IP 从此被永久限流、只能人工删键；
    /// 这类旧键在下次计数时顺手补上过期。
    async fn incr_with_ttl(&self, key: &str, ttl_secs: i64) -> Result<i64, fred::error::Error> {
        const LUA: &str = r"
            local n = redis.call('INCR', KEYS[1])
            if n == 1 or redis.call('TTL', KEYS[1]) == -1 then
                redis.call('EXPIRE', KEYS[1], ARGV[1])
            end
            return n
        ";
        self.client
            .eval(LUA, vec![key.to_owned()], vec![ttl_secs.to_string()])
            .await
    }

    /// 兑换码批次 × IP 核销计数 +1（7d 窗口；#1790-5 max_per_ip 闸）。
    pub async fn redeem_ip_incr(&self, batch: uuid::Uuid, ip: &str) -> i64 {
        let key = format!("redeem:ip:{batch}:{ip}");
        // Redis 故障放行（限制是风控增强，不阻核销主流程）
        self.incr_with_ttl(&key, 7 * 24 * 3600).await.unwrap_or(1)
    }

    /// 核销失败回退计数（预查通过但翻转竞争失败时）。
    pub async fn redeem_ip_decr(&self, batch: uuid::Uuid, ip: &str) {
        // 只回退仍在窗口内的计数：对已过期的键 DECR 会凭空建出一个不带过期的 -1
        const LUA: &str = "if redis.call('EXISTS', KEYS[1]) == 1 then return redis.call('DECR', KEYS[1]) end return 0";
        let key = format!("redeem:ip:{batch}:{ip}");
        let _: Result<i64, _> = self.client.eval(LUA, vec![key], Vec::<String>::new()).await;
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
    /// 到期时间按 Redis 服务器时钟算（同 `channel_permit`）：各副本本机时钟有偏差时，
    /// 快的那台会把别的副本刚续上的存活租约当成过期清掉，放进超额连接。
    pub async fn ws_lease_acquire(&self, key_id: i64, conn_id: &str, limit: i64) -> bool {
        const LUA: &str = r"
            local t = redis.call('TIME')
            local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now)
            if redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[1]) then return 0 end
            redis.call('ZADD', KEYS[1], now + 60000, ARGV[2])
            redis.call('PEXPIRE', KEYS[1], 6 * 3600 * 1000)
            return 1
        ";
        let result: Result<i64, _> = self
            .client
            .eval(
                LUA,
                vec![Self::ws_lease_key(key_id)],
                vec![limit.to_string(), conn_id.to_owned()],
            )
            .await;
        result.map_or(true, |v| v == 1)
    }

    /// 租约续期（会话泵内每 20s；60s 窗口容忍两次丢失）。与获取同用 Redis 服务器时钟。
    pub async fn ws_lease_renew(&self, key_id: i64, conn_id: &str) {
        const LUA: &str = r"
            local t = redis.call('TIME')
            local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
            return redis.call('ZADD', KEYS[1], 'XX', now + 60000, ARGV[1])
        ";
        let _: Result<i64, _> = self
            .client
            .eval(
                LUA,
                vec![Self::ws_lease_key(key_id)],
                vec![conn_id.to_owned()],
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

    /// 渠道 OAuth 登录流程的 PKCE 状态（`oauth:cred:<state>`，§11.38）。返回 false = Redis 故障。
    pub async fn oauth_cred_state_set(&self, state: &str, payload: &str, ttl_secs: i64) -> bool {
        let result: Result<(), _> = self
            .client
            .set(
                format!("oauth:cred:{state}"),
                payload,
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        result.is_ok()
    }

    /// 出口代理后台探测的本轮租约（§11.41）：SET NX EX，拿到的副本执行这一轮。
    /// Redis 不可用时放行（最坏多个副本重复探一轮），不能让告警因为 Redis 抖一下就静默。
    pub async fn egress_probe_claim(&self, holder: &str, ttl_secs: i64) -> bool {
        let claimed: Result<Option<String>, _> = self
            .client
            .set(
                "egress:probe:round",
                holder,
                Some(Expiration::EX(ttl_secs.max(1))),
                Some(SetOptions::NX),
                false,
            )
            .await;
        !matches!(claimed, Ok(None))
    }

    /// 取出并销毁（GETDEL）；不存在 / 已用 / 过期 = None。
    pub async fn oauth_cred_state_take(&self, state: &str) -> Option<String> {
        self.client
            .getdel::<Option<String>, _>(format!("oauth:cred:{state}"))
            .await
            .ok()
            .flatten()
    }

    // ---- 邮箱验证码 / 找回密码（IMPLEMENTATION §11.27）----

    /// 同一邮箱重发冷却：NX 抢到 = 允许发送。Redis 故障放行（限流是尽力语义）。
    pub async fn email_code_cooldown_acquire(&self, email: &str, ttl_secs: i64) -> bool {
        let set: Result<Option<String>, _> = self
            .client
            .set(
                format!("verify:email:cd:{email}"),
                "1",
                Some(Expiration::EX(ttl_secs)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        match set {
            Ok(reply) => reply.is_some(),
            Err(_) => true,
        }
    }

    /// 存验证码（覆盖旧码）。返回 false = Redis 故障，调用方应报错而不是发一封对不上的码。
    pub async fn email_code_set(&self, email: &str, code: &str, ttl_secs: i64) -> bool {
        let result: Result<(), _> = self
            .client
            .set(
                format!("verify:email:{email}"),
                code,
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        if let Err(err) = &result {
            tracing::warn!(error = %err, "email_code_set 失败");
        }
        result.is_ok()
    }

    /// 校验并销毁：对上即 DEL（一次性）；不对 / 不存在 / Redis 故障 → false。
    pub async fn email_code_take(&self, email: &str, code: &str) -> bool {
        // Every attempt consumes the challenge; GETDEL makes success single-use
        // even when two requests present the right code concurrently.
        let stored: Option<String> = self
            .client
            .getdel(format!("verify:email:{email}"))
            .await
            .ok()
            .flatten();
        stored.is_some_and(|stored| {
            stored.len() == code.len()
                && stored
                    .bytes()
                    .zip(code.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
        })
    }

    /// 找回密码 token → user_id（键存 token 的 sha256，明文只出现在邮件里）。
    pub async fn pwreset_set(&self, token_hash: &str, user_id: i64, ttl_secs: i64) -> bool {
        let result: Result<(), _> = self
            .client
            .set(
                format!("pwreset:{token_hash}"),
                user_id.to_string(),
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        if let Err(err) = &result {
            tracing::warn!(error = %err, "pwreset_set 失败");
        }
        result.is_ok()
    }

    /// 取出并销毁 token（GETDEL），返回 user_id。
    pub async fn pwreset_take(&self, token_hash: &str) -> Option<i64> {
        let value: Option<String> = self
            .client
            .getdel(format!("pwreset:{token_hash}"))
            .await
            .ok()
            .flatten();
        value?.parse().ok()
    }

    pub async fn totp_pending_set(&self, binding: &str, sealed: &str) -> bool {
        let r: Result<(), _> = self
            .client
            .set(
                format!("totp:pending:{binding}"),
                sealed,
                Some(Expiration::EX(300)),
                None,
                false,
            )
            .await;
        r.is_ok()
    }
    pub async fn totp_pending_get(&self, binding: &str) -> Option<String> {
        self.client
            .get(format!("totp:pending:{binding}"))
            .await
            .ok()
            .flatten()
    }
    pub async fn totp_pending_del(&self, binding: &str) {
        let _: Result<i64, _> = self.client.del(format!("totp:pending:{binding}")).await;
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
        self.channel_note_record("ch:test", channel_id, result)
            .await;
    }

    /// 记录渠道最近一次上游余额查询结果（`ch:balance:<channel_id>`，30 天 TTL，§11.33）。
    pub async fn channel_balance_record(&self, channel_id: i64, result: &serde_json::Value) {
        self.channel_note_record("ch:balance", channel_id, result)
            .await;
    }

    async fn channel_note_record(&self, prefix: &str, channel_id: i64, result: &serde_json::Value) {
        let key = format!("{prefix}:{channel_id}");
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
            tracing::debug!(error = %err, key, "渠道留痕写入失败（忽略）");
        }
    }

    /// 批量读最近测活结果（列表页一次 MGET 回填所有行；读失败按空处理）。
    pub async fn channel_test_get_many(
        &self,
        channel_ids: &[i64],
    ) -> std::collections::HashMap<i64, serde_json::Value> {
        self.channel_note_get_many("ch:test", channel_ids).await
    }

    /// 批量读最近余额查询结果（同 MGET 回填）。
    pub async fn channel_balance_get_many(
        &self,
        channel_ids: &[i64],
    ) -> std::collections::HashMap<i64, serde_json::Value> {
        self.channel_note_get_many("ch:balance", channel_ids).await
    }

    async fn channel_note_get_many(
        &self,
        prefix: &str,
        channel_ids: &[i64],
    ) -> std::collections::HashMap<i64, serde_json::Value> {
        if channel_ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let keys: Vec<String> = channel_ids
            .iter()
            .map(|id| format!("{prefix}:{id}"))
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

#[cfg(test)]
mod verification_tests {
    use super::*;
    #[tokio::test]
    async fn email_code_is_consumed_atomically_including_failed_guesses() {
        okapi_store::test_support::assert_isolated();
        let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
            .await
            .unwrap();
        let sched = SchedulerRedis::new(redis);
        let email = format!("atomic-{}@example.test", uuid::Uuid::new_v4());
        assert!(sched.email_code_set(&email, "123456", 60).await);
        let (a, b) = tokio::join!(
            sched.email_code_take(&email, "123456"),
            sched.email_code_take(&email, "123456")
        );
        assert_ne!(a, b, "one winner per code");
        assert!(sched.email_code_set(&email, "123456", 60).await);
        assert!(!sched.email_code_take(&email, "000000").await);
        assert!(
            !sched.email_code_take(&email, "123456").await,
            "wrong guesses consume the challenge"
        );
    }
}
