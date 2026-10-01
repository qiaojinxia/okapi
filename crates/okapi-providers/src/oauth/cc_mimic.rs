//! Claude Code 全伪装（可选：`channels.settings.mimic_cc` 开启才生效，IMPLEMENTATION §11.38）。
//!
//! 透传模式（缺省）假设前面站着真实 Claude Code，身份头由客户端自带、网关只搬运。mimic
//! 模式面向"前面不是官方客户端"的场景：网关替客户端编一套与真实 CLI 对齐、且跨请求稳定的
//! 身份（HTTP 头 + system 块 + `metadata.user_id`）。这是对抗性工程——官方 CLI 升级、上游
//! 收紧检测后，版本常量 / beta 集合 / 指纹算法都可能要跟着改。各项逐一对照 Sub2API v0.2.2
//! （`cc_mimicry` / identity service / billing block）与真实 CLI 抓包结论，2026-09。
//!
//! 刻意未做（Sub2API 有、此处不做，理由见各处）：CLI 版本热跟随（用设置覆写代替）、
//! temperature / max_tokens 缺省补齐、cache 断点重排、工具名混淆。

use super::anthropic_max::SYSTEM_PREFIX;
use crate::error::UpstreamError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 内置伪装 CLI 版本基线（三段 semver）。真实 CLI 会持续更新，过低版本会被上游判非正版；
/// 可用 `settings.mimic_cc_version` 覆写，升级时改这里或改设置。
pub const MIMIC_CLI_VERSION: &str = "2.1.258";

/// `cc_version` 指纹盐：真实 CLI 抓包逆出的常量（Sub2API `FINGERPRINT_SALT` 同源）。
/// 改了就与官方 CLI 对不上，上游判第三方。
pub const FINGERPRINT_SALT: &str = "59cf53e54c78";

/// billing attribution 块文本的前缀：上游按它认出 system 数组里的计费归因块。
pub const BILLING_PREFIX: &str = "x-anthropic-billing-header:";

/// mimic 模式的全量 beta 集合：对齐真实 CLI 的完整流量（顺序即抓包顺序）。
/// 上游按"官方 Claude Code 请求才会带的完整集合"判来源，缺项会被降级到第三方额度。
/// 与透传路径的 [`super::anthropic_max::REQUIRED_BETAS`]（三件套）的区别就在这里。
pub const FULL_BETAS: [&str; 9] = [
    "claude-code-20250219",
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
    "prompt-caching-scope-2026-01-05",
    "effort-2025-11-24",
    "context-management-2025-06-27",
    "thinking-binding-controls-2026-08-01",
    "mid-conversation-output-config-2026-07-01",
    "extended-cache-ttl-2025-04-11",
];

/// `count_tokens` 在全量之上追加的 beta。
pub const BETA_TOKEN_COUNTING: &str = "token-counting-2024-11-01";

/// 一把订阅 key 的稳定伪装身份。种子派生、无需存库：同一把 key 恒同一套指纹，
/// 重启 / 多副本一致；重新登录换 key 则换指纹（与 Sub2API 每账号一套的语义一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MimicIdentity {
    /// 装机 id（64 hex）：对应真实 CLI 的本机安装标识，跨请求不漂移。
    pub device_id: String,
    /// 换码时记下的账号 UUID（凭证 `account_id`）；缺省空串（user_id 各格式都允许空）。
    pub account_uuid: String,
    /// 伪装的 CLI 版本（`settings.mimic_cc_version` 覆写或内置基线）。
    pub cli_version: String,
}

impl MimicIdentity {
    /// 从稳定种子派生。`seed` 用 channel_key_id 即可；`account_uuid` 来自换码响应。
    #[must_use]
    pub fn from_seed(seed: &str, account_uuid: Option<&str>, cli_version: &str) -> Self {
        Self {
            device_id: hex::encode(Sha256::digest(format!("okapi-cc-mimic:{seed}"))),
            account_uuid: account_uuid.unwrap_or_default().to_owned(),
            cli_version: cli_version.to_owned(),
        }
    }

    /// 伪装 User-Agent。版本号必须与 billing 块的 `cc_version` 同源同值。
    #[must_use]
    pub fn user_agent(&self) -> String {
        format!("claude-cli/{} (external, cli)", self.cli_version)
    }

    /// 会话 UUID（v4 形态）：种子 = device_id + 首条 user 消息文本。对话只在尾部追加，
    /// 这两样跨轮不变 → 同一会话稳定、不同会话互异（Sub2API `buildStableSessionSeed`
    /// 同思路；无状态代理无法恢复真实 CLI 的进程级随机 UUID，只能这样近似）。
    fn session_uuid(&self, first_user_text: &str) -> String {
        uuid_from_sha256(Sha256::digest(format!(
            "{}::{}",
            self.device_id, first_user_text
        )))
    }

    /// `metadata.user_id`：CLI ≥ 2.1.78 的 JSON 字符串格式（旧版是 `user_…_account_…_session_…`
    /// 拼接串，两种上游都收）。
    fn metadata_user_id(&self, first_user_text: &str) -> String {
        json!({
            "device_id": self.device_id,
            "account_uuid": self.account_uuid,
            "session_id": self.session_uuid(first_user_text),
        })
        .to_string()
    }
}

/// `cc_version` 指纹：`sha256(salt + 首条 user 文本第 4/7/20 字符(不足补 '0') + 版本)` 前 3 位 hex。
/// 算法与盐都逐字节对齐真实 CLI（Sub2API `compute_fingerprint` 复刻自同一来源），
/// 任何偏差都会让 `cc_version=X.Y.Z.{fp}` 与官方对不上。
#[must_use]
pub fn cc_fingerprint(body: &[u8], cli_version: &str) -> String {
    let first = first_user_text(body);
    let mut chars = String::new();
    for i in [4usize, 7, 20] {
        chars.push(first.chars().nth(i).unwrap_or('0'));
    }
    let sum = Sha256::digest(format!("{FINGERPRINT_SALT}{chars}{cli_version}"));
    hex::encode(sum)[..3].to_owned()
}

/// billing attribution 块文本：`x-anthropic-billing-header: cc_version=X.Y.Z.{fp}; cc_entrypoint=cli;`
/// 新版 CLI 已不发 cch 签名，这里同样不发。块本身不带 cache_control（与真实 CLI 一致）。
#[must_use]
pub fn billing_text(body: &[u8], identity: &MimicIdentity) -> String {
    format!(
        "{BILLING_PREFIX} cc_version={}.{fp}; cc_entrypoint=cli;",
        identity.cli_version,
        fp = cc_fingerprint(body, &identity.cli_version),
    )
}

/// 全量 beta 合并：必备集合在前、客户端自带的去重追加。
#[must_use]
pub fn merge_full_betas(existing: Option<&str>) -> String {
    merge_betas(&FULL_BETAS, existing)
}

/// `count_tokens` 的全量 beta：全量集合 + token-counting。
#[must_use]
pub fn merge_count_betas(existing: Option<&str>) -> String {
    let mut required: Vec<&str> = FULL_BETAS.to_vec();
    required.push(BETA_TOKEN_COUNTING);
    merge_betas(&required, existing)
}

fn merge_betas(required: &[&str], existing: Option<&str>) -> String {
    let mut parts: Vec<&str> = required.to_vec();
    for p in existing
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !parts.contains(&p) {
            parts.push(p);
        }
    }
    parts.join(",")
}

/// forge 头覆盖的全部键名（小写）。mimic 路径在出向前把它们从透传 `extra_headers`
/// 里摘掉——reqwest 的 `header()` 是追加不是覆盖，留着会发出两行同名头。
pub const FORGED_HEADER_KEYS: [&str; 13] = [
    "user-agent",
    "accept",
    "x-stainless-lang",
    "x-stainless-package-version",
    "x-stainless-os",
    "x-stainless-arch",
    "x-stainless-runtime",
    "x-stainless-runtime-version",
    "x-stainless-retry-count",
    "x-stainless-timeout",
    "x-stainless-helper-method",
    "x-app",
    "anthropic-dangerous-direct-browser-access",
];

/// 伪造的 CLI 身份头。`x-client-request-id` 每请求一个新 UUID（上游拿它做请求指纹，
/// 缺失或重复都可能触发第三方判定）；流式补 `x-stainless-helper-method: stream`。
/// 真 CLI 即使流式 `Accept` 也是 `application/json`——反直觉但有效的指纹点。
#[must_use]
pub fn forge_headers(identity: &MimicIdentity, stream: bool) -> Vec<(String, String)> {
    let mut headers = vec![
        ("user-agent".to_owned(), identity.user_agent()),
        ("x-stainless-lang".to_owned(), "js".to_owned()),
        (
            "x-stainless-package-version".to_owned(),
            "0.94.0".to_owned(),
        ),
        ("x-stainless-os".to_owned(), "Linux".to_owned()),
        ("x-stainless-arch".to_owned(), "arm64".to_owned()),
        ("x-stainless-runtime".to_owned(), "node".to_owned()),
        (
            "x-stainless-runtime-version".to_owned(),
            "v24.3.0".to_owned(),
        ),
        ("x-stainless-retry-count".to_owned(), "0".to_owned()),
        ("x-stainless-timeout".to_owned(), "600".to_owned()),
        ("x-app".to_owned(), "cli".to_owned()),
        (
            "anthropic-dangerous-direct-browser-access".to_owned(),
            "true".to_owned(),
        ),
        ("accept".to_owned(), "application/json".to_owned()),
        ("x-client-request-id".to_owned(), new_request_id()),
    ];
    if stream {
        headers.push(("x-stainless-helper-method".to_owned(), "stream".to_owned()));
    }
    headers
}

/// 从透传 `Outbound` 里摘掉 forge 覆盖的键，返回新的 `Outbound`。
#[must_use]
pub fn strip_forged_keys(mut outbound: crate::http::Outbound) -> crate::http::Outbound {
    outbound
        .extra_headers
        .retain(|(k, _)| !FORGED_HEADER_KEYS.contains(&k.to_ascii_lowercase().as_str()));
    outbound
}

/// 每请求一个新 UUID（v4 形态）：aws-lc-rs 随机 16 字节 + 版本/变体位。
fn new_request_id() -> String {
    let mut bytes = [0u8; 16];
    aws_lc_rs::rand::fill(&mut bytes).unwrap_or_default();
    uuid_from_bytes(&bytes)
}

/// 摘要前 16 字节 → UUID v4 形态（版本/变体位手工置位；仓库不引 uuid 依赖）。
fn uuid_from_sha256(sum: impl AsRef<[u8]>) -> String {
    uuid_from_bytes(sum.as_ref()[..16].try_into().expect("digest 至少 16 字节"))
}

fn uuid_from_bytes(bytes: &[u8; 16]) -> String {
    let mut b = *bytes;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&b[0..4]),
        hex::encode(&b[4..6]),
        hex::encode(&b[6..8]),
        hex::encode(&b[8..10]),
        hex::encode(&b[10..16])
    )
}

/// messages 里第一条 user 消息的首段 text（兼容 string 与 block 数组两种 content）。
fn first_user_text(body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return String::new();
    };
    let Some(messages) = v.get("messages").and_then(Value::as_array) else {
        return String::new();
    };
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        match msg.get("content") {
            Some(Value::String(s)) => return s.clone(),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        return t.to_owned();
                    }
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// mimic 路径的 body 整形（幂等）：
/// 1. system：字符串转数组；CLI 自述句不在首位则前置（带 `cache_control` ephemeral 5m，
///    与真实 CLI 一致——透传路径不带是因为真客户端自带）；
/// 2. billing 块：已有则重写 `cc_version`（版本覆写 / 升级后重同步），没有则插在首句之后；
/// 3. `metadata.user_id`：客户端带了就保留（真实值比合成的更真，Sub2API 注入路径同策略），
///    缺失才注入伪装值。
///
/// # Errors
/// body 不是合法 JSON / 不是对象时返回 [`UpstreamError::Build`]。
pub fn prepare_body(body: &[u8], identity: &MimicIdentity) -> Result<Vec<u8>, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };

    let mut system: Vec<Value> = match obj.remove("system") {
        Some(Value::String(s)) if s.trim().is_empty() => Vec::new(),
        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    };
    let cc_block = json!({
        "type": "text",
        "text": SYSTEM_PREFIX,
        "cache_control": {"type": "ephemeral", "ttl": "5m"},
    });
    let already = system
        .first()
        .and_then(|b| b.get("text").and_then(Value::as_str))
        .is_some_and(|t| t.starts_with(SYSTEM_PREFIX));
    if !already {
        system.insert(0, cc_block);
    } else if system[0].get("cache_control").is_none() {
        system[0]["cache_control"] = json!({"type": "ephemeral", "ttl": "5m"});
    }

    let billing = billing_text(body, identity);
    let billing_idx = system.iter().position(|b| {
        b.get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with(BILLING_PREFIX))
    });
    match billing_idx {
        Some(i) => system[i]["text"] = json!(billing),
        None => system.insert(1, json!({"type": "text", "text": billing})),
    }
    obj.insert("system".to_owned(), Value::Array(system));

    let needs_user_id = obj
        .get("metadata")
        .and_then(|m| m.get("user_id"))
        .and_then(Value::as_str)
        .is_none_or(|s| s.trim().is_empty());
    if needs_user_id {
        let uid = identity.metadata_user_id(&first_user_text(body));
        obj.entry("metadata".to_owned())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| UpstreamError::Build("metadata_not_object".to_owned()))?
            .insert("user_id".to_owned(), json!(uid));
    }

    serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> MimicIdentity {
        MimicIdentity::from_seed("42", Some("acc-uuid-1"), MIMIC_CLI_VERSION)
    }

    #[test]
    fn identity_is_deterministic_and_shaped() {
        let a = MimicIdentity::from_seed("42", None, MIMIC_CLI_VERSION);
        let b = MimicIdentity::from_seed("42", None, MIMIC_CLI_VERSION);
        let c = MimicIdentity::from_seed("43", None, MIMIC_CLI_VERSION);
        assert_eq!(a.device_id, b.device_id, "同种子同指纹");
        assert_ne!(a.device_id, c.device_id, "不同种子不同指纹");
        assert_eq!(a.device_id.len(), 64, "装机 id 是 64 hex");
        assert!(a.device_id.bytes().all(|b| b.is_ascii_hexdigit()));

        let ua = a.user_agent();
        assert_eq!(ua, "claude-cli/2.1.258 (external, cli)");
        assert!(ua.contains(&a.cli_version));
    }

    #[test]
    fn session_uuid_stable_within_conversation_varies_across() {
        let id = identity();
        let s1 = id.session_uuid("hello world");
        let s2 = id.session_uuid("hello world");
        assert_eq!(s1, s2, "同一会话（首条 user 文本不变）session 稳定");
        assert_ne!(
            id.session_uuid("another conversation"),
            s1,
            "不同会话 session 互异"
        );
        let parts: Vec<&str> = s1.split('-').collect();
        assert_eq!(
            [
                parts[0].len(),
                parts[1].len(),
                parts[2].len(),
                parts[3].len(),
                parts[4].len()
            ],
            [8, 4, 4, 4, 12],
            "UUID 形态"
        );
        assert_eq!(parts[2].chars().next(), Some('4'), "版本位 v4");
        assert!(matches!(
            parts[3].chars().next(),
            Some('8' | '9' | 'a' | 'b')
        ));
    }

    #[test]
    fn metadata_user_id_is_json_with_three_fields() {
        let uid = identity().metadata_user_id("hi");
        let v: Value = serde_json::from_str(&uid).unwrap();
        assert_eq!(v["device_id"], identity().device_id);
        assert_eq!(v["account_uuid"], "acc-uuid-1");
        assert!(v["session_id"].as_str().is_some_and(|s| s.len() == 36));
    }

    #[test]
    fn fingerprint_binds_body_chars_and_version() {
        let body = r#"{"messages":[{"role":"user","content":"abcdefghijklmnopqrstuvw"}]}"#;
        // 首条 user 文本 "abcdefghijklmnopqrstuvw"：第 4/7/20 字符是 e/h/u
        let f1 = cc_fingerprint(body.as_bytes(), "2.1.258");
        assert_eq!(f1.len(), 3);
        assert!(f1.bytes().all(|b| b.is_ascii_hexdigit()));
        // 指纹输入含版本：换版本换指纹
        assert_ne!(
            cc_fingerprint(body.as_bytes(), "2.1.259"),
            f1,
            "版本进 hash：升级后 fp 必须重算"
        );
        // 指纹输入含第 4/7/20 字符：等长替换，只动一个位置
        let at7 = body.replace("abcdefghijklmnopqrstuvw", "abcdefgXijklmnopqrstuvw");
        assert_ne!(
            cc_fingerprint(at7.as_bytes(), "2.1.258"),
            f1,
            "第 7 字符进 hash"
        );
        let at0 = body.replace("abcdefghijklmnopqrstuvw", "Xbcdefghijklmnopqrstuvw");
        assert_eq!(
            cc_fingerprint(at0.as_bytes(), "2.1.258"),
            f1,
            "第 0 字符不进 hash"
        );
        // 文本不足时补 '0' 仍可计算
        let short = br#"{"messages":[{"role":"user","content":"ab"}]}"#;
        assert_eq!(cc_fingerprint(short, MIMIC_CLI_VERSION).len(), 3);
    }

    #[test]
    fn billing_text_shape() {
        let body = br#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let text = billing_text(body, &identity());
        assert!(text.starts_with("x-anthropic-billing-header: cc_version=2.1.258."));
        assert!(text.ends_with("; cc_entrypoint=cli;"));
        let fp = text
            .strip_prefix("x-anthropic-billing-header: cc_version=2.1.258.")
            .unwrap()
            .strip_suffix("; cc_entrypoint=cli;")
            .unwrap();
        assert_eq!(fp, cc_fingerprint(body, MIMIC_CLI_VERSION));
    }

    #[test]
    fn betas_merge_dedup_keep_extras() {
        let full = FULL_BETAS.join(",");
        assert_eq!(merge_full_betas(None), full);
        assert_eq!(
            merge_full_betas(Some("oauth-2025-04-20, context-1m-2025-08-07")),
            format!("{full},context-1m-2025-08-07"),
            "必备项去重、附加项保留在后"
        );
        let counting = format!("{full},{BETA_TOKEN_COUNTING}");
        assert_eq!(merge_count_betas(None), counting);
        assert_eq!(
            merge_count_betas(Some(BETA_TOKEN_COUNTING)),
            counting,
            "客户端重复给的 token-counting 不重复"
        );
    }

    #[test]
    fn forged_headers_complete_and_fresh_request_id() {
        let h = forge_headers(&identity(), false);
        let keys: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        for key in FORGED_HEADER_KEYS
            .iter()
            .filter(|k| **k != "x-stainless-helper-method")
        {
            assert!(keys.contains(key), "缺 {key}");
        }
        assert!(!keys.contains(&"x-stainless-helper-method"), "非流式不带");
        let stream_h = forge_headers(&identity(), true);
        assert!(
            stream_h
                .iter()
                .any(|(k, v)| k == "x-stainless-helper-method" && v == "stream")
        );
        let rid1 = h.iter().find(|(k, _)| k == "x-client-request-id").unwrap();
        let rid2 = forge_headers(&identity(), false)
            .into_iter()
            .find(|(k, _)| k == "x-client-request-id")
            .unwrap();
        assert_ne!(rid1.1, rid2.1, "x-client-request-id 每请求一个新 UUID");
        assert_eq!(rid1.1.len(), 36);
    }

    #[test]
    fn strip_forged_keys_removes_only_those() {
        let outbound = crate::http::Outbound {
            proxy_url: None,
            extra_headers: vec![
                ("User-Agent".to_owned(), "python-sdk/1.0".to_owned()),
                ("x-app".to_owned(), "web".to_owned()),
                ("x-custom".to_owned(), "keep".to_owned()),
            ],
        };
        let stripped = strip_forged_keys(outbound);
        assert_eq!(
            stripped.extra_headers,
            vec![("x-custom".to_owned(), "keep".to_owned())],
            "forge 覆盖的键摘掉（大小写不敏感），其它保留"
        );
    }

    #[test]
    fn prepare_body_injects_system_billing_and_metadata() {
        let raw = br#"{"model":"m","max_tokens":10,"system":"be brief","messages":[{"role":"user","content":"hello mimic"}]}"#;
        let out = prepare_body(raw, &identity()).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let sys = v["system"].as_array().unwrap();
        assert_eq!(sys[0]["text"], SYSTEM_PREFIX, "自述句在首位");
        assert_eq!(
            sys[0]["cache_control"]["type"], "ephemeral",
            "mimic 首句带 cache_control（与真实 CLI 一致）"
        );
        assert!(
            sys[1]["text"].as_str().unwrap().starts_with(BILLING_PREFIX),
            "billing 块插在首句之后"
        );
        assert_eq!(sys[2]["text"], "be brief", "客户端 system 保留在后");
        let uid: Value = serde_json::from_str(v["metadata"]["user_id"].as_str().unwrap()).unwrap();
        assert_eq!(uid["device_id"], identity().device_id);

        // 幂等：再跑一遍结果不变（billing 重写不重复插入、user_id 不动、cache_control 不叠加）
        let again = prepare_body(&out, &identity()).unwrap();
        let v2: Value = serde_json::from_slice(&again).unwrap();
        assert_eq!(v, v2);
    }

    #[test]
    fn prepare_body_keeps_existing_user_id_and_resyncs_billing() {
        let raw = br#"{"model":"m","max_tokens":10,
            "metadata":{"user_id":"user_abc"},
            "system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=2.0.0.abc; cc_entrypoint=cli;"},
                      {"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."}],
            "messages":[{"role":"user","content":"hi"}]}"#;
        let out = prepare_body(raw, &identity()).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v["metadata"]["user_id"], "user_abc",
            "客户端已有的 user_id 不覆盖"
        );
        let sys = v["system"].as_array().unwrap();
        assert_eq!(sys[0]["text"], SYSTEM_PREFIX, "自述句不在首位时前置到首位");
        assert!(
            sys[1]["text"]
                .as_str()
                .unwrap()
                .starts_with("x-anthropic-billing-header: cc_version=2.1.258."),
            "已有 billing 块重写为当前版本指纹，不新增一行"
        );
        assert_eq!(
            sys.iter()
                .filter(|b| b["text"]
                    .as_str()
                    .is_some_and(|t| t.starts_with(BILLING_PREFIX)))
                .count(),
            1,
            "billing 块只有一条"
        );
    }

    #[test]
    fn prepare_body_rejects_non_object() {
        assert!(prepare_body(b"[1]", &identity()).is_err());
    }
}
