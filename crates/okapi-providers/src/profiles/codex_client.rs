//! Codex 渠道「仅限官方客户端」（渠道 `extensions.codex_client`，对应 Claude 那边的
//! `client_profile.native_only`）：订阅账号只接 Codex 官方客户端的请求，第三方客户端跳过这条渠道。
//!
//! 只看请求头，三个条件都要满足（对照 Sub2API `codex_cli_only`，前缀表与 codex-rs 的
//! `is_first_party_originator` 一致，并用本机 Codex 0.160 二进制核过）：
//! - UA 以官方前缀开头（`codex_exec/0.160.0 (Mac OS …)` 这类），或 `originator` 是官方取值；
//! - 带至少一个 `x-codex-` 头（`x-codex-window-id` / `x-codex-turn-metadata` 等，真机几乎必带）；
//! - 配了版本窗口时，UA 里能解析出版本且落在窗口内。
//!
//! 这是路由规则，不是鉴权：抄全这些头的客户端照样通过，挡住的是没有刻意伪装的第三方。
use http::HeaderMap;
use serde::{Deserialize, Serialize};

/// 官方客户端的 UA 前缀（小写比较，只做前缀匹配，不做子串兜底）。
const OFFICIAL_USER_AGENTS: &[&str] = &[
    "codex_cli_rs/",
    "codex-tui/",
    "codex_exec/",
    "codex_vscode/",
    "codex_vscode_copilot/",
    "codex_app/",
    "codex_chatgpt_desktop/",
    "codex_atlas/",
    "codex_sdk_ts/",
];

/// 官方客户端的 `originator` 取值（精确匹配）；另认 `Codex ` 开头的桌面端家族。
const OFFICIAL_ORIGINATORS: &[&str] = &[
    "codex_cli_rs",
    "codex-tui",
    "codex_exec",
    "codex_vscode",
    "codex_vscode_copilot",
    "codex_app",
    "codex_chatgpt_desktop",
    "codex_atlas",
    "codex_sdk_ts",
];

/// 渠道配置：`settings.extensions.codex_client`。
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexClientGate {
    /// 只接 Codex 官方客户端。
    #[serde(default)]
    pub official_only: bool,
    /// 最低客户端版本（`x.y.z`，含）；None = 不限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    /// 最高客户端版本（`x.y.z`，含）；None = 不限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_version: Option<String>,
}

/// 请求看起来出自哪个 Codex 官方客户端；不是官方客户端时为 None。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexClient {
    /// UA 里的版本（`codex_exec/0.160.0` → 0.160.0）；UA 不是官方前缀时（只凭 originator 认出）为 None。
    pub version: Option<Version>,
}

pub type Version = (u64, u64, u64);

/// 不放行的原因，作为 403 的 param 给客户端看。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// 不是官方客户端。
    NotOfficial,
    /// 配了版本窗口但认不出版本。
    VersionUnknown,
    /// 版本低于下限（给出下限）。
    TooOld(String),
    /// 版本高于上限（给出上限）。
    TooNew(String),
}

impl Refusal {
    #[must_use]
    pub fn param(&self) -> String {
        match self {
            Self::NotOfficial => "official_client".to_owned(),
            Self::VersionUnknown => "client_version".to_owned(),
            Self::TooOld(min) => format!("min_version:{min}"),
            Self::TooNew(max) => format!("max_version:{max}"),
        }
    }
}

impl CodexClientGate {
    /// 配置是否生效（只填版本没开开关也按开关算：版本窗口只对官方客户端有意义）。
    #[must_use]
    pub fn enforced(&self) -> bool {
        self.official_only || self.min_version.is_some() || self.max_version.is_some()
    }

    /// 写入时校验：版本必须是 `x.y.z`，且下限不高于上限。
    pub fn validate(&self) -> Result<(), &'static str> {
        let min = self.min_version.as_deref().map(parse_version);
        let max = self.max_version.as_deref().map(parse_version);
        if matches!(min, Some(None)) {
            return Err("min_version");
        }
        if matches!(max, Some(None)) {
            return Err("max_version");
        }
        if let (Some(Some(min)), Some(Some(max))) = (min, max)
            && min > max
        {
            return Err("max_version");
        }
        Ok(())
    }

    /// 这个客户端能不能用这条渠道。
    pub fn admits(&self, client: Option<&CodexClient>) -> Result<(), Refusal> {
        if !self.enforced() {
            return Ok(());
        }
        let client = client.ok_or(Refusal::NotOfficial)?;
        if self.min_version.is_none() && self.max_version.is_none() {
            return Ok(());
        }
        let version = client.version.ok_or(Refusal::VersionUnknown)?;
        if let Some(min) = &self.min_version
            && parse_version(min).is_some_and(|min| version < min)
        {
            return Err(Refusal::TooOld(min.clone()));
        }
        if let Some(max) = &self.max_version
            && parse_version(max).is_some_and(|max| version > max)
        {
            return Err(Refusal::TooNew(max.clone()));
        }
        Ok(())
    }
}

/// 按请求头认出 Codex 官方客户端。
#[must_use]
pub fn detect(headers: &HeaderMap) -> Option<CodexClient> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_ascii_lowercase())
    };
    let user_agent = header("user-agent").unwrap_or_default();
    let official_ua = OFFICIAL_USER_AGENTS
        .iter()
        .find(|prefix| user_agent.starts_with(*prefix));
    let official_originator = header("originator").is_some_and(|originator| {
        OFFICIAL_ORIGINATORS.contains(&originator.as_str()) || originator.starts_with("codex ")
    });
    if official_ua.is_none() && !official_originator {
        return None;
    }
    if !headers
        .keys()
        .any(|name| name.as_str().starts_with("x-codex-"))
    {
        return None;
    }
    let version = official_ua.and_then(|prefix| {
        let rest = &user_agent[prefix.len()..];
        parse_version(rest.split(|c: char| c.is_whitespace() || c == '(').next()?)
    });
    Some(CodexClient { version })
}

/// `x.y.z`（可带 `-pre` 等后缀，只取前三段数字）。
fn parse_version(raw: &str) -> Option<Version> {
    let core = raw.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, value.parse().unwrap());
        }
        headers
    }

    const CLI: &str = "codex_exec/0.160.0 (Mac OS 26.5.1; arm64) unknown (codex_exec; 0.160.0)";

    #[test]
    fn official_clients_need_an_official_identity_and_a_codex_header() {
        let real = headers(&[
            ("user-agent", CLI),
            ("originator", "codex_exec"),
            ("x-codex-window-id", "w"),
        ]);
        assert_eq!(
            detect(&real),
            Some(CodexClient {
                version: Some((0, 160, 0))
            })
        );
        // 只有 originator（UA 被改过）也算官方，但没有版本
        let by_originator = headers(&[
            ("user-agent", "Go-http-client/1.1"),
            ("originator", "codex-tui"),
            ("x-codex-turn-metadata", "{}"),
        ]);
        assert_eq!(detect(&by_originator), Some(CodexClient { version: None }));
        assert!(
            detect(&headers(&[
                ("user-agent", "x"),
                ("originator", "Codex Desktop"),
                ("x-codex-window-id", "w")
            ]))
            .is_some()
        );
        // 缺 x-codex- 头、UA 只是含 codex、originator 伪造前缀，都不算
        assert_eq!(
            detect(&headers(&[
                ("user-agent", CLI),
                ("originator", "codex_exec")
            ])),
            None
        );
        assert_eq!(
            detect(&headers(&[
                ("user-agent", "my-codex_exec/1.0.0"),
                ("x-codex-window-id", "w")
            ])),
            None
        );
        assert_eq!(
            detect(&headers(&[
                ("user-agent", "curl/8"),
                ("originator", "evil-codex_exec"),
                ("x-codex-window-id", "w")
            ])),
            None
        );
        assert_eq!(
            detect(&headers(&[("user-agent", "OpenAI/Python 1.0")])),
            None
        );
    }

    #[test]
    fn version_window_applies_only_when_configured() {
        let gate = |min: Option<&str>, max: Option<&str>| CodexClientGate {
            official_only: true,
            min_version: min.map(str::to_owned),
            max_version: max.map(str::to_owned),
        };
        let cli = CodexClient {
            version: Some((0, 160, 0)),
        };
        let unknown = CodexClient { version: None };
        assert_eq!(
            CodexClientGate::default().admits(None),
            Ok(()),
            "未开启不限制"
        );
        assert_eq!(gate(None, None).admits(None), Err(Refusal::NotOfficial));
        assert_eq!(gate(None, None).admits(Some(&unknown)), Ok(()));
        assert_eq!(
            gate(Some("0.150.0"), Some("0.160.0")).admits(Some(&cli)),
            Ok(())
        );
        assert_eq!(
            gate(Some("0.161.0"), None).admits(Some(&cli)),
            Err(Refusal::TooOld("0.161.0".into()))
        );
        assert_eq!(
            gate(None, Some("0.159.9")).admits(Some(&cli)),
            Err(Refusal::TooNew("0.159.9".into()))
        );
        assert_eq!(
            gate(Some("0.1.0"), None).admits(Some(&unknown)),
            Err(Refusal::VersionUnknown)
        );
        assert_eq!(
            Refusal::TooOld("0.161.0".into()).param(),
            "min_version:0.161.0"
        );
    }

    #[test]
    fn validation_rejects_bad_or_inverted_versions() {
        let gate = |min: &str, max: &str| CodexClientGate {
            official_only: true,
            min_version: Some(min.to_owned()),
            max_version: Some(max.to_owned()),
        };
        assert_eq!(gate("0.150.0", "0.160.0").validate(), Ok(()));
        assert_eq!(gate("0.150", "0.160.0").validate(), Err("min_version"));
        assert_eq!(gate("0.150.0", "latest").validate(), Err("max_version"));
        assert_eq!(gate("0.161.0", "0.160.0").validate(), Err("max_version"));
        assert_eq!(parse_version("0.160.0-alpha.1"), Some((0, 160, 0)));
    }
}
