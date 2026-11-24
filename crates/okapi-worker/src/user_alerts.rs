//! 用户级主动告警评估与投递（个人低余额、API key 即将到期）。
//!
//! 每用户独立静默窗（Redis `notify:user:{uid}:{event}` `SET NX EX`），
//! 出站 Webhook 每次发送前均执行 `okapi_kernel::ssrf::validate_url` 且禁用重定向，
//! 邮件复用全站 `okapi_kernel::mail::Mailer`。
use chrono::{DateTime, Utc};
use fred::clients::Client;
use fred::interfaces::KeysInterface;
use fred::types::{Expiration, SetOptions};
use futures::StreamExt as _;
use okapi_kernel::mail::Mailer;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const EMAIL_SEND_TIMEOUT: Duration = Duration::from_secs(25);
const USER_ALERT_CONCURRENCY: usize = 16;

fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

/// 轮次内复用的邮件发送上下文，避免对最多 300 个用户重复查 PG `settings.smtp` 与 `site_name`。
struct EmailContext {
    mailer: Option<Mailer>,
    site_name: String,
}

impl EmailContext {
    async fn load(pg: &PgPool) -> Self {
        let mailer = Mailer::from_pg(pg).await.ok();
        let site_name = if mailer.is_some() {
            sqlx::query_scalar::<_, String>(
                "SELECT value #>> '{}' FROM settings WHERE key = 'site_name'",
            )
            .fetch_optional(pg)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Okapi".to_owned())
        } else {
            "Okapi".to_owned()
        };
        Self { mailer, site_name }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UserAlertReport {
    pub low_balance_sent: usize,
    pub key_expiry_sent: usize,
}

#[derive(sqlx::FromRow)]
struct LowBalanceRow {
    user_id: i64,
    balance_micro: i64,
    threshold_micro: i64,
    email: Option<String>,
    language: String,
    email_enabled: bool,
    webhook_url: Option<String>,
    webhook_secret: Option<String>,
    min_interval_secs: i32,
}

#[derive(sqlx::FromRow)]
struct ExpiringKeyRow {
    user_id: i64,
    key_id: i64,
    key_name: String,
    key_prefix: String,
    expires_at: DateTime<Utc>,
    warn_days: i32,
    email: Option<String>,
    language: String,
    email_enabled: bool,
    webhook_url: Option<String>,
    webhook_secret: Option<String>,
    min_interval_secs: i32,
}

struct Target<'a> {
    user_id: i64,
    email: Option<&'a str>,
    language: &'a str,
    email_enabled: bool,
    webhook_url: Option<&'a str>,
    webhook_secret: Option<&'a str>,
    min_interval_secs: i32,
}

/// 扫描一轮用户低余额与即将到期的 API key，并按用户偏好有界并发派发通知。
pub async fn evaluate_and_notify(pg: &PgPool, redis: &Client) -> anyhow::Result<UserAlertReport> {
    let http = shared_http_client();
    let email_ctx = tokio::sync::OnceCell::new();
    let low_balance_sent = eval_low_balance(pg, redis, http, &email_ctx).await?;
    let key_expiry_sent = eval_expiring_keys(pg, redis, http, &email_ctx).await?;
    Ok(UserAlertReport {
        low_balance_sent,
        key_expiry_sent,
    })
}

async fn eval_low_balance(
    pg: &PgPool,
    redis: &Client,
    http: &reqwest::Client,
    email_ctx: &tokio::sync::OnceCell<EmailContext>,
) -> anyhow::Result<usize> {
    let rows = sqlx::query_as::<_, LowBalanceRow>(
        r"SELECT u.id AS user_id,
                 u.balance_micro,
                 p.low_balance_threshold_micro AS threshold_micro,
                 u.email,
                 u.language,
                 p.email_enabled,
                 p.webhook_url,
                 p.webhook_secret,
                 p.min_interval_secs
          FROM user_alert_prefs p
          JOIN users u ON u.id = p.user_id
          WHERE u.deleted_at IS NULL AND u.status = 1
            AND p.low_balance_threshold_micro IS NOT NULL
            AND u.balance_micro <= p.low_balance_threshold_micro
            AND (p.email_enabled OR p.webhook_url IS NOT NULL)
          ORDER BY u.balance_micro ASC, u.id ASC
          LIMIT 100",
    )
    .fetch_all(pg)
    .await?;

    let sent = futures::stream::iter(rows)
        .map(|r| async move {
            let target = Target {
                user_id: r.user_id,
                email: r.email.as_deref(),
                language: &r.language,
                email_enabled: r.email_enabled,
                webhook_url: r.webhook_url.as_deref(),
                webhook_secret: r.webhook_secret.as_deref(),
                min_interval_secs: r.min_interval_secs,
            };
            let payload = json!({
                "user_id": r.user_id,
                "balance_micro": r.balance_micro,
                "threshold_micro": r.threshold_micro,
            });
            usize::from(
                deliver_once(
                    pg,
                    redis,
                    http,
                    email_ctx,
                    &target,
                    "user_balance_low",
                    &payload,
                )
                .await,
            )
        })
        .buffer_unordered(USER_ALERT_CONCURRENCY)
        .fold(0usize, |acc, n| async move { acc + n })
        .await;
    Ok(sent)
}

async fn eval_expiring_keys(
    pg: &PgPool,
    redis: &Client,
    http: &reqwest::Client,
    email_ctx: &tokio::sync::OnceCell<EmailContext>,
) -> anyhow::Result<usize> {
    let rows = sqlx::query_as::<_, ExpiringKeyRow>(
        r"SELECT k.user_id,
                 k.id AS key_id,
                 k.name AS key_name,
                 k.key_prefix,
                 k.expires_at,
                 p.key_expiry_warn_days AS warn_days,
                 u.email,
                 u.language,
                 p.email_enabled,
                 p.webhook_url,
                 p.webhook_secret,
                 p.min_interval_secs
          FROM api_keys k
          JOIN user_alert_prefs p ON p.user_id = k.user_id
          JOIN users u ON u.id = k.user_id
          WHERE u.deleted_at IS NULL AND u.status = 1
            AND k.deleted_at IS NULL AND k.status = 1 AND k.session_hash IS NULL
            AND p.key_expiry_warn_days IS NOT NULL
            AND k.expires_at IS NOT NULL
            AND k.expires_at > now()
            AND k.expires_at <= now() + make_interval(days => p.key_expiry_warn_days)
            AND (p.email_enabled OR p.webhook_url IS NOT NULL)
          ORDER BY k.expires_at ASC, k.id ASC
          LIMIT 200",
    )
    .fetch_all(pg)
    .await?;

    let mut grouped: BTreeMap<i64, Vec<ExpiringKeyRow>> = BTreeMap::new();
    for row in rows {
        grouped.entry(row.user_id).or_default().push(row);
    }

    let sent = futures::stream::iter(grouped)
        .map(|(user_id, items)| async move {
            let Some(first) = items.first() else {
                return 0usize;
            };
            let target = Target {
                user_id,
                email: first.email.as_deref(),
                language: &first.language,
                email_enabled: first.email_enabled,
                webhook_url: first.webhook_url.as_deref(),
                webhook_secret: first.webhook_secret.as_deref(),
                min_interval_secs: first.min_interval_secs,
            };
            let keys: Vec<Value> = items
                .iter()
                .map(|k| {
                    json!({
                        "key_id": k.key_id,
                        "name": k.key_name,
                        "key_prefix": k.key_prefix,
                        "expires_at": k.expires_at.to_rfc3339(),
                    })
                })
                .collect();
            let payload = json!({
                "user_id": user_id,
                "warn_days": first.warn_days,
                "keys": keys,
            });
            usize::from(
                deliver_once(
                    pg,
                    redis,
                    http,
                    email_ctx,
                    &target,
                    "user_key_expiring",
                    &payload,
                )
                .await,
            )
        })
        .buffer_unordered(USER_ALERT_CONCURRENCY)
        .fold(0usize, |acc, n| async move { acc + n })
        .await;
    Ok(sent)
}

async fn deliver_once(
    pg: &PgPool,
    redis: &Client,
    http: &reqwest::Client,
    email_ctx: &tokio::sync::OnceCell<EmailContext>,
    target: &Target<'_>,
    event: &str,
    payload: &Value,
) -> bool {
    let mute_key = format!("notify:user:{}:{event}", target.user_id);
    let interval = i64::from(target.min_interval_secs.max(60));
    let claimed: Option<String> = redis
        .set(
            &mute_key,
            "1",
            Some(Expiration::EX(interval)),
            Some(SetOptions::NX),
            false,
        )
        .await
        .ok()
        .flatten();
    if claimed.is_none() {
        return false;
    }

    let at = Utc::now().to_rfc3339();
    let webhook_fut = async {
        match target.webhook_url.filter(|u| !u.is_empty()) {
            Some(url) => {
                send_webhook(pg, http, url, target.webhook_secret, event, &at, payload).await
            }
            None => false,
        }
    };
    let email_fut = async {
        if target.email_enabled
            && let Some(email) = target.email.filter(|e| !e.trim().is_empty())
        {
            let ctx = email_ctx.get_or_init(|| EmailContext::load(pg)).await;
            send_email(ctx, email, target.language, event, &at, payload).await
        } else {
            false
        }
    };
    let (webhook_ok, email_ok) = tokio::join!(webhook_fut, email_fut);
    let delivered = webhook_ok || email_ok;

    if !delivered {
        let _: Result<i64, _> = redis.del(&mute_key).await;
    }
    delivered
}

async fn send_webhook(
    pg: &PgPool,
    http: &reqwest::Client,
    url: &str,
    secret: Option<&str>,
    event: &str,
    at: &str,
    payload: &Value,
) -> bool {
    if let Err(err) = okapi_kernel::ssrf::validate_url(pg, url).await {
        tracing::warn!(event, code = err.code, "user alert webhook blocked by ssrf");
        return false;
    }
    let body = json!({ "event": event, "at": at, "payload": payload }).to_string();
    let mut req = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(SEND_TIMEOUT);
    if let Some(secret) = secret.filter(|s| !s.is_empty()) {
        use hmac::{Hmac, KeyInit, Mac};
        if let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) {
            mac.update(at.as_bytes());
            mac.update(b".");
            mac.update(body.as_bytes());
            req = req.header("x-okapi-timestamp", at).header(
                "x-okapi-signature",
                format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
            );
        }
    }
    match req.body(body).send().await {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            tracing::warn!(event, status = %resp.status(), "user alert webhook rejected");
            false
        }
        Err(err) => {
            tracing::warn!(event, error = %err, "user alert webhook failed");
            false
        }
    }
}

async fn send_email(
    ctx: &EmailContext,
    to: &str,
    language: &str,
    event: &str,
    at: &str,
    payload: &Value,
) -> bool {
    let Some(mailer) = ctx.mailer.as_ref() else {
        return false;
    };
    let lang = okapi_kernel::mail::templates::Lang::resolve(Some(language), None);
    let mail = okapi_kernel::mail::templates::event_notice(
        lang,
        &ctx.site_name,
        to,
        event,
        at,
        payload,
    );
    match tokio::time::timeout(EMAIL_SEND_TIMEOUT, mailer.send(mail)).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            tracing::warn!(event, to, error = %err, "user alert email failed");
            false
        }
        Err(_) => {
            tracing::warn!(event, to, "user alert email timed out");
            false
        }
    }
}
