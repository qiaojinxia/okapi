//! 服务器压力：读 Linux `/proc` 与 `statvfs`。容器里 `/proc/stat`、`/proc/meminfo`、
//! `/proc/loadavg` 反映的是宿主机（没装 lxcfs 时），磁盘看的是根文件系统所在的盘；
//! 网卡计数是容器自己的网络命名空间。非 Linux（本地开发）各项为 `None`，面板显示「不支持」。

use serde::Serialize;
use std::time::{Duration, Instant};

/// `/proc/stat` 首行累计 jiffies：总量与空闲（idle + iowait）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
}

#[must_use]
pub fn parse_cpu(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal（guest 已含在 user 里，不重复加）
    let total = fields.iter().take(8).sum();
    let idle = fields.get(3)? + fields.get(4).copied().unwrap_or(0);
    Some(CpuTimes { total, idle })
}

/// 两次采样之间的 CPU 占用（0–100）。计数回绕或间隔为零返回 `None`。
#[must_use]
pub fn cpu_percent(prev: CpuTimes, cur: CpuTimes) -> Option<f64> {
    let total = cur.total.checked_sub(prev.total)?;
    let idle = cur.idle.checked_sub(prev.idle)?;
    (total > 0).then(|| (total.saturating_sub(idle)) as f64 * 100.0 / total as f64)
}

#[must_use]
pub fn parse_cpu_count(stat: &str) -> usize {
    stat.lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .count()
}

#[must_use]
pub fn parse_loadavg(raw: &str) -> Option<[f64; 3]> {
    let mut it = raw.split_whitespace().map(str::parse::<f64>);
    Some([it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?])
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}

/// `/proc/meminfo`（单位 kB）。没有 `MemAvailable` 的老内核按 free + buffers + cached 估。
#[must_use]
pub fn parse_meminfo(raw: &str) -> Option<Memory> {
    let get = |key: &str| {
        raw.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kb| kb * 1024)
    };
    let total_bytes = get("MemTotal")?;
    let available_bytes = get("MemAvailable").unwrap_or_else(|| {
        get("MemFree").unwrap_or(0) + get("Buffers").unwrap_or(0) + get("Cached").unwrap_or(0)
    });
    Some(Memory {
        total_bytes,
        available_bytes,
        swap_total_bytes: get("SwapTotal").unwrap_or(0),
        swap_free_bytes: get("SwapFree").unwrap_or(0),
    })
}

/// `/proc/net/dev` 里除回环外所有网卡的累计收发字节。
#[must_use]
pub fn parse_net_dev(raw: &str) -> Option<(u64, u64)> {
    let mut found = false;
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in raw.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let cols: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        if let (Some(r), Some(t)) = (cols.first(), cols.get(8)) {
            rx = rx.saturating_add(*r);
            tx = tx.saturating_add(*t);
            found = true;
        }
    }
    found.then_some((rx, tx))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Process {
    pub rss_bytes: u64,
    pub threads: u64,
    pub open_fds: Option<u64>,
}

#[must_use]
pub fn parse_process_status(raw: &str) -> Option<Process> {
    let get = |key: &str| {
        raw.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
    };
    Some(Process {
        rss_bytes: get("VmRSS")? * 1024,
        threads: get("Threads").unwrap_or(0),
        open_fds: None,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Disk {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// 一次读数（累计计数器）；占用率要两次读数相减，见 [`Rates::between`]。
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub at: Instant,
    pub cpu: Option<CpuTimes>,
    pub net: Option<(u64, u64)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Rates {
    pub cpu_percent: Option<f64>,
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
}

impl Rates {
    #[must_use]
    pub fn between(prev: &Reading, cur: &Reading) -> Self {
        let secs = cur.at.duration_since(prev.at).as_secs_f64();
        let rate = |a: u64, b: u64| {
            (secs > 0.0)
                .then(|| b.checked_sub(a))
                .flatten()
                .map(|d| d as f64 / secs)
        };
        let (rx, tx) = match (prev.net, cur.net) {
            (Some((r0, t0)), Some((r1, t1))) => (rate(r0, r1), rate(t0, t1)),
            _ => (None, None),
        };
        Self {
            cpu_percent: prev.cpu.zip(cur.cpu).and_then(|(a, b)| cpu_percent(a, b)),
            net_rx_bps: rx,
            net_tx_bps: tx,
        }
    }
}

/// 某一时刻的服务器概况（不含需要两次读数的占用率）。
#[derive(Clone, Debug, Default, Serialize)]
pub struct Host {
    pub cpus: Option<usize>,
    pub load: Option<[f64; 3]>,
    pub memory: Option<Memory>,
    pub disk: Option<Disk>,
    pub uptime_secs: Option<u64>,
    pub process: Option<Process>,
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[must_use]
pub fn reading() -> Reading {
    Reading {
        at: Instant::now(),
        cpu: read("/proc/stat").as_deref().and_then(parse_cpu),
        net: read("/proc/net/dev").as_deref().and_then(parse_net_dev),
    }
}

#[must_use]
pub fn host() -> Host {
    let process = read("/proc/self/status")
        .as_deref()
        .and_then(parse_process_status)
        .map(|mut p| {
            p.open_fds = std::fs::read_dir("/proc/self/fd")
                .ok()
                .map(|d| d.count() as u64);
            p
        });
    Host {
        cpus: read("/proc/stat")
            .as_deref()
            .map(parse_cpu_count)
            .filter(|n| *n > 0),
        load: read("/proc/loadavg").as_deref().and_then(parse_loadavg),
        memory: read("/proc/meminfo").as_deref().and_then(parse_meminfo),
        disk: disk("/"),
        uptime_secs: read("/proc/uptime")
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .map(|s| s as u64),
        process,
    }
}

/// 隔一小段时间读两次，得出当前的 CPU 占用与网卡速率（实时面板用）。
pub async fn rates_now(window: Duration) -> Rates {
    let first = reading();
    tokio::time::sleep(window).await;
    Rates::between(&first, &reading())
}

#[must_use]
pub fn disk(path: &str) -> Option<Disk> {
    let stat = rustix::fs::statvfs(path).ok()?;
    let block = stat.f_frsize.max(1);
    Some(Disk {
        total_bytes: stat.f_blocks.saturating_mul(block),
        free_bytes: stat.f_bavail.saturating_mul(block),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 50 0 25 400 25 0 0 0 0 0\ncpu1 50 0 25 400 25 0 0 0 0 0\nintr 1\n";

    #[test]
    fn cpu_counts_iowait_as_idle_and_counts_cores() {
        let a = parse_cpu(STAT).unwrap();
        assert_eq!(
            a,
            CpuTimes {
                total: 1000,
                idle: 850
            }
        );
        let b = CpuTimes {
            total: 1100,
            idle: 900,
        };
        assert_eq!(cpu_percent(a, b), Some(50.0));
        assert_eq!(cpu_percent(b, a), None, "计数回绕不出负数");
        assert_eq!(parse_cpu_count(STAT), 2);
    }

    #[test]
    fn meminfo_prefers_mem_available() {
        let raw = "MemTotal:       2048 kB\nMemFree:         100 kB\nMemAvailable:    1024 kB\nBuffers: 1 kB\nCached: 1 kB\nSwapTotal: 512 kB\nSwapFree: 256 kB\n";
        let m = parse_meminfo(raw).unwrap();
        assert_eq!(m.total_bytes, 2048 * 1024);
        assert_eq!(m.available_bytes, 1024 * 1024);
        assert_eq!(m.swap_free_bytes, 256 * 1024);
        let old = "MemTotal: 2048 kB\nMemFree: 100 kB\nBuffers: 10 kB\nCached: 20 kB\n";
        assert_eq!(parse_meminfo(old).unwrap().available_bytes, 130 * 1024);
    }

    #[test]
    fn net_dev_skips_loopback() {
        let raw = "Inter-|   Receive |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes\n    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\n  eth0: 100 1 0 0 0 0 0 0 200 1 0 0 0 0 0 0\n  eth1: 5 1 0 0 0 0 0 0 7 1 0 0 0 0 0 0\n";
        assert_eq!(parse_net_dev(raw), Some((105, 207)));
    }

    #[test]
    fn loadavg_and_process_status() {
        assert_eq!(
            parse_loadavg("0.50 1.25 2.00 1/100 42\n"),
            Some([0.5, 1.25, 2.0])
        );
        let p = parse_process_status("Name: okapi\nVmRSS:\t  2048 kB\nThreads:\t12\n").unwrap();
        assert_eq!((p.rss_bytes, p.threads), (2048 * 1024, 12));
    }

    #[test]
    fn disk_reads_the_root_filesystem() {
        let d = disk("/").unwrap();
        assert!(d.total_bytes > 0 && d.free_bytes <= d.total_bytes);
    }
}
