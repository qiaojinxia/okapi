//! Claude Code 客户端身份的共用部分：稳定装机身份、`metadata.user_id`、`cc_version` 指纹。
//!
//! 请求整形（UA、beta、system 块、cch 签名）只在 [`crate::profiles`] 的最新客户端配置里，
//! 由渠道的 `extensions.client_profile` 选用。这里只放与版本无关、跨请求必须稳定的身份。

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `cc_version` 指纹盐：真实 CLI 抓包逆出的常量（Sub2API `FINGERPRINT_SALT` 同源）。
/// 改了就与官方 CLI 对不上，上游判第三方。
const FINGERPRINT_SALT: &str = "59cf53e54c78";

/// billing attribution 块文本的前缀：上游按它认出 system 数组里的计费归因块。
pub const BILLING_PREFIX: &str = "x-anthropic-billing-header:";

/// `count_tokens` 在客户端 beta 之上追加的 beta。
pub const BETA_TOKEN_COUNTING: &str = "token-counting-2024-11-01";

/// 客户端配置自己生成的身份头（小写）。出向前从透传 `extra_headers` 里摘掉——
/// reqwest 的 `header()` 是追加不是覆盖，留着会发出两行同名头。
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

/// 一把订阅 key 的稳定伪装身份。种子派生、无需存库：同一把 key 恒同一套指纹，
/// 重启 / 多副本一致；重新登录换 key 则换指纹（与 Sub2API 每账号一套的语义一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MimicIdentity {
    /// 装机 id（64 hex）：对应真实 CLI 的本机安装标识，跨请求不漂移。
    pub device_id: String,
    /// 换码时记下的账号 UUID（凭证 `account_id`）；缺省空串（user_id 各格式都允许空）。
    pub account_uuid: String,
    /// 模拟的 CLI 版本（取自客户端配置）。
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

    /// 会话 UUID（v4 形态）：种子 = device_id +（有则）下游用户 + 首条 user 消息文本。对话只在尾部追加，
    /// 这几样跨轮不变 → 同一会话稳定；同一用户的不同对话开场白相同时仍会碰撞（Sub2API
    /// `buildStableSessionSeed` 同思路；无状态代理无法恢复真实 CLI 的进程级随机 UUID，只能这样近似）。
    fn session_uuid(&self, scope: Option<&str>, first_user_text: &str) -> String {
        let seed = match scope {
            Some(scope) => format!("{}::{scope}::{first_user_text}", self.device_id),
            None => format!("{}::{first_user_text}", self.device_id),
        };
        uuid_from_sha256(Sha256::digest(seed))
    }

    /// `metadata.user_id`：CLI ≥ 2.1.78 的 JSON 字符串格式。显式会话 ID 优先。
    pub(crate) fn metadata_user_id_for_session(
        &self,
        first_user_text: &str,
        session_id: Option<&str>,
        scope: Option<&str>,
    ) -> String {
        // 手工拼：`json!` 会按字母序排键，真机是 device_id → account_uuid → session_id。
        let session =
            session_id.map_or_else(|| self.session_uuid(scope, first_user_text), str::to_owned);
        format!(
            r#"{{"device_id":{},"account_uuid":{},"session_id":{}}}"#,
            Value::from(self.device_id.as_str()),
            Value::from(self.account_uuid.as_str()),
            Value::from(session),
        )
    }
}

/// `cc_version` 指纹：`sha256(salt + 用户输入第 4/7/20 字符(不足补 '0') + 版本)` 前 3 位 hex。
/// 用户输入取第一条 user 消息里第一个不是 `<system-reminder>` 的文本块：2.1.290 会在
/// 用户原话前插入提醒块，抓包里的指纹按原话算。
/// 索引按 JavaScript UTF-16 code unit 计算；单独选中代理项时按 U+FFFD 编码。
/// 不能用 Rust 字符或 UTF-8 字节索引替代，含 emoji 的输入会产生不同指纹。
#[must_use]
pub fn cc_fingerprint(body: &[u8], cli_version: &str) -> String {
    let first = first_user_text(body);
    let mut chars = String::new();
    let units: Vec<u16> = first.encode_utf16().collect();
    for i in [4usize, 7, 20] {
        chars.push(units.get(i).map_or('0', |unit| {
            char::from_u32(u32::from(*unit)).unwrap_or(char::REPLACEMENT_CHARACTER)
        }));
    }
    let sum = Sha256::digest(format!("{FINGERPRINT_SALT}{chars}{cli_version}"));
    hex::encode(sum)[..3].to_owned()
}

/// 摘要前 16 字节 → UUID v4 形态（版本/变体位手工置位；仓库不引 uuid 依赖）。
fn uuid_from_sha256(sum: impl AsRef<[u8]>) -> String {
    uuid_from_bytes(sum.as_ref()[..16].try_into().expect("digest 至少 16 字节"))
}

pub(crate) fn uuid_from_bytes(bytes: &[u8; 16]) -> String {
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

/// 第一条 user 消息的用户原话（兼容 string 与 block 数组两种 content，跳过提醒块）。
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
                if let Some(text) = blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .find(|text| !text.starts_with("<system-reminder>"))
                {
                    return text.to_owned();
                }
            }
            _ => {}
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const VERSION: &str = "2.1.290";

    fn identity() -> MimicIdentity {
        MimicIdentity::from_seed("42", Some("acc-uuid-1"), VERSION)
    }

    #[test]
    fn identity_is_deterministic_and_shaped() {
        let a = MimicIdentity::from_seed("42", None, VERSION);
        let b = MimicIdentity::from_seed("42", None, VERSION);
        let c = MimicIdentity::from_seed("43", None, VERSION);
        assert_eq!(a.device_id, b.device_id, "同种子同指纹");
        assert_ne!(a.device_id, c.device_id, "不同种子不同指纹");
        assert_eq!(a.device_id.len(), 64, "装机 id 是 64 hex");
        assert!(a.device_id.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn session_uuid_stable_within_conversation_varies_across() {
        let id = identity();
        let s1 = id.session_uuid(None, "hello world");
        assert_eq!(
            s1,
            id.session_uuid(None, "hello world"),
            "同一会话 session 稳定"
        );
        assert_ne!(
            id.session_uuid(None, "another conversation"),
            s1,
            "不同会话互异"
        );
        let mine = id.session_uuid(Some("7"), "hello world");
        assert_eq!(
            mine,
            id.session_uuid(Some("7"), "hello world"),
            "同一用户同一会话稳定"
        );
        assert_ne!(
            mine,
            id.session_uuid(Some("8"), "hello world"),
            "开场白相同的不同用户不撞"
        );
        let parts: Vec<&str> = s1.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
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
        let uid = identity().metadata_user_id_for_session("hi", None, None);
        let v: Value = serde_json::from_str(&uid).unwrap();
        assert_eq!(v["device_id"], identity().device_id);
        assert_eq!(v["account_uuid"], "acc-uuid-1");
        assert!(v["session_id"].as_str().is_some_and(|s| s.len() == 36));
        let explicit = identity().metadata_user_id_for_session("hi", Some("s"), Some("7"));
        assert_eq!(
            serde_json::from_str::<Value>(&explicit).unwrap()["session_id"],
            "s"
        );
    }

    /// 指纹值取自 2.1.290 隔离抓包：主请求在原话前带提醒块，辅助请求只有一个块。
    #[test]
    fn fingerprint_matches_captured_2_1_290_requests() {
        let main = br##"{"messages":[{"role":"user","content":[
            {"type":"text","text":"<system-reminder>\nAttribution for git commits\n</system-reminder>\n"},
            {"type":"text","text":"Reply with OK"}]},
            {"role":"system","content":[{"type":"text","text":"# Environment"}]}]}"##;
        assert_eq!(cc_fingerprint(main, VERSION), "23d");
        let print =
            br#"{"messages":[{"role":"user","content":"Reply exactly OK. Do not use tools."}]}"#;
        assert_eq!(cc_fingerprint(print, VERSION), "fe6");
        let auxiliary = "<session>\nReply with OK\n</session>\n\nWrite the title in the predominant language of the session \u{2014} a stray word or code token in another language doesn't change it, and neither does the English of these instructions.";
        let auxiliary =
            json!({"messages":[{"role":"user","content":[{"type":"text","text":auxiliary}]}]});
        assert_eq!(
            cc_fingerprint(auxiliary.to_string().as_bytes(), VERSION),
            "8eb"
        );
    }

    #[test]
    fn fingerprint_binds_body_chars_and_version() {
        let body = r#"{"messages":[{"role":"user","content":"abcdefghijklmnopqrstuvw"}]}"#;
        // 首条 user 文本 "abcdefghijklmnopqrstuvw"：第 4/7/20 字符是 e/h/u
        let f1 = cc_fingerprint(body.as_bytes(), VERSION);
        assert_eq!(f1.len(), 3);
        assert_ne!(
            cc_fingerprint(body.as_bytes(), "2.1.291"),
            f1,
            "版本进 hash"
        );
        let at7 = body.replace("abcdefghijklmnopqrstuvw", "abcdefgXijklmnopqrstuvw");
        assert_ne!(
            cc_fingerprint(at7.as_bytes(), VERSION),
            f1,
            "第 7 字符进 hash"
        );
        let at0 = body.replace("abcdefghijklmnopqrstuvw", "Xbcdefghijklmnopqrstuvw");
        assert_eq!(
            cc_fingerprint(at0.as_bytes(), VERSION),
            f1,
            "第 0 字符不进 hash"
        );
        let short = br#"{"messages":[{"role":"user","content":"ab"}]}"#;
        assert_eq!(cc_fingerprint(short, VERSION).len(), 3, "不足补 '0'");
    }
}
