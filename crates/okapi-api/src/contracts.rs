//! 强类型控制面契约（`channels.settings` / `settings_patch`）与零依赖 TypeScript 契约生成。
//!
//! 让 Rust 后端作为唯一真源：渠道配置结构、权限点全集与稳定错误码清单直接编译期/单测校验
//! 并生成 `frontend/src/lib/contracts.gen.ts`，避免正则扫描或前后端定义漂移。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const TOOL_LOSS_POLICIES: &[&str] = &["safe", "strict", "allow"];

pub const OVERRIDE_OPS: &[&str] = &[
    "set",
    "delete",
    "move",
    "copy",
    "replace",
    "regex_replace",
    "set_header",
    "delete_header",
    "return_error",
];

pub const OVERRIDE_MATCHERS: &[&str] = &[
    "eq",
    "neq",
    "prefix",
    "suffix",
    "contains",
    "regex",
    "exists",
    "missing",
    "gt",
    "gte",
    "lt",
    "lte",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideOpDto {
    Set,
    Delete,
    Move,
    Copy,
    Replace,
    RegexReplace,
    SetHeader,
    DeleteHeader,
    ReturnError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideMatcherDto {
    Eq,
    Neq,
    Prefix,
    Suffix,
    Contains,
    Regex,
    Exists,
    Missing,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideConditionDto {
    pub field: String,
    pub matcher: OverrideMatcherDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestOverrideRuleDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<OverrideConditionDto>,
    pub op: OverrideOpDto,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLossPolicyDto {
    Safe,
    Strict,
    Allow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToggleMode {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageStreamUsageDto {
    Cumulative,
    PerImage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalPeriodDto {
    Total,
    Day,
    Week,
    Month,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LowBalanceActionDto {
    Notify,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LowBalanceDto {
    pub threshold_micro: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<LowBalanceActionDto>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexClientGateDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name")]
pub enum ClientProfileDto {
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "claude-code")]
    ClaudeCode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entrypoint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_class: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_only: Option<bool>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelExtensionsDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_profile: Option<ClientProfileDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_client: Option<CodexClientGateDto>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelSettingsDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_control: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_to_content: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bill_by_response_model: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip_request_fields: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses_native: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_loss_policy: Option<ToolLossPolicyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance_refresh: Option<ToggleMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_probe: Option<ToggleMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_stream_usage: Option<ImageStreamUsageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_balance: Option<LowBalanceDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_request_fields: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_overrides: Option<Vec<RequestOverrideRuleDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ChannelExtensionsDto>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 字段级合并 `channels.settings`：
/// - 顶层 `null` 删除对应键，其余非空值覆盖同级键；
/// - 嵌套配置段 `extensions` 与 `account_control` 按二级键合并（二级 `null` 删除对应子键），
///   确保只改 `account_control` 时不会误抹掉同渠道的 `extensions.client_profile` / `oauth_token_url`；
/// - 字典类映射（`extra_headers` / `inject_request_fields`）传对象则整表替换、传 `null` 删除。
pub fn merge_channel_settings_patch(base: &Value, patch: &Value) -> Result<Value, &'static str> {
    let Some(patch_obj) = patch.as_object() else {
        return Err("settings_patch");
    };
    let mut merged = base.as_object().cloned().unwrap_or_default();
    for (key, val) in patch_obj {
        if val.is_null() {
            merged.remove(key);
            continue;
        }
        if matches!(key.as_str(), "extensions" | "account_control")
            && let Some(sub_patch) = val.as_object()
        {
            let mut sub_merged = merged
                .get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (sub_key, sub_val) in sub_patch {
                if sub_val.is_null() {
                    sub_merged.remove(sub_key);
                } else {
                    sub_merged.insert(sub_key.clone(), sub_val.clone());
                }
            }
            merged.insert(key.clone(), Value::Object(sub_merged));
            continue;
        }
        merged.insert(key.clone(), val.clone());
    }
    Ok(Value::Object(merged))
}

fn push_string_list(out: &mut String, items: &[&str], indent: &str) {
    for item in items {
        let _ = writeln!(out, "{indent}'{item}',");
    }
}

/// 渲染 `frontend/src/lib/contracts.gen.ts` 内容（纯 ASCII 注释，供前端静态类型检查直接消费）。
#[must_use]
pub fn render_typescript() -> String {
    let mut out = String::new();
    out.push_str(
        "// Generated by okapi-api::contracts::render_typescript; keep in sync with Rust contracts.\n\n",
    );

    out.push_str("export const ERROR_CODES = {\n");
    for (ident, wire) in crate::error::codes::NAMED {
        let _ = writeln!(out, "  {ident}: '{wire}',");
    }
    out.push_str("} as const\n\n");
    out.push_str("export type ErrorCode = (typeof ERROR_CODES)[keyof typeof ERROR_CODES]\n\n");

    out.push_str("export const SESSION_AUTH_CODES: ReadonlySet<string> = new Set([\n");
    push_string_list(&mut out, crate::error::codes::SESSION_AUTH, "  ");
    out.push_str("])\n\n");

    out.push_str("export const KEY_REVOKED_CODES: ReadonlySet<string> = new Set([\n");
    push_string_list(&mut out, crate::error::codes::KEY_REVOKED, "  ");
    out.push_str("])\n\n");

    out.push_str("export const PERMISSIONS = [\n");
    push_string_list(&mut out, crate::permissions::ALL, "  ");
    out.push_str("] as const\n\n");
    out.push_str("export type PermissionCode = (typeof PERMISSIONS)[number]\n\n");

    out.push_str("export const TOOL_LOSS_POLICIES = [\n");
    push_string_list(&mut out, TOOL_LOSS_POLICIES, "  ");
    out.push_str("] as const\n");
    out.push_str("export type ToolLossPolicy = (typeof TOOL_LOSS_POLICIES)[number]\n\n");

    out.push_str("export const OVERRIDE_OPS = [\n");
    push_string_list(&mut out, OVERRIDE_OPS, "  ");
    out.push_str("] as const\n");
    out.push_str("export type OverrideOp = (typeof OVERRIDE_OPS)[number]\n\n");

    out.push_str("export const OVERRIDE_MATCHERS = [\n");
    push_string_list(&mut out, OVERRIDE_MATCHERS, "  ");
    out.push_str("] as const\n");
    out.push_str("export type OverrideMatcher = (typeof OVERRIDE_MATCHERS)[number]\n\n");

    out.push_str(
        "export interface OverrideCondition {\n  field: string\n  matcher: OverrideMatcher\n  value?: unknown\n}\n\n",
    );
    out.push_str(
        "export interface RequestOverrideRule {\n  name?: string\n  enabled: boolean\n  conditions?: OverrideCondition[]\n  op: OverrideOp\n  path?: string\n  from?: string\n  value?: unknown\n  pattern?: string\n  status?: number\n}\n\n",
    );

    out.push_str("export type LocalPeriod = 'total' | 'day' | 'week' | 'month'\n\n");

    out.push_str(
        "export interface CodexClientGate {\n  official_only?: boolean\n  min_version?: string\n  max_version?: string\n}\n\n",
    );
    out.push_str(
        "export interface LowBalanceRule {\n  threshold_micro: number\n  action?: 'notify' | 'stop'\n}\n\n",
    );
    out.push_str(
        "export interface AccountControl {\n  usage?: { period?: 'hour' | 'day' | 'week' | 'month'; requests?: number | null; tokens?: number | null; cost_micro?: number | null }\n  quota_aware: boolean\n  quota_threshold_pct: number\n  quota_limits?: Record<string, number>\n  quota_keep_limits?: Record<string, number>\n  quota_forecast?: 'off' | 'shadow' | 'enforce'\n  local_tokens?: { cap: number; period?: LocalPeriod } | null\n  local_cost?: { cap_micro: number; period?: LocalPeriod } | null\n  rate_limit_cooldown_secs: number\n  failure_threshold: number\n  failure_cooldown_secs: number\n  refresh_mode: 'managed' | 'external'\n  refresh_margin_secs: number\n}\n\n",
    );
    out.push_str(
        "export type ClientProfile =\n  | { name: 'native' }\n  | {\n      name: 'claude-code'\n      mode: 'auto' | 'passthrough' | 'mimic'\n      revision?: string\n      entrypoint?: 'cli' | 'sdk-cli'\n      request_class?: 'main' | 'auxiliary'\n      native_only?: boolean\n    }\n\n",
    );
    out.push_str(
        "export interface ChannelExtensions {\n  client_profile?: ClientProfile\n  codex_client?: CodexClientGate\n}\n\n",
    );
    out.push_str(
        "export interface ChannelSettings {\n  account_control?: AccountControl\n  thinking_to_content: boolean\n  bill_by_response_model: boolean\n  strip_request_fields: string[]\n  responses_native?: boolean\n  tool_loss_policy?: ToolLossPolicy\n  api_version?: string\n  aws_region?: string\n  oauth_token_url?: string\n  balance_refresh?: 'on' | 'off'\n  health_probe?: 'on' | 'off'\n  image_stream_usage?: 'cumulative' | 'per_image'\n  low_balance?: LowBalanceRule\n  extra_headers?: Record<string, string>\n  inject_request_fields?: Record<string, unknown>\n  request_overrides?: RequestOverrideRule[]\n  extensions?: ChannelExtensions\n}\n\n",
    );
    out.push_str(
        "export interface ChannelSettingsPatch {\n  account_control?: Partial<{ [K in keyof AccountControl]: AccountControl[K] | null }> | null\n  thinking_to_content?: boolean | null\n  bill_by_response_model?: boolean | null\n  strip_request_fields?: string[] | null\n  responses_native?: boolean | null\n  tool_loss_policy?: ToolLossPolicy | null\n  api_version?: string | null\n  aws_region?: string | null\n  oauth_token_url?: string | null\n  balance_refresh?: 'on' | 'off' | null\n  health_probe?: 'on' | 'off' | null\n  image_stream_usage?: 'cumulative' | 'per_image' | null\n  low_balance?: LowBalanceRule | null\n  extra_headers?: Record<string, string> | null\n  inject_request_fields?: Record<string, unknown> | null\n  request_overrides?: RequestOverrideRule[] | null\n  extensions?: {\n    client_profile?: ClientProfile | null\n    codex_client?: CodexClientGate | null\n  } | null\n}\n",
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_updates_and_removes_fields_without_clobbering_siblings() {
        let base = json!({
            "oauth_token_url": "https://auth.example.com/token",
            "thinking_to_content": false,
            "extra_headers": {"X-One": "1", "X-Two": "2"},
            "extensions": {
                "client_profile": {"name": "claude-code", "mode": "auto"},
                "codex_client": {"official_only": true}
            },
            "account_control": {
                "failure_threshold": 3,
                "quota_aware": true
            }
        });
        let patch = json!({
            "thinking_to_content": true,
            "extra_headers": {"X-One": "updated"},
            "extensions": {
                "client_profile": {"name": "native"},
                "codex_client": null
            },
            "account_control": {
                "failure_threshold": 5
            }
        });
        let merged = merge_channel_settings_patch(&base, &patch).unwrap();
        assert_eq!(merged["oauth_token_url"], "https://auth.example.com/token");
        assert_eq!(merged["thinking_to_content"], true);
        assert_eq!(merged["extra_headers"], json!({"X-One": "updated"}));
        assert_eq!(
            merged["extensions"]["client_profile"],
            json!({"name": "native"})
        );
        assert!(merged["extensions"].get("codex_client").is_none());
        assert_eq!(merged["account_control"]["failure_threshold"], 5);
        assert_eq!(merged["account_control"]["quota_aware"], true);

        let dto: ChannelSettingsDto = serde_json::from_value(merged).unwrap();
        assert_eq!(dto.thinking_to_content, Some(true));
        assert_eq!(
            dto.oauth_token_url.as_deref(),
            Some("https://auth.example.com/token")
        );
        assert!(merge_channel_settings_patch(&base, &json!("bad")).is_err());
    }

    #[test]
    fn typescript_contracts_file_matches_rust_definitions() {
        let on_disk = include_str!("../../../frontend/src/lib/contracts.gen.ts");
        assert_eq!(on_disk, render_typescript());
    }
}
