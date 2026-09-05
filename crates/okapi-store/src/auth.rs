use crate::error::StoreError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// 资源权限范围（IMPLEMENTATION §6.2 own/all）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermScope {
    /// 全部资源。
    All,
    /// 仅属主资源（owner_id = 本人）。
    Own,
    /// 无权限。
    Denied,
}

/// 鉴权命中的 key 元数据（网关鉴权缓存的值对象；序列化进 Redis auth:key:*）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuthedKey {
    pub key_id: i64,
    pub user_id: i64,
    pub key_status: i16,
    pub user_status: i16,
    /// 1=user 10=admin 100=super_admin（对齐 new-api）。
    pub role: i16,
    /// 自定义子角色的权限点集合（admin_roles.permissions）；
    /// None = 未绑定自定义角色（admin 默认全权，对齐 new-api 迁移习惯）。
    pub permissions: Option<Vec<String>>,
    /// 生效渠道池：api_keys.pool_override > 生效定价组的 price_groups.pool_code > default。
    /// 保留 Option 只为鉴权缓存的序列化兼容；解析后恒为 Some（缺省 `default`）。
    pub pool_code: Option<String>,
    /// 该池的选路策略（随鉴权缓存一起带下来，热路径不再查库）。
    pub pool_strategy: Option<String>,
    /// 主池对某模型无候选时退到的池（channel_pools.fallback_pool_code，单跳）。
    #[serde(default)]
    pub pool_fallback: Option<String>,
    /// 生效定价分组：key 分组覆盖 > 用户最高优先级组 > 默认组。
    pub group_code: String,
    /// users.price_multiplier × 1e6（定点，避免浮点穿透计费路径）。
    pub multiplier_scaled: i64,
    pub rpm_limit: Option<i32>,
    pub tpm_limit: Option<i32>,
    pub rpd_limit: Option<i32>,
    pub max_concurrency: Option<i32>,
    pub model_allowlist: Option<serde_json::Value>,
    /// key 级 IP 白名单（地址或 CIDR 字符串；None/空 = 不限）。此前列在库里、网关从不读。
    #[serde(default)]
    pub ip_allowlist: Option<Vec<String>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// 团 key：归属成员（分账与限额锚点；None = 非团 key）。
    pub member_user_id: Option<i64>,
    /// 成员月度限额（micro；None = 不限或非团 key）。
    pub member_monthly_limit_micro: Option<i64>,
}

impl AuthedKey {
    #[must_use]
    pub fn is_usable(&self, now: DateTime<Utc>) -> bool {
        self.key_status == 1 && self.user_status == 1 && self.expires_at.is_none_or(|at| at > now)
    }

    /// 管理面身份：admin(10)/super_admin(100)。
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.role >= 10
    }

    /// 权限点检查（IMPLEMENTATION §6.2）：
    /// super_admin 全通过；admin 未绑定自定义角色 = 全权；
    /// 绑定自定义角色 = 集合内命中（支持 `*` 通配全权点）。普通用户一律拒绝。
    #[must_use]
    pub fn has_permission(&self, permission: &str) -> bool {
        !matches!(self.permission_scope(permission), PermScope::Denied)
    }

    /// 带资源范围的权限判定（#6267）：`{base}` = 全部资源，`{base}.own` = 仅属主资源。
    #[must_use]
    pub fn permission_scope(&self, base: &str) -> PermScope {
        if self.role >= 100 {
            return PermScope::All;
        }
        if self.role < 10 {
            return PermScope::Denied;
        }
        match &self.permissions {
            None => PermScope::All,
            Some(points) => {
                if points.iter().any(|p| p == "*" || p == base) {
                    PermScope::All
                } else if points.iter().any(|p| p.strip_suffix(".own") == Some(base)) {
                    PermScope::Own
                } else {
                    PermScope::Denied
                }
            }
        }
    }

    /// 有序池链：主池 → 降级池（去重）。候选查询与 custom_pass 点查都吃这个。
    #[must_use]
    pub fn pool_chain(&self) -> Vec<&str> {
        let primary = self
            .pool_code
            .as_deref()
            .unwrap_or(crate::channels::DEFAULT_POOL);
        let mut chain = vec![primary];
        if let Some(fb) = self.pool_fallback.as_deref()
            && fb != primary
        {
            chain.push(fb);
        }
        chain
    }

    /// IP 白名单检查：未配置 / 空清单 = 不限；配置了则来源 IP 必须命中，
    /// 拿不到来源 IP 一律拒绝（fail-closed：既然配了白名单，"不知道从哪来"就是不在名单上）。
    #[must_use]
    pub fn allows_ip(&self, ip: Option<std::net::IpAddr>) -> bool {
        match self.ip_allowlist.as_deref() {
            None | Some([]) => true,
            Some(list) => ip.is_some_and(|ip| crate::netmatch::allowed(list, ip)),
        }
    }

    /// 模型白名单检查（null = 不限）。
    #[must_use]
    pub fn allows_model(&self, model: &str) -> bool {
        match &self.model_allowlist {
            None => true,
            Some(list) => list
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(model))),
        }
    }
}

/// 按 key 哈希查找（SHA-256 hex；明文不落库）。
pub async fn find_key_by_hash(
    pool: &PgPool,
    key_hash: &str,
) -> Result<Option<AuthedKey>, StoreError> {
    // 用 CTE 先定生效分组，再由它推出池：池默认跟随生效定价分组，
    // 这样"改分组"同时改价与改可用上游，符合直觉；key 可用 pool_override 单独钉住。
    let row = sqlx::query!(
        r#"
        WITH resolved AS (
            SELECT k.id AS key_id,
                   k.user_id,
                   k.status AS key_status,
                   u.status AS user_status,
                   u.role,
                   ar.permissions AS admin_permissions,
                   (u.price_multiplier * 1000000)::bigint AS multiplier_scaled,
                   k.rpm_limit, k.tpm_limit, k.rpd_limit, k.max_concurrency,
                   k.model_allowlist,
                   k.ip_allowlist,
                   k.expires_at,
                   k.member_user_id,
                   k.pool_override,
                   tm.monthly_spend_limit_micro AS member_monthly_limit_micro,
                   COALESCE(
                       k.group_override,
                       (SELECT ug.group_code FROM user_groups ug
                         WHERE ug.user_id = u.id ORDER BY ug.priority DESC LIMIT 1),
                       (SELECT pg2.group_code FROM price_groups pg2 WHERE pg2.is_default LIMIT 1),
                       'default'
                   ) AS group_code
            FROM api_keys k
            JOIN users u ON u.id = k.user_id
            LEFT JOIN admin_roles ar ON ar.id = u.admin_role_id
            LEFT JOIN team_members tm
                   ON tm.team_user_id = k.user_id AND tm.member_user_id = k.member_user_id
            WHERE k.key_hash = $1 AND k.deleted_at IS NULL AND u.deleted_at IS NULL
        )
        SELECT r.key_id AS "key_id!",
               r.user_id AS "user_id!",
               r.key_status AS "key_status!",
               r.user_status AS "user_status!",
               r.role AS "role!",
               r.admin_permissions,
               r.multiplier_scaled AS "multiplier_scaled!",
               r.rpm_limit, r.tpm_limit, r.rpd_limit, r.max_concurrency,
               r.model_allowlist,
               r.ip_allowlist,
               r.expires_at,
               r.member_user_id,
               r.member_monthly_limit_micro,
               r.group_code AS "group_code!",
               COALESCE(r.pool_override, pg.pool_code, 'default') AS "pool_code!",
               cp.routing_strategy AS "pool_strategy?",
               cp.fallback_pool_code AS "pool_fallback?"
        FROM resolved r
        LEFT JOIN price_groups pg ON pg.group_code = r.group_code
        LEFT JOIN channel_pools cp
               ON cp.pool_code = COALESCE(r.pool_override, pg.pool_code, 'default')
        "#,
        key_hash
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| AuthedKey {
        key_id: r.key_id,
        user_id: r.user_id,
        key_status: r.key_status,
        user_status: r.user_status,
        role: r.role,
        permissions: r
            .admin_permissions
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok()),
        pool_code: Some(r.pool_code),
        pool_strategy: r.pool_strategy,
        pool_fallback: r.pool_fallback,
        group_code: r.group_code,
        multiplier_scaled: r.multiplier_scaled,
        rpm_limit: r.rpm_limit,
        tpm_limit: r.tpm_limit,
        rpd_limit: r.rpd_limit,
        max_concurrency: r.max_concurrency,
        model_allowlist: r.model_allowlist,
        ip_allowlist: r
            .ip_allowlist
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok()),
        expires_at: r.expires_at,
        member_user_id: r.member_user_id,
        member_monthly_limit_micro: r.member_monthly_limit_micro,
    }))
}
