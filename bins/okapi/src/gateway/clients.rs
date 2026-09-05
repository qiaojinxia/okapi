//! client_type 识别（#5277）：UA 解析为稳定枚举串，进 billing_records 与 CH 维度列。
//!
//! 规则按**特异性降序**排列且取首次命中：专用工具（Claude Code / Codex CLI / IDE
//! 插件）在前，它们的 UA 往往同时带着底层 SDK 的标识（Claude Code 走
//! anthropic SDK、Cursor 走 openai SDK），先匹配 SDK 会把工具归错类。

use axum::http::HeaderMap;

const RULES: &[(&str, &str)] = &[
    // 编码智能体 / CLI
    ("claude-code", "claude-code"),
    ("claude_cli", "claude-code"),
    ("codex", "codex-cli"),
    ("gemini-cli", "gemini-cli"),
    ("cursor", "cursor"),
    ("cline", "cline"),
    ("continue", "continue"),
    // 桌面 / Web 聊天客户端
    ("lobechat", "lobechat"),
    ("lobe-chat", "lobechat"),
    ("cherry", "cherry-studio"),
    ("chatbox", "chatbox"),
    ("nextchat", "nextchat"),
    ("chatgpt-next-web", "nextchat"),
    ("sillytavern", "sillytavern"),
    ("immersive", "immersive-translate"),
    ("dify", "dify"),
    // 官方 SDK（Stainless 生成，UA 形如 `OpenAI/Python 1.x` / `Anthropic/JS 0.x`）
    ("openai/python", "openai-python"),
    ("openai-python", "openai-python"),
    ("openai/js", "openai-node"),
    ("openai-node", "openai-node"),
    ("anthropic/python", "anthropic-python"),
    ("anthropic/js", "anthropic-node"),
    ("langchain", "langchain"),
    ("litellm", "litellm"),
    // 通用 HTTP 客户端（最后兜底：任何专用工具都可能建在它们之上）
    ("postmanruntime", "postman"),
    ("python-requests", "python-requests"),
    ("python-httpx", "python-httpx"),
    ("axios", "axios"),
    ("node-fetch", "node"),
    ("undici", "node"),
    ("go-http-client", "go"),
    ("okhttp", "okhttp"),
    ("curl", "curl"),
];

#[must_use]
pub fn detect_client_type(headers: &HeaderMap) -> &'static str {
    let Some(ua) = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
    else {
        return "";
    };
    let ua = ua.to_ascii_lowercase();
    for (needle, label) in RULES {
        if ua.contains(needle) {
            return label;
        }
    }
    ""
}

/// 对端 socket 地址经中间件写入的内部头。入口先剥掉客户端可能伪造的同名头再写入，
/// 故它只可能来自本进程——是"这个请求究竟从哪来"的唯一可信锚点。
pub const PEER_IP_HEADER: &str = "x-okapi-peer-ip";

/// 边缘（CDN / 反代）注入的信任凭证头（§14.2 模式二 #6502）：验证通过即信任转发头，
/// 免去维护 CDN 回源 IP 段。
pub const EDGE_KEY_HEADER: &str = "x-okapi-edge-key";

/// 网关 / 控制台中间件：把 `ConnectInfo<SocketAddr>` 写成内部头，供只拿得到 `HeaderMap` 的
/// 鉴权 / 记账路径读取（改十几个 handler 的签名去接 ConnectInfo 不值得）。
/// 测试用 `axum::serve(listener, app)` 不带 connect info 时无扩展 → 只剥不写。
pub async fn stamp_peer_ip(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    req.headers_mut().remove(PEER_IP_HEADER);
    if let Some(peer) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
        && let Ok(value) = axum::http::HeaderValue::from_str(&peer.to_string())
    {
        req.headers_mut().insert(PEER_IP_HEADER, value);
    }
    next.run(req).await
}

/// 转发头信任策略（§14.2）。
///
/// 转发头是**调用方可写**的：直连部署下 `X-Forwarded-For: <随便什么>` 完全由客户端说了算，
/// 而项目自带的 `deploy/nginx-sse.conf` 用 `$proxy_add_x_forwarded_for`——它**追加**而非覆盖，
/// 客户端塞的值原样留在链首。所以"XFF 取最左"等于让调用方自选来源 IP：key 级 IP 白名单
/// （§11.17）、登录 / 兑换的每 IP 限流、日志 `client_ip` 列会一起失真——白名单尤其致命，
/// 一个头就能绕过。
///
/// 定案（§14.2 两模式）：**来源可信时转发头才作数**——
/// 1. 对端落在 `OKAPI_TRUSTED_PROXIES` 名单内（未设置时缺省仅环回：同机反代开箱即用，
///    容器 / K8s 里反代与网关不同 IP，须显式配网段）；
/// 2. 或请求带着对得上 `OKAPI_EDGE_KEY` 的边缘凭证头（免维护 CDN 回源段）。
///
/// 不可信来源一律取 socket 对端。可信来源下 XFF 取**最右非信任跳**——反代追加的那一段
/// 才是它亲眼看到的对端，更左侧全是上一跳的转述。
pub struct TrustPolicy {
    /// 信任反代 / CDN 回源段（地址或 CIDR）。空 = 只认边缘凭证。
    proxies: Vec<String>,
    /// 边缘凭证（None = 未启用模式二）。
    edge_key: Option<String>,
}

impl TrustPolicy {
    /// 从环境装配。`OKAPI_TRUSTED_PROXIES` 未设置 = 仅环回；显式设空串 = 谁都不信
    /// （直连公网部署的正确配置）。非法条目丢弃并告警——一条拼错的网段不该让整份名单失效。
    #[must_use]
    pub fn from_env() -> Self {
        let proxies = match std::env::var("OKAPI_TRUSTED_PROXIES") {
            Err(_) => vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()],
            Ok(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .filter_map(|e| {
                    if okapi_store::netmatch::is_valid_entry(e) {
                        Some(e.to_owned())
                    } else {
                        tracing::warn!(entry = e, "OKAPI_TRUSTED_PROXIES 条目非法，已忽略");
                        None
                    }
                })
                .collect(),
        };
        let edge_key = std::env::var("OKAPI_EDGE_KEY")
            .ok()
            .filter(|v| !v.is_empty());
        if proxies.is_empty() && edge_key.is_none() {
            tracing::info!("无信任来源：转发头一律不作数，来源 IP 只取 socket 对端");
        }
        Self { proxies, edge_key }
    }

    /// 测试构造：绕开环境变量直接给名单（`from_env` 的 `OnceLock` 全局不便于用例摆布）。
    #[cfg(test)]
    fn new(proxies: &[&str], edge_key: Option<&str>) -> Self {
        Self {
            proxies: proxies.iter().map(|s| (*s).to_owned()).collect(),
            edge_key: edge_key.map(str::to_owned),
        }
    }

    /// 该地址是否属于信任反代。
    fn trusts(&self, ip: std::net::IpAddr) -> bool {
        self.proxies
            .iter()
            .any(|e| okapi_store::netmatch::entry_matches(e, ip))
    }

    /// 来源是否可信。拿不到对端（未挂 connect info）且无边缘凭证 → 不可信：
    /// "不知道你从哪来"不该换来"那就信你自己说的"。
    fn source_trusted(&self, headers: &HeaderMap, peer: Option<std::net::IpAddr>) -> bool {
        if peer.is_some_and(|ip| self.trusts(ip)) {
            return true;
        }
        match (
            self.edge_key.as_deref(),
            headers.get(EDGE_KEY_HEADER).and_then(|v| v.to_str().ok()),
        ) {
            (Some(expect), Some(got)) => ct_eq(expect.as_bytes(), got.as_bytes()),
            _ => false,
        }
    }

    /// 请求来源 IP。
    #[must_use]
    pub fn client_ip(&self, headers: &HeaderMap) -> Option<std::net::IpAddr> {
        let peer = header_ip(headers, PEER_IP_HEADER);
        if !self.source_trusted(headers, peer) {
            return peer;
        }
        for name in ["true-client-ip", "cf-connecting-ip", "x-real-ip"] {
            if let Some(ip) = header_ip(headers, name) {
                return Some(ip);
            }
        }
        if let Some(chain) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            // 右→左跳过信任段，第一个非信任跳即真实来源；整条链都是自己人时取最左。
            let mut leftmost_trusted = None;
            for hop in chain.rsplit(',') {
                let Ok(ip) = hop.trim().parse::<std::net::IpAddr>() else {
                    continue;
                };
                if !self.trusts(ip) {
                    return Some(ip);
                }
                leftmost_trusted = Some(ip);
            }
            if leftmost_trusted.is_some() {
                return leftmost_trusted;
            }
        }
        peer
    }
}

/// 进程级信任策略（首次使用时读环境）。
pub fn trust_policy() -> &'static TrustPolicy {
    static POLICY: std::sync::OnceLock<TrustPolicy> = std::sync::OnceLock::new();
    POLICY.get_or_init(TrustPolicy::from_env)
}

/// 请求来源 IP（白名单判定用）。
#[must_use]
pub fn client_ip(headers: &HeaderMap) -> Option<std::net::IpAddr> {
    trust_policy().client_ip(headers)
}

/// 请求来源 IP 的字符串形态（落 billing / CH `client_ip` 列）。
#[must_use]
pub fn detect_client_ip(headers: &HeaderMap) -> Option<String> {
    client_ip(headers).map(|ip| ip.to_string())
}

/// 取单个头的首个合法 IP（`x-real-ip` 等单值头；多值时取首段）。
fn header_ip(headers: &HeaderMap, name: &str) -> Option<std::net::IpAddr> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .and_then(|v| v.trim().parse().ok())
}

/// 定长比较：边缘凭证逐字节折叠，不因首个不同字节提前返回。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::{EDGE_KEY_HEADER, PEER_IP_HEADER, TrustPolicy, detect_client_type};
    use axum::http::{HeaderMap, HeaderValue, header};

    fn detect(ua: &str) -> &'static str {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, HeaderValue::from_str(ua).unwrap());
        detect_client_type(&headers)
    }

    /// 官方 SDK 的真实 UA 形态（Stainless 生成器）——此前规则表写的是
    /// `openai-python`，与真实 UA `OpenAI/Python` 永远匹配不上，分布图里
    /// 最大的一类流量会全部落进"未知"。
    #[test]
    fn stainless_sdk_user_agents_are_recognised() {
        assert_eq!(detect("OpenAI/Python 1.54.3"), "openai-python");
        assert_eq!(detect("OpenAI/JS 4.73.0"), "openai-node");
        assert_eq!(detect("Anthropic/Python 0.39.0"), "anthropic-python");
        assert_eq!(detect("Anthropic/JS 0.32.1"), "anthropic-node");
    }

    /// 专用工具建在 SDK 之上，UA 同时含两者标识时必须归到工具而非 SDK。
    #[test]
    fn specific_tools_win_over_underlying_sdk() {
        assert_eq!(
            detect("claude-code/1.0.2 Anthropic/JS 0.32.1"),
            "claude-code"
        );
        assert_eq!(detect("Cursor/0.45 OpenAI/JS 4.7"), "cursor");
        assert_eq!(detect("codex_cli_rs/0.9.0"), "codex-cli");
    }

    #[test]
    fn generic_http_clients_are_last_resort_and_unknown_is_empty() {
        assert_eq!(detect("python-requests/2.32.0"), "python-requests");
        assert_eq!(detect("curl/8.6.0"), "curl");
        assert_eq!(detect("Mozilla/5.0 (X11; Linux) SomeBrowser/1.0"), "");
        assert_eq!(detect_client_type(&HeaderMap::new()), "");
    }
    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    /// 不可信来源（直连公网）：转发头一概不作数，只认 socket 对端。
    /// 这正是 key 级 IP 白名单被一个 `X-Forwarded-For` 绕过的那条路。
    #[test]
    fn untrusted_peer_cannot_forge_its_source() {
        let policy = TrustPolicy::new(&["127.0.0.1/32"], None);
        let headers = hm(&[
            (PEER_IP_HEADER, "198.51.100.7"),
            ("x-forwarded-for", "203.0.113.9"),
            ("cf-connecting-ip", "203.0.113.9"),
            ("true-client-ip", "203.0.113.9"),
            ("x-real-ip", "203.0.113.9"),
        ]);
        assert_eq!(
            policy.client_ip(&headers).map(|i| i.to_string()).as_deref(),
            Some("198.51.100.7")
        );
    }

    /// 拿不到对端又无边缘凭证 = 无从判断来源 → 不可信，不退化成"信头"。
    #[test]
    fn missing_peer_is_untrusted_not_trusted() {
        let policy = TrustPolicy::new(&["127.0.0.1/32"], None);
        assert_eq!(policy.client_ip(&hm(&[("x-real-ip", "203.0.113.9")])), None);
    }

    /// 可信反代：XFF 取最右非信任跳。nginx 的 `$proxy_add_x_forwarded_for` 是追加，
    /// 链首那段由客户端书写——取最左就等于让调用方自选来源。
    #[test]
    fn trusted_proxy_takes_rightmost_untrusted_hop() {
        let policy = TrustPolicy::new(&["127.0.0.1/32", "10.0.0.0/8"], None);
        let headers = hm(&[
            (PEER_IP_HEADER, "127.0.0.1"),
            ("x-forwarded-for", "203.0.113.9, 198.51.100.4, 10.0.0.1"),
        ]);
        assert_eq!(
            policy.client_ip(&headers).map(|i| i.to_string()).as_deref(),
            Some("198.51.100.4"),
            "10.0.0.1 是信任段要跳过，再右一跳才是它亲眼看到的对端"
        );

        // 客户端伪造链首、反代追加真实对端 → 取到的是真实对端而非伪造值
        let forged = hm(&[
            (PEER_IP_HEADER, "127.0.0.1"),
            ("x-forwarded-for", "203.0.113.9, 198.51.100.7"),
        ]);
        assert_eq!(
            policy.client_ip(&forged).map(|i| i.to_string()).as_deref(),
            Some("198.51.100.7")
        );

        // 整条链都是自己人 → 取最左（都在信任域内，无从再分辨）
        let internal = hm(&[
            (PEER_IP_HEADER, "127.0.0.1"),
            ("x-forwarded-for", "10.1.2.3, 10.0.0.1"),
        ]);
        assert_eq!(
            policy
                .client_ip(&internal)
                .map(|i| i.to_string())
                .as_deref(),
            Some("10.1.2.3")
        );
    }

    /// 边缘凭证（模式二）：对端不在名单内也可信，凭证对不上则不可信。
    #[test]
    fn edge_key_grants_and_withholds_trust() {
        let policy = TrustPolicy::new(&[], Some("s3cret"));
        let ok = hm(&[
            (PEER_IP_HEADER, "198.51.100.7"),
            (EDGE_KEY_HEADER, "s3cret"),
            ("x-real-ip", "203.0.113.9"),
        ]);
        assert_eq!(
            policy.client_ip(&ok).map(|i| i.to_string()).as_deref(),
            Some("203.0.113.9")
        );
        let bad = hm(&[
            (PEER_IP_HEADER, "198.51.100.7"),
            (EDGE_KEY_HEADER, "s3cre"),
            ("x-real-ip", "203.0.113.9"),
        ]);
        assert_eq!(
            policy.client_ip(&bad).map(|i| i.to_string()).as_deref(),
            Some("198.51.100.7")
        );
    }

    /// 可信来源下 CDN 头优先于 XFF；无任何头则回落对端。
    #[test]
    fn trusted_cdn_header_wins_and_peer_is_last_resort() {
        let policy = TrustPolicy::new(&["127.0.0.1/32"], None);
        let headers = hm(&[
            (PEER_IP_HEADER, "127.0.0.1"),
            ("cf-connecting-ip", "203.0.113.9"),
            ("x-forwarded-for", "198.51.100.4"),
        ]);
        assert_eq!(
            policy.client_ip(&headers).map(|i| i.to_string()).as_deref(),
            Some("203.0.113.9")
        );
        assert_eq!(
            policy
                .client_ip(&hm(&[(PEER_IP_HEADER, "127.0.0.1")]))
                .map(|i| i.to_string())
                .as_deref(),
            Some("127.0.0.1")
        );
    }
}
