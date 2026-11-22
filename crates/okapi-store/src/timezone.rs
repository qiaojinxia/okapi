//! Use the process/host timezone, independently of database container defaults.
use crate::StoreError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

fn normalize(hint: &str) -> Option<String> {
    let hint = hint.trim().trim_start_matches(':');
    let hint = match hint.split_once("zoneinfo/") {
        Some((_, zone)) => zone,
        None if hint.starts_with('/') => return None,
        None => hint,
    };
    let hint = hint.strip_prefix("posix/").unwrap_or(hint);
    let zone = match hint {
        "UTC0" | "GMT0" => "UTC",
        other => other,
    };
    (!zone.is_empty()
        && !zone.split('/').any(|part| matches!(part, "." | ".."))
        && zone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+')))
    .then(|| zone.to_owned())
}

fn copied_timezone(local: &[u8], root: &std::path::Path) -> Option<String> {
    // zone.tab omits UTC and many aliases; check those before the geographic index.
    for zone in ["Etc/UTC", "UTC", "Etc/GMT", "GMT"] {
        if std::fs::read(root.join(zone)).is_ok_and(|bytes| bytes == local) {
            return Some(zone.to_owned());
        }
    }
    for index in ["zone1970.tab", "zone.tab"] {
        let Ok(index) = std::fs::read_to_string(root.join(index)) else {
            continue;
        };
        for zone in index
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_whitespace().nth(2))
            .filter_map(normalize)
        {
            if std::fs::read(root.join(&zone)).is_ok_and(|bytes| bytes == local) {
                return Some(zone);
            }
        }
    }
    None
}

pub fn machine_timezone() -> Result<&'static str, StoreError> {
    static ZONE: OnceLock<Option<String>> = OnceLock::new();
    ZONE.get_or_init(|| {
        let configured=std::env::var("TZ").ok().filter(|s| !s.is_empty());
        let linked=std::fs::canonicalize("/etc/localtime").ok()
            .and_then(|p| p.to_str().and_then(normalize));
        let named=std::fs::read_to_string("/etc/timezone").ok().and_then(|s|normalize(&s));
        let local=std::fs::read("/etc/localtime").ok();
        let zone=configured.as_deref().and_then(normalize).or(linked).or(named)
            .or_else(|| local.as_deref().and_then(|bytes| copied_timezone(bytes,std::path::Path::new("/usr/share/zoneinfo"))))
            .or_else(|| (configured.is_none() && local.is_none() && chrono::Local::now().offset().local_minus_utc()==0)
                .then(|| "UTC".to_owned()));
        if let Some(zone)=&zone {
            tracing::info!(timezone=%zone,"machine timezone resolved");
        } else {
            tracing::error!("cannot resolve machine timezone; install tzdata or set TZ to the host IANA timezone");
        }
        zone
    }).as_deref().ok_or(StoreError::InvalidData("machine_timezone_unavailable"))
}

/// 统计日历时区的设置键。值是 IANA 名字符串；null / 空串 = 跟随服务器时区（[`machine_timezone`]）。
///
/// 它决定「一天」从哪里切：ClickHouse 查询的会话时区（按天 / 按小时分桶、`today()`）、PG 连接的会话时区
/// （「今日新增」「今日收支」）、渠道用量窗口。写入（CH 插入、建表、迁移）始终是 UTC，存储不受影响；
/// 计费路径（冷却、额度锚点）本来就显式按 UTC 算，也不受影响。
pub const SETTING: &str = "reporting_timezone";

static OVERRIDE: RwLock<Option<&'static str>> = RwLock::new(None);
static INTERNED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
static EPOCH: OnceLock<Instant> = OnceLock::new();
/// 最近一次换时区距 [`EPOCH`] 的纳秒数；0 = 进程内从没换过。
static CHANGED_NANOS: AtomicU64 = AtomicU64::new(0);

/// 统计口径的日历时区：后台设置了就用设置，否则跟随服务器。
pub fn reporting_timezone() -> Result<&'static str, StoreError> {
    if let Some(zone) = *OVERRIDE.read().unwrap_or_else(PoisonError::into_inner) {
        return Ok(zone);
    }
    machine_timezone()
}

/// 设置值 → 时区名。null / 空串 = 跟随服务器（`Ok(None)`）；字符串必须原样就是规范的 IANA 名字
/// （不收 `posix/` 前缀、`UTC0` 这类别名和路径）。这里只校验写法，数据库认不认由写入方另查。
#[allow(clippy::result_unit_err)]
pub fn parse_setting(value: &serde_json::Value) -> Result<Option<String>, ()> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(raw) if raw.trim().is_empty() => Ok(None),
        serde_json::Value::String(raw) => normalize(raw)
            .filter(|zone| zone == raw && zone.len() <= 64)
            .map(Some)
            .ok_or(()),
        _ => Err(()),
    }
}

fn intern(zone: &str) -> &'static str {
    let mut interned = INTERNED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(found) = interned.iter().find(|known| **known == zone) {
        return found;
    }
    // 每个不同的时区名只泄漏一次：取值来自后台设置，总量就是用过的时区个数。
    let leaked: &'static str = Box::leak(zone.to_owned().into_boxed_str());
    interned.push(leaked);
    leaked
}

fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

fn nanos_since_epoch(at: Instant) -> u64 {
    u64::try_from(at.saturating_duration_since(epoch()).as_nanos()).unwrap_or(u64::MAX)
}

/// 换统计时区（进程内全局，`None` = 跟随服务器）；返回是否真的变了。变了就记下时刻：
/// 之前建立的 PG 连接会话时区还是旧的，下次取用时丢掉重连（见 [`opened_before_change`]）。
pub fn apply_reporting_timezone(zone: Option<&str>) -> bool {
    let next = zone.map(intern);
    let mut current = OVERRIDE.write().unwrap_or_else(PoisonError::into_inner);
    if *current == next {
        return false;
    }
    *current = next;
    CHANGED_NANOS.store(nanos_since_epoch(Instant::now()).max(1), Ordering::SeqCst);
    true
}

/// PG 连接（已存在 `age` 这么久）是否建立在最近一次换时区之前。连接池的 `before_acquire` 用它
/// 淘汰会话时区过期的连接；从没换过时恒为 false，热路径只多一次原子读。
#[must_use]
pub fn opened_before_change(age: Duration) -> bool {
    let changed = CHANGED_NANOS.load(Ordering::SeqCst);
    if changed == 0 {
        return false;
    }
    let opened = Instant::now().checked_sub(age).unwrap_or_else(epoch);
    nanos_since_epoch(opened) < changed
}

/// 把 settings 里的统计时区值（键不存在 = `None`）应用到本进程；返回是否变了。
/// 库里是坏值（只可能是手改的）记一条 WARN，按跟随服务器处理，统计照常能查。
pub fn apply_setting(value: Option<&serde_json::Value>) -> bool {
    let zone = match value.map(parse_setting) {
        None => None,
        Some(Ok(zone)) => zone,
        Some(Err(())) => {
            tracing::warn!(value = ?value, "settings.reporting_timezone 不是合法的时区名，按跟随服务器处理");
            None
        }
    };
    let changed = apply_reporting_timezone(zone.as_deref());
    if changed {
        tracing::info!(
            timezone = reporting_timezone().unwrap_or("unresolved"),
            follows_host = zone.is_none(),
            "statistics timezone applied"
        );
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_tz_and_utc_aliases_are_normalized() {
        assert_eq!(
            normalize(":/usr/share/zoneinfo/Asia/Kolkata").as_deref(),
            Some("Asia/Kolkata")
        );
        assert_eq!(normalize("UTC0").as_deref(), Some("UTC"));
        assert!(normalize("../../tmp/zone").is_none());
    }
    #[test]
    fn setting_values_parse_strictly() {
        use serde_json::json;
        assert_eq!(parse_setting(&json!(null)), Ok(None));
        assert_eq!(parse_setting(&json!("")), Ok(None));
        assert_eq!(parse_setting(&json!("  ")), Ok(None));
        assert_eq!(
            parse_setting(&json!("Asia/Shanghai")),
            Ok(Some("Asia/Shanghai".to_owned()))
        );
        assert_eq!(
            parse_setting(&json!("Etc/GMT-8")),
            Ok(Some("Etc/GMT-8".to_owned()))
        );
        // The name is interpolated into ClickHouse SQL: only the canonical spelling with the
        // IANA character set passes, never paths, aliases, padding or quotes.
        for bad in [
            json!("../etc/passwd"),
            json!("/usr/share/zoneinfo/Asia/Shanghai"),
            json!("posix/Asia/Shanghai"),
            json!("UTC0"),
            json!(" Asia/Shanghai"),
            json!("Asia/Shang hai"),
            json!("Asia/Shanghai'"),
            json!("Asia/Shanghai\\"),
            json!(8),
            json!(["UTC"]),
            json!({"zone": "UTC"}),
        ] {
            assert!(parse_setting(&bad).is_err(), "{bad}");
        }
    }

    // The only unit test that switches the process-wide zone; it restores the previous value.
    #[test]
    fn switching_zone_retires_only_older_connections() {
        let before = *OVERRIDE.read().unwrap();
        let other = if before == Some("Etc/GMT-3") {
            "Etc/GMT-4"
        } else {
            "Etc/GMT-3"
        };
        assert!(apply_reporting_timezone(Some(other)));
        assert_eq!(reporting_timezone().unwrap(), other);
        assert!(
            !apply_reporting_timezone(Some(other)),
            "same zone is not a change"
        );
        assert!(opened_before_change(Duration::from_hours(10 * 365 * 24)));
        assert!(!opened_before_change(Duration::ZERO));
        // Interning hands back the same allocation for the same name.
        assert!(std::ptr::eq(intern(other), intern(other)));
        apply_reporting_timezone(before);
        assert_eq!(*OVERRIDE.read().unwrap(), before);
    }

    #[test]
    fn copied_utc_file_does_not_require_zone_tab_entry() {
        let root = std::env::temp_dir().join(format!("okapi-zone-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Etc")).unwrap();
        std::fs::write(root.join("Etc/UTC"), b"test-utc-tzif").unwrap();
        assert_eq!(
            copied_timezone(b"test-utc-tzif", &root).as_deref(),
            Some("Etc/UTC")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
