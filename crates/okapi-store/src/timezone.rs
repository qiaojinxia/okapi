//! Use the process/host timezone, independently of database container defaults.
use crate::StoreError;
use std::sync::OnceLock;

pub fn machine_timezone() -> Result<&'static str, StoreError> {
    static ZONE: OnceLock<Option<String>> = OnceLock::new();
    ZONE.get_or_init(|| {
        let configured = std::env::var("TZ").ok().filter(|s| !s.is_empty());
        let linked = std::fs::canonicalize("/etc/localtime").ok().and_then(|p| {
            p.to_str()
                .and_then(|s| s.split_once("zoneinfo/").map(|(_, zone)| zone.to_owned()))
        });
        let named = std::fs::read_to_string("/etc/timezone")
            .ok()
            .map(|s| s.trim().to_owned());
        configured
            .or(linked)
            .or(named)
            .or_else(|| {
                let local = std::fs::read("/etc/localtime").ok();
                let index = std::fs::read_to_string("/usr/share/zoneinfo/zone.tab").ok();
                local
                    .as_ref()
                    .zip(index.as_ref())
                    .and_then(|(local, index)| {
                        index
                            .lines()
                            .filter(|line| !line.starts_with('#'))
                            .filter_map(|line| line.split_whitespace().nth(2))
                            .find(|zone| {
                                std::fs::read(format!("/usr/share/zoneinfo/{zone}"))
                                    .is_ok_and(|bytes| bytes == *local)
                            })
                            .map(str::to_owned)
                    })
                    .or_else(|| {
                        // Minimal UTC containers need no named zone file.
                        (local.is_none() && chrono::Local::now().offset().local_minus_utc() == 0)
                            .then(|| "UTC".to_owned())
                    })
            })
            .map(|s| s.trim_start_matches(':').to_owned())
            .filter(|s| {
                !s.is_empty()
                    && s.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+')
                    })
            })
    })
    .as_deref()
    .ok_or(StoreError::InvalidData("machine_timezone_unavailable"))
}
