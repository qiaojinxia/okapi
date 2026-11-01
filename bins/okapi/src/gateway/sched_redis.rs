//! 调度用 Redis：Responses L1 历史绑定、L2 会话亲和与渠道 key 并发信号量，以及各角色共用的
//! 小块 Redis 状态。
//!
//! L1 在 response_affinity 中按用户/API key 隔离且不可改绑；L2（session_affinity）只提升缓存命中。
//! 粘性键带版本号，L1 的 v2 哈希原始响应 ID，L2 沿用 v1。
//!
//! 本文件只放连接与共用原语（多键读、固定窗 / TTL 计数）；方法按领域分在子模块里：
//! 数据面选路（channel_keys、channel_permit、cluster_inflight、media）、鉴权与限流
//! （auth_cache、user_limits、abuse_counters）、控制面（web_sessions、verification、
//! channel_notes、kpi）、后台租约（background_leases）。

use axum::http::HeaderMap;
use fred::clients::Client;
use fred::interfaces::{KeysInterface, LuaInterface};
use sha2::{Digest, Sha256};

mod abuse_counters;
mod auth_cache;
mod background_leases;
mod batch_statistics;
mod channel_keys;
mod channel_notes;
pub mod channel_permit;
mod cluster_inflight;
mod kpi;
mod media;
pub mod response_affinity;
mod responses_ws;
mod session_affinity;
pub(crate) mod token_count;
mod user_limits;
mod verification;
mod web_sessions;

use self::kpi::KPI_TTL_SECS;
pub use self::kpi::{KPI_WINDOW_MAX, KpiSecond};
pub use self::session_affinity::StickyHit;
pub use self::web_sessions::WebSessionRow;

#[derive(Clone)]
pub struct SchedulerRedis {
    client: Client,
}

impl SchedulerRedis {
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// 多键读，键不同槽也成立：管道里逐条 GET，单机仍是一次往返，Cluster 下 fred 按槽分发。
    /// MGET 要求所有键同槽，`auth:key:*` + `auth:ver`、`lat:ck:*` 这类无 tag 的键在 Cluster 下会 CROSSSLOT。
    async fn get_many<T: fred::types::FromValue>(
        &self,
        keys: Vec<String>,
    ) -> Result<Vec<Option<T>>, fred::error::Error> {
        let pipe = self.client.pipeline();
        for key in keys {
            let _: () = pipe.get(key).await?;
        }
        pipe.all().await
    }

    /// 固定窗计数闸：INCR 后比上限（先计后判的"尽力语义"）；首次写入挂 TTL。
    /// Redis 故障放行——保护性限流宁可短暂失守，也不因缓存抖动打挂全站（账本才 fail-closed）。
    async fn fixed_window_ok(&self, key: &str, ttl_secs: i64, limit: i64) -> bool {
        let count: i64 = match self.incr_with_ttl(key, ttl_secs).await {
            Ok(n) => n,
            Err(err) => {
                tracing::debug!(error = %err, key, "固定窗计数 incr 失败（放行）");
                return true;
            }
        };
        count <= limit
    }

    /// 计数 +1 并保证键带过期，INCR 与 EXPIRE 在同一条脚本里原子完成。分两步写时，两步之间
    /// 进程崩溃或 EXPIRE 失败会留下永不过期的计数键，该 IP 从此被永久限流、只能人工删键；
    /// 这类旧键在下次计数时顺手补上过期。
    async fn incr_with_ttl(&self, key: &str, ttl_secs: i64) -> Result<i64, fred::error::Error> {
        self.incr_by_with_ttl(key, 1, ttl_secs).await
    }

    /// [`Self::incr_with_ttl`] 的任意增量版（消费 / token 累加器）：键名带时间桶，丢一次 EXPIRE
    /// 就是一个永不过期的旧桶，所以同样走原子脚本。
    pub(crate) async fn incr_by_with_ttl(
        &self,
        key: &str,
        delta: i64,
        ttl_secs: i64,
    ) -> Result<i64, fred::error::Error> {
        const LUA: &str = r"
            local n = redis.call('INCRBY', KEYS[1], ARGV[1])
            if n == tonumber(ARGV[1]) or redis.call('TTL', KEYS[1]) == -1 then
                redis.call('EXPIRE', KEYS[1], ARGV[2])
            end
            return n
        ";
        self.client
            .eval(
                LUA,
                vec![key.to_owned()],
                vec![delta.to_string(), ttl_secs.to_string()],
            )
            .await
    }

    /// 计数 +1 并把过期时间顺延到 `ttl_secs` 之后（滑动窗）。[`Self::incr_by_with_ttl`] 的窗口从第一次
    /// 起算、到期归零重数；「连续失败几次」要的是只要还在连着失败就不归零，隔了 `ttl_secs` 没再失败
    /// 才作废。INCR 与 EXPIRE 同一脚本原子完成，理由同 [`Self::incr_with_ttl`]。
    pub async fn incr_sliding(&self, key: &str, ttl_secs: i64) -> Result<i64, fred::error::Error> {
        const LUA: &str = r"
            local n = redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], ARGV[1])
            return n
        ";
        self.client
            .eval(LUA, vec![key.to_owned()], vec![ttl_secs.to_string()])
            .await
    }
}

/// 会话标识提取（§3.2）：优先客户端自报的会话 ID——Claude Code 的 `x-claude-code-session-id`
/// 头、请求体自报的会话（`body_session`：Anthropic 入口是 `metadata.user_id` 里的 session，
/// Responses 入口是 `prompt_cache_key`，Codex CLI 填的就是会话 id）、通用的 `session_id` /
/// `session-id`（Codex CLI 0.160 起）/ `x-session-id` 头（Nginx 需 `underscores_in_headers on`）；
/// 都没有才取首两条消息规范化文本哈希。内容哈希只是近似：开场相同的两个会话会撞在一起，压缩上下文后同一会话会变。
/// 哈希 = SHA-256 前 8 字节 hex（xxhash 为 M3 性能优化项）。
#[must_use]
pub fn session_hash(
    headers: &HeaderMap,
    body_session: Option<&str>,
    messages: &[okapi_api::MessageProbe],
) -> Option<String> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let explicit = header("x-claude-code-session-id")
        .or(body_session.map(str::trim).filter(|v| !v.is_empty()))
        .or_else(|| header("session_id"))
        .or_else(|| header("session-id"))
        .or_else(|| header("x-session-id"));
    if let Some(value) = explicit {
        return Some(short_hash(value.as_bytes()));
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
mod session_hash_tests {
    use super::session_hash;
    use axum::http::HeaderMap;
    use okapi_api::MessageProbe;

    fn messages(first: &str) -> Vec<MessageProbe> {
        vec![MessageProbe {
            role: "user".into(),
            content: serde_json::json!(first),
            tool_calls: serde_json::Value::Null,
        }]
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    /// 客户端自报的会话 ID 优先于内容哈希：开场相同的两个 Claude Code 会话分开粘，
    /// 同一会话压缩上下文（首条消息变了）后仍粘原账号。
    #[test]
    fn client_session_ids_take_precedence_over_content() {
        let none = HeaderMap::new();
        let same_opening = messages("hello");
        assert_eq!(
            session_hash(&none, None, &same_opening),
            session_hash(&none, None, &same_opening)
        );
        let a = headers(&[("x-claude-code-session-id", "session-a")]);
        let b = headers(&[("x-claude-code-session-id", "session-b")]);
        assert_ne!(
            session_hash(&a, None, &same_opening),
            session_hash(&b, None, &same_opening)
        );
        assert_eq!(
            session_hash(&a, None, &messages("hello")),
            session_hash(&a, None, &messages("summary after compaction"))
        );
        // 头与 metadata 指同一会话时键相同；头优先于 metadata，metadata 优先于通用头。
        assert_eq!(
            session_hash(&a, Some("other"), &same_opening),
            session_hash(&none, Some("session-a"), &same_opening)
        );
        let generic = headers(&[("session_id", "generic")]);
        assert_eq!(
            session_hash(&generic, Some("session-a"), &same_opening),
            session_hash(&a, None, &same_opening)
        );
        assert_eq!(
            session_hash(&generic, None, &same_opening),
            session_hash(
                &headers(&[("x-session-id", "generic")]),
                None,
                &same_opening
            )
        );
        // Codex CLI 0.160：连字符会话头，与请求体 prompt_cache_key 是同一个值，HTTP / WS 落同一键
        let codex = headers(&[("session-id", "thread-1")]);
        assert_eq!(
            session_hash(&codex, None, &same_opening),
            session_hash(&headers(&[("session_id", "thread-1")]), None, &same_opening)
        );
        assert_eq!(
            session_hash(&codex, None, &same_opening),
            session_hash(&none, Some("thread-1"), &messages("after compaction"))
        );
        let blank = headers(&[("x-claude-code-session-id", "  ")]);
        assert_eq!(
            session_hash(&blank, Some(" "), &same_opening),
            session_hash(&none, None, &same_opening),
            "空白 ID 回落到内容哈希"
        );
    }
}
