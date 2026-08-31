//! 权限点常量（IMPLEMENTATION §6.2）：`{资源}.{动作}[.{范围}]`。
//!
//! 语义：super_admin 全通过；admin 未绑定自定义角色 = 全权（new-api 迁移零成本）；
//! 绑定自定义角色 = 仅集合内权限点（`*` 为全权通配）。
//! own/all 资源属主范围（channel.write.own 等）随渠道属主过滤在 M2 后续批接入。

pub const CHANNEL_READ: &str = "channel.read";
pub const CHANNEL_WRITE: &str = "channel.write";
/// 定价配置只读（模型/分组/套餐/兑换码/规则列表）——与写分离，支撑只读运营角色。
pub const PRICING_READ: &str = "pricing.read";
pub const PRICING_WRITE: &str = "pricing.write";
pub const PRICING_PUBLISH: &str = "pricing.publish";
pub const USER_BALANCE_ADJUST: &str = "user.balance_adjust";
/// 用户与令牌只读（列表/搜索/详情）。
pub const USER_READ: &str = "user.read";
pub const USER_MANAGE: &str = "user.manage";
/// 账务与统计只读（账单明细 + 看板 KPI/维度聚合/趋势，统计一律走 ClickHouse）。
pub const BILLING_READ: &str = "billing.read";
/// 角色管理：额外强制 super_admin（防 admin 自我提权，见 console::admin）。
pub const ROLE_MANAGE: &str = "role.manage";
/// 系统设置只读（设置页加载；敏感键在 console 层脱敏）。
pub const SETTINGS_READ: &str = "settings.read";
pub const SETTINGS_WRITE: &str = "settings.write";
/// 按日志退款/批量退款（#1790-10）。
pub const BILLING_REFUND: &str = "billing.refund";
/// 代客查看用户令牌/分组/余额（#1790-2，强审计）。
pub const USER_ASSIST: &str = "user.assist";
pub const CACHE_FLUSH: &str = "cache.flush";
/// MCP 写工具总闸（叠加资源权限与 settings.mcp_write_enabled，§7.3 三道闸）。
pub const MCP_WRITE: &str = "mcp.write";

/// 全部权限点清单：由 `GET /admin/permissions` 暴露给前端角色编辑器，
/// 避免前端硬编码字符串与后端漂移。新增权限点必须同步登记到此处（有用例把关）。
pub const ALL: &[&str] = &[
    CHANNEL_READ,
    CHANNEL_WRITE,
    PRICING_READ,
    PRICING_WRITE,
    PRICING_PUBLISH,
    USER_READ,
    USER_BALANCE_ADJUST,
    USER_MANAGE,
    USER_ASSIST,
    BILLING_READ,
    BILLING_REFUND,
    ROLE_MANAGE,
    SETTINGS_READ,
    SETTINGS_WRITE,
    CACHE_FLUSH,
    MCP_WRITE,
];

#[cfg(test)]
mod tests {
    use super::ALL;

    /// 清单不得重复，且必须与本模块声明的权限点常量数量一致——
    /// 新增常量却忘记登记进 ALL 会导致前端角色编辑器漏项。
    #[test]
    fn all_permissions_are_unique_and_complete() {
        let mut sorted = ALL.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "ALL 中存在重复权限点");
        let declared = include_str!("permissions.rs")
            .lines()
            .filter(|l| l.starts_with("pub const ") && l.contains(": &str ="))
            .count();
        assert_eq!(ALL.len(), declared, "新增权限点后请同步登记进 ALL");
    }
}
