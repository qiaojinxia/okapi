//! 错误响应契约：`{"error":{"message":"<code>","type":"okapi_error","code":"<code>"}}`。
//! message 字段同置为 error_code（OpenAI 客户端要求非空 message；i18n 红线禁自然语言）。

use serde::Serialize;

/// 稳定错误码（前端 errors 命名空间做 i18n 映射）。
pub mod codes {
    pub const INVALID_API_KEY: &str = "invalid_api_key";
    pub const KEY_DISABLED: &str = "key_disabled";
    /// 当前操作要求浏览器携带有效的邮箱/密码登录会话 Cookie，但请求未带会话 Cookie。
    pub const SESSION_REQUIRED: &str = "session_required";
    /// 请求携带的网页登录会话已失效（已登出、被踢下线或过期）。
    pub const SESSION_EXPIRED: &str = "session_expired";
    /// 要删除 / 停用的正是这次请求自己用的 key（网页登录换来的那把或粘贴登录的那把）：
    /// 删了当前会话立刻失效，只能换一把 key 登录后再处理。
    pub const CURRENT_KEY_IN_USE: &str = "current_key_in_use";
    pub const MODEL_NOT_ALLOWED: &str = "model_not_allowed";
    pub const MODEL_NOT_FOUND: &str = "model_not_found";
    pub const INSUFFICIENT_QUOTA: &str = "insufficient_quota";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const NO_AVAILABLE_CHANNEL: &str = "no_available_channel";
    /// 模型在当前池有可用渠道，但不支持该入口；param 给出可用接口路径。
    pub const UNSUPPORTED_ENDPOINT: &str = "unsupported_endpoint";
    pub const EMPTY_COMPLETION: &str = "empty_completion";
    /// 客户端在拿到响应前断开：预扣已全额退回、不计费（失败日志里的 error_code，nginx 499 语义）。
    pub const CLIENT_CLOSED_REQUEST: &str = "client_closed_request";
    pub const UPSTREAM_ERROR: &str = "upstream_error";
    pub const UPSTREAM_TIMEOUT: &str = "upstream_timeout";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const INTERNAL_ERROR: &str = "internal_error";
    pub const STATS_DISABLED: &str = "stats_disabled";
    pub const PERMISSION_DENIED: &str = "permission_denied";
    pub const MEMBER_LIMIT_EXCEEDED: &str = "member_limit_exceeded";
    pub const KEY_QUOTA_EXCEEDED: &str = "key_quota_exceeded";
    pub const NOT_FOUND: &str = "not_found";
    /// 用户给 key 选的分组不在其可选集合内（组可能存在，只是他没资格选）。
    pub const GROUP_NOT_SELECTABLE: &str = "group_not_selectable";
    /// 分组还没进价簿：新建的分组要发布一次定价才能用（param = 分组码）。
    pub const GROUP_NOT_PUBLISHED: &str = "group_not_published";
    /// 来源 IP 不在该 key 的白名单内（param = 识别到的来源 IP，便于用户对照）。
    pub const IP_NOT_ALLOWED: &str = "ip_not_allowed";
    /// 该账户此刻正在结算，账目在动——对账修复要等它稳定下来再来（隔几秒重试）。
    pub const RECONCILE_UNSTABLE: &str = "reconcile_unstable";
    /// 报价单价超过请求声明的 `provider.max_price` 上限（param = 超限的那一轴与实际单价；
    /// 按次计价模型没有单价可比，param = `per_call:` 加本次按次实付价）。
    pub const PRICE_ABOVE_MAX: &str = "price_above_max";
    pub const SERVER_TOOL_UNPRICED: &str = "server_tool_unpriced";
    /// 请求里的工具转成可用上游的协议后会被丢掉，而渠道的工具转换策略不接受（IMPLEMENTATION §11.46）。
    pub const TOOL_CONVERSION_UNSUPPORTED: &str = "tool_conversion_unsupported";
    /// 该模型的候选渠道都只接受真实 Claude Code 客户端（渠道 `client_profile.native_only`），
    /// 本次请求不是。
    pub const CLAUDE_CODE_CLIENT_REQUIRED: &str = "claude_code_client_required";
    /// 该模型的候选渠道都只接受 Codex 官方客户端（渠道 `extensions.codex_client`），请求不是
    /// 官方客户端或版本不在窗口里；param 是 `official_client` / `client_version` / `min_version:x` / `max_version:x`。
    pub const CODEX_CLIENT_REQUIRED: &str = "codex_client_required";
    /// 该分组对此模型的全部候选渠道都因负毛利被熔断（IMPLEMENTATION §11.34；param = 摘掉的候选数）。
    pub const MARGIN_BLOCKED: &str = "margin_blocked";
    /// 本进程的后台结算积压超过上界（IMPLEMENTATION §12.2）：数据面在鉴权前拒绝，
    /// 未预扣、未碰上游，客户端稍后重试即可（param = 当前积压笔数）。
    pub const OVERLOADED: &str = "overloaded";
    /// 登录 key 要核对所属登录会话，但会话存储（Redis）暂不可达：服务端故障，不是凭证无效，稍后重试。
    pub const AUTH_BACKEND_UNAVAILABLE: &str = "auth_backend_unavailable";

    /// 需要网页登录会话的操作缺少/失效会话时返回的错误码（不代表 Bearer API Key 失效）。
    pub const SESSION_AUTH: &[&str] = &[SESSION_REQUIRED, SESSION_EXPIRED];

    /// Bearer API Key 已吊销/停用/无效的错误码（前端收到后清本地登录态回登录页）。
    pub const KEY_REVOKED: &[&str] = &[INVALID_API_KEY, KEY_DISABLED];

    /// `(Rust 常量名, 线缆 error_code)` 全集，供 TypeScript 契约代码生成使用。
    pub const NAMED: &[(&str, &str)] = &[
        ("INVALID_API_KEY", INVALID_API_KEY),
        ("KEY_DISABLED", KEY_DISABLED),
        ("SESSION_REQUIRED", SESSION_REQUIRED),
        ("SESSION_EXPIRED", SESSION_EXPIRED),
        ("CURRENT_KEY_IN_USE", CURRENT_KEY_IN_USE),
        ("MODEL_NOT_ALLOWED", MODEL_NOT_ALLOWED),
        ("MODEL_NOT_FOUND", MODEL_NOT_FOUND),
        ("INSUFFICIENT_QUOTA", INSUFFICIENT_QUOTA),
        ("RATE_LIMITED", RATE_LIMITED),
        ("NO_AVAILABLE_CHANNEL", NO_AVAILABLE_CHANNEL),
        ("UNSUPPORTED_ENDPOINT", UNSUPPORTED_ENDPOINT),
        ("EMPTY_COMPLETION", EMPTY_COMPLETION),
        ("CLIENT_CLOSED_REQUEST", CLIENT_CLOSED_REQUEST),
        ("UPSTREAM_ERROR", UPSTREAM_ERROR),
        ("UPSTREAM_TIMEOUT", UPSTREAM_TIMEOUT),
        ("BAD_REQUEST", BAD_REQUEST),
        ("INTERNAL_ERROR", INTERNAL_ERROR),
        ("STATS_DISABLED", STATS_DISABLED),
        ("PERMISSION_DENIED", PERMISSION_DENIED),
        ("MEMBER_LIMIT_EXCEEDED", MEMBER_LIMIT_EXCEEDED),
        ("KEY_QUOTA_EXCEEDED", KEY_QUOTA_EXCEEDED),
        ("NOT_FOUND", NOT_FOUND),
        ("GROUP_NOT_SELECTABLE", GROUP_NOT_SELECTABLE),
        ("GROUP_NOT_PUBLISHED", GROUP_NOT_PUBLISHED),
        ("IP_NOT_ALLOWED", IP_NOT_ALLOWED),
        ("RECONCILE_UNSTABLE", RECONCILE_UNSTABLE),
        ("PRICE_ABOVE_MAX", PRICE_ABOVE_MAX),
        ("SERVER_TOOL_UNPRICED", SERVER_TOOL_UNPRICED),
        ("TOOL_CONVERSION_UNSUPPORTED", TOOL_CONVERSION_UNSUPPORTED),
        ("CLAUDE_CODE_CLIENT_REQUIRED", CLAUDE_CODE_CLIENT_REQUIRED),
        ("CODEX_CLIENT_REQUIRED", CODEX_CLIENT_REQUIRED),
        ("MARGIN_BLOCKED", MARGIN_BLOCKED),
        ("OVERLOADED", OVERLOADED),
        ("AUTH_BACKEND_UNAVAILABLE", AUTH_BACKEND_UNAVAILABLE),
    ];

    /// 全部稳定错误码列表（与 `NAMED` 一一对应）。
    pub const ALL: &[&str] = &[
        INVALID_API_KEY,
        KEY_DISABLED,
        SESSION_REQUIRED,
        SESSION_EXPIRED,
        CURRENT_KEY_IN_USE,
        MODEL_NOT_ALLOWED,
        MODEL_NOT_FOUND,
        INSUFFICIENT_QUOTA,
        RATE_LIMITED,
        NO_AVAILABLE_CHANNEL,
        UNSUPPORTED_ENDPOINT,
        EMPTY_COMPLETION,
        CLIENT_CLOSED_REQUEST,
        UPSTREAM_ERROR,
        UPSTREAM_TIMEOUT,
        BAD_REQUEST,
        INTERNAL_ERROR,
        STATS_DISABLED,
        PERMISSION_DENIED,
        MEMBER_LIMIT_EXCEEDED,
        KEY_QUOTA_EXCEEDED,
        NOT_FOUND,
        GROUP_NOT_SELECTABLE,
        GROUP_NOT_PUBLISHED,
        IP_NOT_ALLOWED,
        RECONCILE_UNSTABLE,
        PRICE_ABOVE_MAX,
        SERVER_TOOL_UNPRICED,
        TOOL_CONVERSION_UNSUPPORTED,
        CLAUDE_CODE_CLIENT_REQUIRED,
        CODEX_CLIENT_REQUIRED,
        MARGIN_BLOCKED,
        OVERLOADED,
        AUTH_BACKEND_UNAVAILABLE,
    ];
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorDetail {
    /// = code（不携带自然语言）。
    pub message: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ErrorBody {
    #[must_use]
    pub fn new(code: &str, param: Option<String>, request_id: Option<String>) -> Self {
        Self {
            error: ErrorDetail {
                message: code.to_owned(),
                kind: "okapi_error",
                code: code.to_owned(),
                param,
                request_id,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::codes::{ALL, NAMED};

    #[test]
    fn all_error_codes_are_unique_and_complete() {
        let mut sorted = ALL.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "ALL 中存在重复 error_code");
        let declared = include_str!("error.rs")
            .lines()
            .filter(|l| l.trim_start().starts_with("pub const ") && l.contains(": &str ="))
            .count();
        assert_eq!(
            ALL.len(),
            declared,
            "新增错误码后请同步登记进 codes::ALL / NAMED"
        );
        assert_eq!(NAMED.len(), ALL.len());
    }
}
