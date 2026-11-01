//! 告警日志：tracing 层把 WARN / ERROR 收进进程内环形缓冲，后台每 2 秒批量写进
//! Redis 列表 `ops:logs`（保留最近 2000 条，7 天无新告警整键过期）。多进程 / 多副本部署时所有角色的告警汇到
//! 同一处，面板读 Redis；Redis 不可达时回退到本进程缓冲。
//!
//! ERROR 另存一个列表 `ops:logs:error`（保留 500 条），读取时与 WARN 合并：大量 WARN 刷屏时
//! 关键 ERROR 不会被挤出面板。
//!
//! 写 Redis 失败只记 debug：本层只收 WARN 以上，失败日志不会再回到自己身上形成回路。
//! 连续写不进时改向 stderr 打一行（stderr 不经 tracing、也不进本层），部署侧的标准输出采集能看到
//! 「告警面本身失明」。

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

pub const REDIS_KEY: &str = "ops:logs";
const REDIS_KEEP: i64 = 2000;
/// ERROR 单独保底：与 WARN 分开存，读取时合并。
pub const REDIS_ERROR_KEY: &str = "ops:logs:error";
const REDIS_ERROR_KEEP: i64 = 500;
/// 连续这么多次写 Redis 失败（约 20 秒）就向 stderr 报一次。
const FLUSH_FAILURES_BEFORE_STDERR: u32 = 10;
/// 每次写入续期：持续有告警时滚动保留，全站静默 7 天后整列表过期，旧条目不永久驻留。
const REDIS_TTL_SECS: i64 = 7 * 24 * 3600;
const LOCAL_KEEP: usize = 500;
const LOCAL_ERROR_KEEP: usize = 200;
/// 待写 Redis 的上限：Redis 长时间不可达时丢最旧的，不让内存无界增长。
const PENDING_KEEP: usize = 2000;
const MESSAGE_MAX: usize = 2000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub ts: String,
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub role: String,
}

#[derive(Default)]
struct Buffers {
    recent: VecDeque<Entry>,
    /// 本进程最近的 ERROR 另留一份，WARN 刷屏时回退面板也看得到
    recent_errors: VecDeque<Entry>,
    pending: VecDeque<Entry>,
}

fn is_error(entry: &Entry) -> bool {
    entry.level == "ERROR"
}

/// 待写队列超限时先丢最旧的 WARN，全是 ERROR 才丢最旧的 ERROR。
fn trim_pending(pending: &mut VecDeque<Entry>) {
    while pending.len() > PENDING_KEEP {
        match pending.iter().position(|e| !is_error(e)) {
            Some(index) => {
                pending.remove(index);
            }
            None => {
                pending.pop_front();
            }
        }
    }
}

static BUFFERS: OnceLock<Mutex<Buffers>> = OnceLock::new();
static ORIGIN: OnceLock<(String, String)> = OnceLock::new();

fn buffers() -> &'static Mutex<Buffers> {
    BUFFERS.get_or_init(Mutex::default)
}

fn push(entry: Entry) {
    let Ok(mut b) = buffers().lock() else {
        return;
    };
    if is_error(&entry) {
        if b.recent_errors.len() >= LOCAL_ERROR_KEEP {
            b.recent_errors.pop_front();
        }
        b.recent_errors.push_back(entry.clone());
    } else {
        if b.recent.len() >= LOCAL_KEEP {
            b.recent.pop_front();
        }
        b.recent.push_back(entry.clone());
    }
    b.pending.push_back(entry);
    trim_pending(&mut b.pending);
}

/// 写失败的一批放回待写队列头部（它们比失败期间新进来的更旧），总量仍按 [`PENDING_KEEP`] 丢最旧的。
fn requeue(batch: Vec<Entry>) {
    let Ok(mut b) = buffers().lock() else {
        return;
    };
    for entry in batch.into_iter().rev() {
        b.pending.push_front(entry);
    }
    trim_pending(&mut b.pending);
}

/// 本进程最近的告警（新的在前）。
#[must_use]
pub fn recent() -> Vec<Entry> {
    buffers()
        .lock()
        .map(|b| {
            merge_newest_first(
                b.recent.iter().rev().cloned().collect(),
                b.recent_errors.iter().rev().cloned().collect(),
            )
        })
        .unwrap_or_default()
}

/// 两列都按新的在前，合并成一列（时间戳是同格式的 RFC 3339 UTC，可直接按字符串比较）。
fn merge_newest_first(warnings: Vec<Entry>, errors: Vec<Entry>) -> Vec<Entry> {
    let mut merged = Vec::with_capacity(warnings.len() + errors.len());
    let (mut w, mut e) = (
        warnings.into_iter().peekable(),
        errors.into_iter().peekable(),
    );
    loop {
        let take_error = match (w.peek(), e.peek()) {
            (Some(warn), Some(error)) => error.ts >= warn.ts,
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => break,
        };
        merged.extend(if take_error { e.next() } else { w.next() });
    }
    merged
}

fn take_pending() -> Vec<Entry> {
    buffers()
        .lock()
        .map(|mut b| b.pending.drain(..).collect())
        .unwrap_or_default()
}

#[derive(Default)]
struct Message {
    text: String,
    fields: String,
}

/// 凭证类字段名：值不进 `ops:logs`。这里的告警会写进共享 Redis、经控制台按 `settings.read` 读出，
/// 比 stdout 的受众宽得多——例如单用户模式首启的 root key 是 WARN 级、带 `api_key` 字段打印的。
fn sensitive(name: &str) -> bool {
    matches!(
        name,
        "api_key" | "token" | "secret" | "password" | "credential" | "authorization" | "cookie"
    ) || ["_token", "_secret", "_password", "_credential", "_api_key"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.text, "{value:?}");
        } else if sensitive(field.name()) {
            let _ = write!(self.fields, " {}=[redacted]", field.name());
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.text.push_str(value);
        } else if sensitive(field.name()) {
            let _ = write!(self.fields, " {}=[redacted]", field.name());
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

/// 只收 WARN / ERROR 的 tracing 层。
pub struct OpsLogLayer;

impl<S: Subscriber> Layer<S> for OpsLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::WARN {
            return;
        }
        let mut msg = Message::default();
        event.record(&mut msg);
        msg.text.push_str(&msg.fields);
        // 换行与控制字符转义：上游错误原文、用户填的邮箱都可能带 `\n`，面板按行渲染，
        // 不转义的话一条 WARN 能伪造出一行假的 ERROR
        let mut message = crate::text::escape_for_log(&msg.text).into_owned();
        if message.len() > MESSAGE_MAX {
            let cut = (0..=MESSAGE_MAX)
                .rev()
                .find(|i| message.is_char_boundary(*i))
                .unwrap_or(0);
            message.truncate(cut);
            message.push('…');
        }
        let (node, role) = ORIGIN.get().cloned().unwrap_or_default();
        push(Entry {
            ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            level: level.as_str().to_owned(),
            target: event.metadata().target().to_owned(),
            message,
            node,
            role,
        });
    }
}

/// 启动写 Redis 的后台任务（每进程一次；`all` 模式三个角色共用）。
pub fn spawn_flusher(redis: fred::clients::Client, node: &str, role: &str) {
    if ORIGIN.set((node.to_owned(), role.to_owned())).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failures = 0u32;
        loop {
            tick.tick().await;
            super::guarded("ops_logbuf", async {
                let batch = take_pending();
                if batch.is_empty() {
                    return;
                }
                if let Err(e) = write(&redis, &batch).await {
                    // Redis 故障正是最需要告警的时候：整批放回待写队列，恢复后补写，不随这一次失败丢掉
                    tracing::debug!(error = %e, requeued = batch.len(), "ops log flush failed");
                    requeue(batch);
                    failures = failures.saturating_add(1);
                    if failures == FLUSH_FAILURES_BEFORE_STDERR {
                        eprintln!(
                            "{} WARN okapi::ops::logbuf: ops log flush to Redis failing ({failures} attempts); alerts are buffered locally only: {e}",
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                        );
                    }
                } else {
                    if failures >= FLUSH_FAILURES_BEFORE_STDERR {
                        eprintln!(
                            "{} INFO okapi::ops::logbuf: ops log flush to Redis recovered after {failures} failed attempts",
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                        );
                    }
                    failures = 0;
                }
            })
            .await;
        }
    });
}

async fn write(redis: &fred::clients::Client, batch: &[Entry]) -> anyhow::Result<()> {
    use fred::interfaces::{KeysInterface, ListInterface};
    let encode = |errors: bool| -> Vec<String> {
        batch
            .iter()
            .filter(|e| is_error(e) == errors)
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect()
    };
    let pipe = redis.pipeline();
    for (key, keep, values) in [
        (REDIS_KEY, REDIS_KEEP, encode(false)),
        (REDIS_ERROR_KEY, REDIS_ERROR_KEEP, encode(true)),
    ] {
        if values.is_empty() {
            continue;
        }
        let _: () = pipe.lpush(key, values).await?;
        let _: () = pipe.ltrim(key, 0, keep - 1).await?;
        let _: () = pipe.expire(key, REDIS_TTL_SECS, None).await?;
    }
    let _: Vec<fred::types::Value> = pipe.all().await?;
    Ok(())
}

/// 读汇总的告警（新的在前）。`Err` = Redis 不可达，调用方回退到 [`recent`]。
pub async fn load(redis: &fred::clients::Client) -> anyhow::Result<Vec<Entry>> {
    use fred::interfaces::ListInterface;
    let decode = |raw: Vec<String>| -> Vec<Entry> {
        raw.iter()
            .filter_map(|r| serde_json::from_str(r).ok())
            .collect()
    };
    let warnings: Vec<String> = redis.lrange(REDIS_KEY, 0, REDIS_KEEP - 1).await?;
    let errors: Vec<String> = redis
        .lrange(REDIS_ERROR_KEY, 0, REDIS_ERROR_KEEP - 1)
        .await?;
    Ok(merge_newest_first(decode(warnings), decode(errors)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn captures_warn_and_error_with_fields_only() {
        let subscriber = tracing_subscriber::registry().with(OpsLogLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("ops-logbuf-test info");
            tracing::warn!(channel = 7, "ops-logbuf-test warn");
            tracing::error!(error = %"boom", "ops-logbuf-test error");
        });
        let mine: Vec<Entry> = recent()
            .into_iter()
            .filter(|e| e.message.starts_with("ops-logbuf-test"))
            .collect();
        assert_eq!(mine.len(), 2, "{mine:?}");
        assert_eq!(mine[0].level, "ERROR");
        assert_eq!(mine[0].message, "ops-logbuf-test error error=boom");
        assert_eq!(mine[1].message, "ops-logbuf-test warn channel=7");
    }

    #[test]
    fn credential_fields_never_reach_the_shared_buffer() {
        let subscriber = tracing_subscriber::registry().with(OpsLogLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(api_key = %"sk-okapi-secret", tokens = 12, "ops-redact-test");
        });
        let entry = recent()
            .into_iter()
            .find(|e| e.message.starts_with("ops-redact-test"))
            .unwrap();
        assert_eq!(
            entry.message,
            "ops-redact-test api_key=[redacted] tokens=12"
        );
    }

    /// 一条告警只占面板的一行：消息与字段里的换行、终端转义都转义掉，伪造不出假的下一行。
    #[test]
    fn captured_lines_cannot_forge_extra_rows() {
        let subscriber = tracing_subscriber::registry().with(OpsLogLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                to = %"x@a.test\nERROR forged node=gw-1",
                error = %"boom\u{1b}[2J",
                "ops-forge-test\nERROR fake"
            );
        });
        let entry = recent()
            .into_iter()
            .find(|e| e.message.starts_with("ops-forge-test"))
            .unwrap();
        assert!(!entry.message.contains('\n') && !entry.message.contains('\u{1b}'));
        assert_eq!(
            entry.message,
            "ops-forge-test\\nERROR fake to=x@a.test\\nERROR forged node=gw-1 error=boom\\u{1b}[2J"
        );
    }

    /// 待写队列满时先丢 WARN：Redis 长时间不可达、WARN 刷屏也不会把排队中的 ERROR 挤掉。
    #[test]
    fn pending_overflow_drops_warnings_before_errors() {
        let entry = |level: &str, n: usize| Entry {
            ts: format!("{n:06}"),
            level: level.into(),
            target: String::new(),
            message: format!("{level}-{n}"),
            node: String::new(),
            role: String::new(),
        };
        let mut pending: VecDeque<Entry> = VecDeque::new();
        pending.push_back(entry("ERROR", 0));
        for n in 1..=PENDING_KEEP {
            pending.push_back(entry("WARN", n));
        }
        trim_pending(&mut pending);
        assert_eq!(pending.len(), PENDING_KEEP);
        assert_eq!(pending.front().unwrap().message, "ERROR-0");
        assert_eq!(pending[1].message, "WARN-2");
    }

    #[test]
    fn merged_view_interleaves_errors_by_time() {
        let entry = |level: &str, ts: &str| Entry {
            ts: ts.into(),
            level: level.into(),
            target: String::new(),
            message: String::new(),
            node: String::new(),
            role: String::new(),
        };
        let merged = merge_newest_first(
            vec![
                entry("WARN", "2026-10-11T00:00:03.000Z"),
                entry("WARN", "2026-10-11T00:00:01.000Z"),
            ],
            vec![entry("ERROR", "2026-10-11T00:00:02.000Z")],
        );
        let order: Vec<_> = merged
            .iter()
            .map(|e| (e.level.as_str(), e.ts.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("WARN", "2026-10-11T00:00:03.000Z"),
                ("ERROR", "2026-10-11T00:00:02.000Z"),
                ("WARN", "2026-10-11T00:00:01.000Z")
            ]
        );
    }

    #[test]
    fn failed_flush_is_requeued_ahead_of_newer_entries() {
        let entry = |message: &str| Entry {
            ts: String::new(),
            level: "WARN".into(),
            target: String::new(),
            message: message.into(),
            node: String::new(),
            role: String::new(),
        };
        let mine = |batch: Vec<Entry>| -> Vec<String> {
            batch
                .into_iter()
                .map(|e| e.message)
                .filter(|m| m.starts_with("ops-requeue-"))
                .collect()
        };
        push(entry("ops-requeue-a"));
        push(entry("ops-requeue-b"));
        let failed = take_pending();
        push(entry("ops-requeue-c"));
        requeue(failed);
        assert_eq!(
            mine(take_pending()),
            ["ops-requeue-a", "ops-requeue-b", "ops-requeue-c"]
        );
    }
}
