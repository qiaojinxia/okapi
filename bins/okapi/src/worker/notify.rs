//! 通知多路（IMPLEMENTATION #1790-8，M4）：worker 事件 → 多通道分发。
//!
//! 配置 `settings.notify_channels` = JSON 数组：
//! `[{"type":"webhook","url":"https://...","events":["drift","channel_cooldown","balance_low"],
//!    "min_interval_secs":300},
//!   {"type":"email","to":["ops@example.com"],"events":["drift"],"lang":"zh-CN"}]`
//! webhook 通道 POST JSON；email 通道走 `settings.smtp`（IMPLEMENTATION §11.27），
//! 未配置 SMTP 只打日志。
//! 频率限制：Redis `notify:mute:<idx>:<event>` SET NX EX——静默期内同事件跳过；
//! Redis 故障放行（通知丢失可容忍，不阻 worker 主循环）。发送失败仅日志。

use fred::clients::Client;
use fred::interfaces::KeysInterface;
use fred::types::{Expiration, SetOptions};
use serde_json::Value;
use sqlx::PgPool;
use std::time::Duration;

const DEFAULT_MIN_INTERVAL_SECS: i64 = 300;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Notifier {
    pg: PgPool,
    redis: Client,
    http: reqwest::Client,
}

impl Notifier {
    #[must_use]
    pub fn new(pg: PgPool, redis: Client) -> Self {
        Self {
            pg,
            redis,
            http: reqwest::Client::new(),
        }
    }

    /// 事件分发：读通道配置 → 订阅过滤 → 频率闸 → webhook POST。
    pub async fn dispatch(&self, event: &str, payload: &Value) {
        let channels =
            sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'notify_channels'"#)
                .fetch_optional(&self.pg)
                .await
                .ok()
                .flatten();
        let Some(Value::Array(channels)) = channels else {
            return;
        };
        for (idx, ch) in channels.iter().enumerate() {
            let kind = ch.get("type").and_then(Value::as_str).unwrap_or("");
            if kind != "webhook" && kind != "email" {
                continue;
            }
            let subscribed = ch
                .get("events")
                .and_then(Value::as_array)
                .is_some_and(|evs| evs.iter().any(|e| e.as_str() == Some(event)));
            if !subscribed {
                continue;
            }
            let interval = ch
                .get("min_interval_secs")
                .and_then(Value::as_i64)
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_MIN_INTERVAL_SECS);
            if !self.mute_acquire(idx, event, interval).await {
                continue;
            }
            let at = chrono::Utc::now().to_rfc3339();
            if kind == "email" {
                self.send_email(ch, event, &at, payload).await;
                continue;
            }
            let Some(url) = ch.get("url").and_then(Value::as_str) else {
                continue;
            };
            let body = serde_json::json!({
                "event": event,
                "at": at,
                "payload": payload,
            });
            let result = self
                .http
                .post(url)
                .json(&body)
                .timeout(SEND_TIMEOUT)
                .send()
                .await;
            match result {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => {
                    tracing::warn!(event, url, status = %resp.status(), "通知 webhook 非 2xx");
                }
                Err(err) => tracing::warn!(event, url, error = %err, "通知 webhook 发送失败"),
            }
        }
    }

    /// email 通道：`to` 数组逐个投递（一封一收件人，收件人互不可见）；
    /// `lang` 选模板语言（缺省 en）；SMTP 未配置只打一条日志。
    async fn send_email(&self, ch: &Value, event: &str, at: &str, payload: &Value) {
        let recipients: Vec<&str> = ch
            .get("to")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if recipients.is_empty() {
            return;
        }
        let mailer = match crate::mail::Mailer::from_pg(&self.pg).await {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(event, error = %err, "email 通知通道跳过：SMTP 未配置");
                return;
            }
        };
        let lang =
            crate::mail::templates::Lang::resolve(ch.get("lang").and_then(Value::as_str), None);
        let site = sqlx::query_scalar!(
            r#"SELECT value #>> '{}' AS "v!" FROM settings WHERE key = 'site_name'"#
        )
        .fetch_optional(&self.pg)
        .await
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Okapi".to_owned());
        for to in recipients {
            let mail = crate::mail::templates::event_notice(lang, &site, to, event, at, payload);
            if let Err(err) = mailer.send(mail).await {
                tracing::warn!(event, to, error = %err, "通知邮件发送失败");
            }
        }
    }

    /// 频率闸：NX 抢占成功 = 允许发送；静默期内返回 false。Redis 故障放行。
    async fn mute_acquire(&self, idx: usize, event: &str, interval_secs: i64) -> bool {
        let key = format!("notify:mute:{idx}:{event}");
        let set: Result<Option<String>, _> = self
            .redis
            .set(
                &key,
                "1",
                Some(Expiration::EX(interval_secs)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        // NX 抢到 = Ok(Some("OK"))；静默期内（键已存在）= Ok(None)
        match set {
            Ok(reply) => reply.is_some(),
            Err(_) => true,
        }
    }
}

/// 余额低于阈值的用户扫描（settings.balance_low_threshold_micro，缺省 0=关闭）。
/// 返回 (user_id, balance_micro) 列表（上限 20，防 payload 膨胀）；
/// 按余额降序：优先展示尚有余额但将耗尽的用户（0 余额沉睡用户排后）。
/// 同额再按 id 降序兜底排序：没有 tie-break 时 `LIMIT 20` 从同额用户里取谁由 PG 自由决定，
/// 同一批数据两次轮询能给出不同的 20 个人，通知收件人随机漂移。
pub async fn scan_balance_low(pg: &PgPool) -> anyhow::Result<Vec<(i64, i64)>> {
    let threshold = sqlx::query_scalar!(
        r#"SELECT (value #>> '{}')::bigint AS "v!" FROM settings
           WHERE key = 'balance_low_threshold_micro'"#
    )
    .fetch_optional(pg)
    .await?
    .unwrap_or(0);
    if threshold <= 0 {
        return Ok(Vec::new());
    }
    let rows = sqlx::query!(
        r#"SELECT id, balance_micro FROM users
           WHERE deleted_at IS NULL AND balance_micro < $1 AND balance_micro >= 0
           ORDER BY balance_micro DESC, id DESC LIMIT 20"#,
        threshold
    )
    .fetch_all(pg)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.balance_micro)).collect())
}

/// 当前处于冷却/受限状态的渠道 key 数（channel_cooldown 事件源）。
pub async fn count_cooling_keys(pg: &PgPool) -> anyhow::Result<i64> {
    let n = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM channel_keys WHERE status IN (2, 3, 4)"#
    )
    .fetch_one(pg)
    .await?;
    Ok(n)
}
