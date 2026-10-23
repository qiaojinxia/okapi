//! 出站连接时的目标校验（SSRF 的第二层）。
//!
//! 管理面保存 URL 时（`console::ssrf`）只把域名解析一次、看一眼结果；域名之后可以改指内网
//! （DNS 重绑定：先给公网 IP 过闸，TTL 调 0 后换成内网 / 云元数据地址），网关就会带着渠道凭证向内网发请求。
//! 这里在**连接时**对解析结果再过同一条判据，并且连接只用这次校验过的地址——不再二次解析，
//! 校验与连接之间没有空窗。
//!
//! - 策略是进程级开关（[`set_allow_private`]，由应用按 `settings.ssrf_policy.allow_private` 同步），缺省严格；
//! - 只管**直连**的域名解析。经 HTTP / SOCKS 代理出去的连接由代理去解析目标，这里管不到，
//!   也不该拿代理自己（常在内网）的地址开刀；
//! - IP 字面量不经解析，仍由保存时的检查把关（字面量不会重绑定）；
//! - 已经建好的 keep-alive 连接不会重新解析，也就不会被收紧后的策略拦下，直到空闲回收；
//!   它连的是当初校验过的地址，不是重绑定之后的新地址。

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};

static ALLOW_PRIVATE: AtomicBool = AtomicBool::new(false);

/// 允许 / 禁止直连到私网、环回、链路本地地址（对应 `settings.ssrf_policy.allow_private`）。
pub fn set_allow_private(allow: bool) {
    ALLOW_PRIVATE.store(allow, Ordering::Relaxed);
}

#[must_use]
pub fn allow_private() -> bool {
    ALLOW_PRIVATE.load(Ordering::Relaxed)
}

/// 私网 / 环回 / 链路本地 / 未指定 / 广播；v6 的环回、未指定、ULA（fc00::/7）、链路本地（fe80::/10）
/// 与 v4 映射地址。管理面保存 URL 时用的就是这一条判据。
#[must_use]
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.to_ipv4_mapped()
                .is_some_and(|ip| is_private_ip(IpAddr::V4(ip)))
                || v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 ULA 与 fe80::/10 链路本地
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// 解析结果里只要有一个私网地址就整体拒绝：攻击者的 DNS 可以同时返回公网和内网记录，连接器会挨个试。
///
/// # Errors
/// 策略不允许私网、而结果里有私网地址时返回 `PermissionDenied`。
pub fn ensure_allowed(addrs: &[SocketAddr], allow_private: bool) -> io::Result<()> {
    match addrs.iter().find(|addr| is_private_ip(addr.ip())) {
        Some(addr) if !allow_private => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("ssrf: 目标解析到私网地址 {}，ssrf_policy 不允许", addr.ip()),
        )),
        _ => Ok(()),
    }
}

/// 解析并校验；返回的地址就是要连的地址。IP 字面量原样返回（保存时已查过，且不会重绑定）。
///
/// # Errors
/// 解析失败，或结果被 [`ensure_allowed`] 拒绝。
pub async fn resolve_checked(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    let host = host.trim_matches(['[', ']']);
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    ensure_allowed(&addrs, allow_private())?;
    Ok(addrs)
}

/// reqwest 的 DNS 解析器：直连的 client 用它，解析结果过 [`ensure_allowed`]，reqwest 随后只连这些地址。
pub struct GuardedResolver;

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let addrs = resolve_checked(name.as_str(), 0)
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 443)
    }

    #[test]
    fn private_ranges() {
        for ip in [
            "10.0.0.1",
            "172.16.5.5",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2606:4700::1", "::ffff:8.8.8.8"] {
            assert!(!is_private_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn a_private_answer_is_refused_unless_the_policy_allows_it() {
        let public = [addr("8.8.8.8"), addr("2606:4700::1")];
        assert!(ensure_allowed(&public, false).is_ok());
        // 域名重绑定后解析到云元数据地址
        let rebound = [addr("169.254.169.254")];
        let error = ensure_allowed(&rebound, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(ensure_allowed(&rebound, true).is_ok());
        // 公网 + 内网混着返回也整体拒绝：连接器会挨个试到内网那个
        let mixed = [addr("8.8.8.8"), addr("10.0.0.7")];
        assert!(ensure_allowed(&mixed, false).is_err());
    }

    #[tokio::test]
    async fn ip_literals_skip_resolution_and_the_connect_time_check() {
        // 字面量不会重绑定，保存时已经按策略查过；这里原样放行（含方括号形式的 v6）
        assert_eq!(
            resolve_checked("127.0.0.1", 8080).await.unwrap(),
            vec!["127.0.0.1:8080".parse().unwrap()]
        );
        assert_eq!(
            resolve_checked("[::1]", 443).await.unwrap(),
            vec!["[::1]:443".parse().unwrap()]
        );
    }
}
