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
