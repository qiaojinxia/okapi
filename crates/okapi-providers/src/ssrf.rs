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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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

/// 不是公网单播的地址：私网、环回、链路本地、未指定、广播，以及 IANA 特殊用途段——共享地址空间
/// 100.64.0.0/10（运营商级 NAT；阿里云元数据 100.100.100.200 就在里面）、Azure WireServer 168.63.129.16、文档 / 保留 / 多播段，
/// v6 在 2000::/3 之外的全部（环回、ULA、链路本地、多播……）以及其中的文档 / 协议保留段。
/// 内嵌 v4 的写法（v4 映射、NAT64 `64:ff9b::/96`、6to4 `2002::/16`）落到内嵌的 v4 上判。
///
/// 基准测试段 198.18.0.0/15 不在其中：代理软件的 fake-ip DNS 会把所有域名解析进这一段，
/// 流量由本机的代理接走、并不到内网；拦它只会让这类环境连不上任何上游。
///
/// 管理面保存 URL、出站连接时的解析结果校验、图片抓取三处共用这一条判据，不各留一份表
/// （图片抓取在它之上多拒 198.18.0.0/15）。
#[must_use]
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => special_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(embedded) = embedded_v4(v6) {
                return special_v4(embedded);
            }
            let s = v6.segments();
            // 2000::/3 之外没有可路由的公网单播：环回、未指定、ULA、链路本地、站点本地、多播、
            // NAT64 本地用途（64:ff9b:1::/48）……都在这里一并拒掉
            (s[0] & 0xe000) != 0x2000
                // 2001:0000–01ff 是 IETF 协议保留（Teredo、ORCHID 等），2001:db8::/32 文档
                || (s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                // 3fff::/20 文档（RFC 9637）
                || (s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

fn special_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, d] = ip.octets();
    matches!(a, 0 | 10 | 127 | 224..=255)
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        // 192.0.0.0/24 IETF 协议保留、192.0.2.0/24 文档；192.0.1.0/24 与 192.0.3.0 以后是公网
        || (a == 192 && b == 0 && matches!(c, 0 | 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // Azure 的 WireServer：公网地址，但只对 Azure 虚机内部有意义，是常见的云内 SSRF 目标
        || (a == 168 && b == 63 && c == 129 && d == 16)
}

/// v6 地址里内嵌的 v4：v4 映射 `::ffff:a.b.c.d`、v4 兼容 `::a.b.c.d`、NAT64 `64:ff9b::/96`、
/// 6to4 `2002:a.b:c.d::/48`。v6 的环回 `::1` 与未指定 `::` 不算兼容写法。
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let from = |high: u16, low: u16| {
        let [a, b] = high.to_be_bytes();
        let [c, d] = low.to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    };
    match ip.segments() {
        [0, 0, 0, 0, 0, 0xffff, high, low] | [0x64, 0xff9b, 0, 0, 0, 0, high, low] => {
            Some(from(high, low))
        }
        [0, 0, 0, 0, 0, 0, high, low] if high != 0 || low > 1 => Some(from(high, low)),
        [0x2002, high, low, ..] => Some(from(high, low)),
        _ => None,
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
            "::",
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

    /// 以前只盖私网 / 环回 / 链路本地：运营商级 NAT 段、保留段、多播、文档段都漏过去。
    /// 阿里云的元数据地址 100.100.100.200 在 100.64.0.0/10 里。
    #[test]
    fn special_purpose_ranges_are_not_public() {
        for ip in [
            "100.100.100.200",
            "168.63.129.16",
            "100.64.0.1",
            "100.127.255.254",
            "0.1.2.3",
            "192.0.0.192",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "192.88.99.1",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            // v6 在 2000::/3 之外：多播、站点本地、NAT64 本地用途、放弃前缀
            "ff02::1",
            "fec0::1",
            "64:ff9b:1::1",
            "100::1",
            // 2000::/3 之内的协议保留 / 文档段
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "3fff::1",
            "3fff:fff::1",
            // 内嵌 v4：映射、兼容、NAT64、6to4 都按内嵌的地址判
            "::ffff:100.100.100.200",
            "::10.0.0.1",
            "::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "2002:a00:1::1",
            "2002:7f00:1::",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "168.63.129.15",
            "168.63.129.17",
            "192.0.1.1",
            "192.0.3.1",
            "198.17.255.255",
            "198.20.0.1",
            // 基准测试段：fake-ip 代理环境里所有域名都解析到这里，通用判据不拦
            "198.18.0.1",
            "198.19.255.255",
            "223.255.255.255",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "3fff:1000::1",
            // 内嵌的是公网 v4：映射 / NAT64 / 6to4 都放行（IPv6-only 网络里的 IPv4-only 站点就走 NAT64）
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
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
