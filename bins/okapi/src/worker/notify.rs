//! 通知多路（IMPLEMENTATION #1790-8，M4）：worker 事件 → 多通道分发。
//!
//! 配置 `settings.notify_channels` = JSON 数组：
//! `[{"type":"webhook","url":"https://...","events":["drift","channel_cooldown","balance_low"],
//!    "min_interval_secs":300},
//!   {"type":"email","to":["ops@example.com"],"events":["drift"],"lang":"zh-CN"}]`
//! webhook 通道 POST JSON；email 通道走 `settings.smtp`（IMPLEMENTATION §11.27），
//! 未配置 SMTP 只打日志。
//! Redis `notify:mute:<idx>:<event>` 先认领短租约，成功才开始静默窗，失败释放；
//! Redis 故障放行。Webhook 可配置签名 secret，逐用户余额须 include_balances 显式开启。

use fred::clients::Client;
use fred::interfaces::{KeysInterface, LuaInterface};
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
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
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
            let claim = uuid::Uuid::new_v4().to_string();
            if !self.claim(idx, event, &claim).await {
                continue;
            }
            let payload = notification_payload(ch, event, payload);
            let at = chrono::Utc::now().to_rfc3339();
            if kind == "email" {
                let success = tokio::time::timeout(
                    Duration::from_mins(1),
                    self.send_email(ch, event, &at, &payload),
                )
                .await
                .unwrap_or(false);
                self.finish_claim(idx, event, &claim, interval, success)
                    .await;
                continue;
            }
            let Some(url) = ch.get("url").and_then(Value::as_str) else {
                self.finish_claim(idx, event, &claim, interval, false).await;
                continue;
            };
            if let Err(error) = crate::console::ssrf::validate_url(&self.pg, url).await {
                tracing::error!(event, channel=idx, code = %error.code, "notification URL rejected; configure ssrf_policy for private deployments");
                if self.mute_acquire(idx, "ssrf_rejected_audit", 86400).await {
                    let detail = serde_json::json!({"channel_index":idx,"event":event,"error_code":error.code,"reason":error.param});
                    if let Err(error) = okapi_store::admin::record_audit(
                        &self.pg,
                        "worker",
                        "notification.url_rejected",
                        &idx.to_string(),
                        detail,
                    )
                    .await
                    {
                        tracing::error!(%error,"blocked notification audit could not be persisted");
                    }
                }
                self.finish_claim(idx, event, &claim, interval, false).await;
                continue;
            }
            let body = serde_json::json!({"event":event,"at":at,"payload":payload}).to_string();
            let mut request = self
                .http
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .timeout(SEND_TIMEOUT);
            request = signed_webhook(request, ch, &at, &body);
            let success = match request.body(body).send().await {
                Ok(resp) if resp.status().is_success() => true,
                Ok(resp) => {
                    tracing::warn!(event,channel=idx,status=%resp.status(),"notification webhook rejected");
                    false
                }
                Err(_) => {
                    tracing::warn!(event, channel = idx, "notification webhook failed");
                    false
                }
            };
            self.finish_claim(idx, event, &claim, interval, success)
                .await;
        }
    }

    /// email 通道：`to` 数组逐个投递（一封一收件人，收件人互不可见）；
    /// `lang` 选模板语言（缺省 en）；SMTP 未配置只打一条日志。
    async fn send_email(&self, ch: &Value, event: &str, at: &str, payload: &Value) -> bool {
        let recipients: Vec<&str> = ch
            .get("to")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if recipients.is_empty() {
            return false;
        }
        let mailer = match crate::mail::Mailer::from_pg(&self.pg).await {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(event, error = %err, "email 通知通道跳过：SMTP 未配置");
                return false;
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
        let mut success = true;
        for to in recipients {
            let mail = crate::mail::templates::event_notice(lang, &site, to, event, at, payload);
            if let Err(err) = mailer.send(mail).await {
                success = false;
                tracing::warn!(event, to, error = %err, "通知邮件发送失败");
            }
        }
        success
    }

    async fn claim(&self, idx: usize, event: &str, owner: &str) -> bool {
        let key = format!("notify:mute:{idx}:{event}");
        let result: Result<Option<String>, _> = self
            .redis
            .set(
                key,
                owner,
                Some(Expiration::EX(120)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        result.map_or(true, |r| r.is_some())
    }
    async fn finish_claim(
        &self,
        idx: usize,
        event: &str,
        owner: &str,
        interval: i64,
        success: bool,
    ) {
        const LUA: &str = "if redis.call('GET',KEYS[1])~=ARGV[1] then return 0 end if ARGV[2]=='1' then redis.call('SET',KEYS[1],'sent','EX',ARGV[3]) else redis.call('DEL',KEYS[1]) end return 1";
        let _: Result<i64, _> = self
            .redis
            .eval(
                LUA,
                vec![format!("notify:mute:{idx}:{event}")],
                vec![
                    owner.to_owned(),
                    if success { "1" } else { "0" }.to_owned(),
                    interval.to_string(),
                ],
            )
            .await;
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

fn signed_webhook(
    mut request: reqwest::RequestBuilder,
    ch: &Value,
    at: &str,
    body: &str,
) -> reqwest::RequestBuilder {
    if let Some(secret) = ch
        .get("secret")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        use hmac::{Hmac, KeyInit, Mac};
        if let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) {
            mac.update(at.as_bytes());
            mac.update(b".");
            mac.update(body.as_bytes());
            request = request.header("x-okapi-timestamp", at).header(
                "x-okapi-signature",
                format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
            );
        }
    }
    request
}

fn notification_payload(ch: &Value, event: &str, payload: &Value) -> Value {
    let mut payload = payload.clone();
    if event == "balance_low"
        && ch.get("include_balances").and_then(Value::as_bool) != Some(true)
        && let Some(users) = payload.get_mut("users").and_then(Value::as_array_mut)
    {
        for user in users {
            if let Some(user) = user.as_object_mut() {
                user.remove("balance_micro");
            }
        }
    }
    payload
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

/// 扫一轮低余额并派发 `balance_low`；阈值关闭或无人低于阈值时不吵。
///
/// 载荷在这里拼而不是在 worker 主循环的 `select!` 臂里：拼在循环里的话，用例只能照抄一份
/// `json!` 自娱自乐，字段改坏了照样绿（`margin_breaker` 同理，见 `margin_breaker::evaluate_and_notify`）。
pub async fn balance_low_and_notify(pg: &PgPool, notifier: &Notifier) -> anyhow::Result<usize> {
    let low = scan_balance_low(pg).await?;
    if low.is_empty() {
        return Ok(0);
    }
    let users: Vec<Value> = low
        .iter()
        .map(|(id, bal)| serde_json::json!({ "user_id": id, "balance_micro": bal }))
        .collect();
    notifier
        .dispatch("balance_low", &serde_json::json!({ "users": users }))
        .await;
    Ok(low.len())
}

/// 数一轮冷却中的渠道 key 并派发 `channel_cooldown`；零冷却不吵。
pub async fn channel_cooldown_and_notify(pg: &PgPool, notifier: &Notifier) -> anyhow::Result<i64> {
    let cooling = count_cooling_keys(pg).await?;
    if cooling > 0 {
        notifier
            .dispatch("channel_cooldown", &serde_json::json!({ "count": cooling }))
            .await;
    }
    Ok(cooling)
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
