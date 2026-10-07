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
    /// Deliberately required in cached JSON: pre-budget auth entries must be reloaded.
    pub quota_limited: bool,
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
    /// 生效分组的每用户分钟 / 小时请求上限（`price_groups.rpm_limit / rph_limit`，§11.32）；
    /// 随鉴权缓存下发，热路径不查库。None = 不限。
    #[serde(default)]
    pub group_rpm_limit: Option<i32>,
    #[serde(default)]
    pub group_rph_limit: Option<i32>,
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
    /// 登录 key（网页登录经 /auth/session-key 换来）所属会话 sid 的 sha256 十六进制；None = 普通 key。
    /// 登录 key 只在请求带着这条会话的 cookie、且会话仍有效时可用（gateway::auth 校验）。
    #[serde(default)]
    pub session_hash: Option<String>,
}

impl AuthedKey {
    /// Acting identity is independent of the shared billing wallet.
    #[must_use]
    pub fn actor_user_id(&self) -> i64 {
        self.member_user_id.unwrap_or(self.user_id)
    }
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
        self.permission_scope(permission) == PermScope::All
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
#[allow(clippy::too_many_lines)] // One joined identity snapshot, including delegated team ownership.
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
                         WHERE ug.user_id = u.id ORDER BY ug.priority DESC, ug.group_code LIMIT 1),
                       (SELECT pg2.group_code FROM price_groups pg2 WHERE pg2.is_default
                         ORDER BY pg2.group_code LIMIT 1),
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
               r.admin_permissions AS "admin_permissions?",
               r.multiplier_scaled AS "multiplier_scaled!",
               r.rpm_limit, r.tpm_limit, r.rpd_limit, r.max_concurrency,
               r.model_allowlist,
               r.ip_allowlist,
               r.expires_at,
               r.member_user_id,
               r.member_monthly_limit_micro,
               r.group_code AS "group_code!",
               pg.rpm_limit AS "group_rpm_limit?",
               pg.rph_limit AS "group_rph_limit?",
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

    let (quota_limited, session_hash) = if let Some(row) = &row {
        sqlx::query_as::<_, (bool, Option<String>)>(
            "SELECT quota_mode = 1, session_hash FROM api_keys WHERE id = $1",
        )
        .bind(row.key_id)
        .fetch_one(pool)
        .await?
    } else {
        (false, None)
    };
    // Team keys authenticate the member as well as the wallet. Removing a
    // membership or banning its user must disable their delegated credentials.
    let member = if let Some(r) = &row
        && let Some(id) = r.member_user_id
    {
        let found: Option<(i16, i16, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT u.status,u.role,ar.permissions FROM users u JOIN team_members tm ON tm.member_user_id=u.id AND tm.team_user_id=$1 LEFT JOIN admin_roles ar ON ar.id=u.admin_role_id WHERE u.id=$2 AND u.deleted_at IS NULL"
        ).bind(r.user_id).bind(id).fetch_optional(pool).await?;
        let Some(found) = found else { return Ok(None) };
        Some(found)
    } else {
        None
    };
    row.map(|r| {
        let (member_status, role, policy) = member.unwrap_or((1, r.role, r.admin_permissions));
        Ok(AuthedKey {
            key_id: r.key_id,
            quota_limited,
            user_id: r.user_id,
            key_status: r.key_status,
            user_status: if member_status == 1 {
                r.user_status
            } else {
                member_status
            },
            role,
            permissions: parse_policy(policy)?,
            pool_code: Some(r.pool_code),
            pool_strategy: r.pool_strategy,
            pool_fallback: r.pool_fallback,
            group_code: r.group_code,
            group_rpm_limit: r.group_rpm_limit,
            group_rph_limit: r.group_rph_limit,
            multiplier_scaled: r.multiplier_scaled,
            rpm_limit: r.rpm_limit,
            tpm_limit: r.tpm_limit,
            rpd_limit: r.rpd_limit,
            max_concurrency: r.max_concurrency,
            model_allowlist: r.model_allowlist,
            ip_allowlist: parse_policy(r.ip_allowlist)?,
            expires_at: r.expires_at,
            member_user_id: r.member_user_id,
            member_monthly_limit_micro: r.member_monthly_limit_micro,
            session_hash,
        })
    })
    .transpose()
}

fn parse_policy(value: Option<serde_json::Value>) -> Result<Option<Vec<String>>, StoreError> {
    value
        .map(serde_json::from_value::<Vec<String>>)
        .transpose()
        .map_err(|_| StoreError::InvalidData("auth_policy_invalid"))
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn corrupt_ip_and_permission_policies_fail_closed() {
        for policy in [
            serde_json::json!("broken"),
            serde_json::json!(["valid", 7]),
            serde_json::json!({}),
        ] {
            assert!(parse_policy(Some(policy)).is_err());
        }
        assert_eq!(
            parse_policy(Some(serde_json::json!([]))).unwrap(),
            Some(vec![])
        );
    }
    #[test]
    fn own_scope_does_not_authorize_global_operations() {
        let key:AuthedKey=serde_json::from_value(serde_json::json!({"key_id":1,"user_id":1,"key_status":1,"quota_limited":false,"user_status":1,"role":10,"permissions":["channel.write.own"],"group_code":"default","multiplier_scaled":1_000_000})).unwrap();
        assert_eq!(key.permission_scope("channel.write"), PermScope::Own);
        assert!(!key.has_permission("channel.write"));
    }
}
