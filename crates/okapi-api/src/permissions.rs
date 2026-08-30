//! 权限点常量（IMPLEMENTATION §6.2）：`{资源}.{动作}[.{范围}]`。
//!
//! 语义：super_admin 全通过；admin 未绑定自定义角色 = 全权（new-api 迁移零成本）；
//! 绑定自定义角色 = 仅集合内权限点（`*` 为全权通配）。
//! own/all 资源属主范围（channel.write.own 等）随渠道属主过滤在 M2 后续批接入。

pub const CHANNEL_READ: &str = "channel.read";
pub const CHANNEL_WRITE: &str = "channel.write";
pub const PRICING_WRITE: &str = "pricing.write";
pub const PRICING_PUBLISH: &str = "pricing.publish";
pub const USER_BALANCE_ADJUST: &str = "user.balance_adjust";
pub const USER_MANAGE: &str = "user.manage";
pub const BILLING_READ: &str = "billing.read";
/// 角色管理：额外强制 super_admin（防 admin 自我提权，见 console::admin）。
pub const ROLE_MANAGE: &str = "role.manage";
pub const SETTINGS_WRITE: &str = "settings.write";
/// 按日志退款/批量退款（#1790-10）。
pub const BILLING_REFUND: &str = "billing.refund";
/// 代客查看用户令牌/分组/余额（#1790-2，强审计）。
pub const USER_ASSIST: &str = "user.assist";
pub const CACHE_FLUSH: &str = "cache.flush";
/// MCP 写工具总闸（叠加资源权限与 settings.mcp_write_enabled，§7.3 三道闸）。
pub const MCP_WRITE: &str = "mcp.write";
