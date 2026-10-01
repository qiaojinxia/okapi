//! Use the process/host timezone, independently of database container defaults.
use crate::StoreError;
use std::sync::OnceLock;

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
