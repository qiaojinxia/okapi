//! IP / CIDR 匹配（key 级 IP 白名单，IMPLEMENTATION §11.17）。
//!
//! 不引依赖：白名单条目只有两种形态——单个地址（`203.0.113.9`、`2001:db8::1`）与
//! CIDR（`203.0.113.0/24`、`2001:db8::/32`）；解析失败的条目视为不匹配而非放行。

use std::net::IpAddr;

/// 单条白名单条目是否匹配地址。IPv4 与 IPv6 不互相匹配（`::ffff:1.2.3.4` 不做映射折叠，
/// 白名单要精确）。
#[must_use]
pub fn entry_matches(entry: &str, ip: IpAddr) -> bool {
    let entry = entry.trim();
    if entry.is_empty() {
        return false;
    }
    let Some((base, bits)) = entry.split_once('/') else {
        return entry.parse::<IpAddr>().is_ok_and(|e| e == ip);
    };
    let Ok(base) = base.trim().parse::<IpAddr>() else {
        return false;
    };
    let Ok(bits) = bits.trim().parse::<u32>() else {
        return false;
    };
    match (base, ip) {
        (IpAddr::V4(b), IpAddr::V4(a)) => {
            if bits > 32 {
                return false;
            }
            let mask = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            (u32::from(b) & mask) == (u32::from(a) & mask)
        }
        (IpAddr::V6(b), IpAddr::V6(a)) => {
            if bits > 128 {
                return false;
            }
            let mask = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            (u128::from(b) & mask) == (u128::from(a) & mask)
        }
        _ => false,
    }
}

/// 白名单是否放行：空清单 = 不限；非空清单里任一条命中即放行。
#[must_use]
pub fn allowed(list: &[String], ip: IpAddr) -> bool {
    list.is_empty() || list.iter().any(|e| entry_matches(e, ip))
}

/// 条目是否是合法的地址或 CIDR（写入前校验；不让一条拼错的白名单把 key 锁死却毫无提示）。
#[must_use]
pub fn is_valid_entry(entry: &str) -> bool {
    let entry = entry.trim();
    match entry.split_once('/') {
        None => entry.parse::<IpAddr>().is_ok(),
        Some((base, bits)) => match (base.trim().parse::<IpAddr>(), bits.trim().parse::<u32>()) {
            (Ok(IpAddr::V4(_)), Ok(b)) => b <= 32,
            (Ok(IpAddr::V6(_)), Ok(b)) => b <= 128,
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn exact_and_cidr_v4() {
        assert!(entry_matches("203.0.113.9", ip("203.0.113.9")));
        assert!(!entry_matches("203.0.113.9", ip("203.0.113.10")));
        assert!(entry_matches("203.0.113.0/24", ip("203.0.113.255")));
        assert!(!entry_matches("203.0.113.0/24", ip("203.0.114.1")));
        assert!(entry_matches("0.0.0.0/0", ip("8.8.8.8")), "/0 放行一切 v4");
        assert!(
            !entry_matches("203.0.113.0/33", ip("203.0.113.1")),
            "非法前缀不放行"
        );
    }

    #[test]
    fn cidr_v6_and_no_cross_family() {
        assert!(entry_matches("2001:db8::/32", ip("2001:db8:1::5")));
        assert!(!entry_matches("2001:db8::/32", ip("2001:db9::1")));
        assert!(
            !entry_matches("203.0.113.0/24", ip("::ffff:203.0.113.9")),
            "不折叠映射地址"
        );
        assert!(!entry_matches("garbage", ip("1.1.1.1")));
        assert!(!entry_matches("", ip("1.1.1.1")));
    }

    #[test]
    fn list_semantics_and_validation() {
        assert!(allowed(&[], ip("1.1.1.1")), "空清单 = 不限");
        let list = vec!["10.0.0.0/8".to_owned(), "203.0.113.9".to_owned()];
        assert!(allowed(&list, ip("10.20.30.40")));
        assert!(allowed(&list, ip("203.0.113.9")));
        assert!(!allowed(&list, ip("203.0.113.10")));
        assert!(is_valid_entry("10.0.0.0/8"));
        assert!(is_valid_entry("2001:db8::1"));
        assert!(!is_valid_entry("10.0.0.0/40"));
        assert!(!is_valid_entry("example.com"));
    }
}
