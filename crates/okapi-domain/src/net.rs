//! Shared network classification primitives and default probe constants.
//!
//! Kept in `okapi-domain` (zero external dependencies) so both `okapi-kernel`
//! (write-time URL/SSRF validation) and `okapi-providers` (connect-time DNS-rebinding
//! guard & egress probe) share a single authoritative rule table without coupling
//! `okapi-kernel` to `okapi-providers`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// 出口代理缺省探测地址。
pub const DEFAULT_EGRESS_PROBE_TARGET: &str = "https://www.cloudflare.com/cdn-cgi/trace";

/// 不是公网单播的地址：私网、环回、链路本地、未指定、广播，以及 IANA 特殊用途段——共享地址空间
/// 100.64.0.0/10（运营商级 NAT；阿里云元数据 100.100.100.200 就在里面）、Azure WireServer 168.63.129.16、文档 / 保留 / 多播段，
/// v6 在 2000::/3 之外的全部（环回、ULA、链路本地、多播……）以及其中的文档 / 协议保留段。
/// 内嵌 v4 的写法（v4 映射、NAT64 `64:ff9b::/96`、6to4 `2002::/16`）落到内嵌的 v4 上判。
///
/// 基准测试段 198.18.0.0/15 不在其中：代理软件的 fake-ip DNS 会把所有域名解析进这一段，
/// 流量由本机的代理接走、并不到内网；拦它只会让这类环境连不上任何上游。
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
