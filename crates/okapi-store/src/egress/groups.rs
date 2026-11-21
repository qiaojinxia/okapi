use super::{
    Assignment, Binding, GroupInput, GroupRow, ReconcileReport, lock_assignments, reconcile_locked,
};
use crate::error::StoreError;
use crate::listing::{Page, Slice, count_unless_all, len_as_total};
use sqlx::{PgConnection, PgPool};

// ---- 代理组 ----

/// 新建或覆盖代理组（成员整组替换），同事务对账固定分配。
pub async fn upsert_group(
    pool: &PgPool,
    input: &GroupInput<'_>,
) -> Result<ReconcileReport, StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    sqlx::query!(
        r#"
        INSERT INTO proxy_groups (code, name, mode, owner_id, description)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (code) DO UPDATE SET
            name = EXCLUDED.name, mode = EXCLUDED.mode,
            description = EXCLUDED.description, updated_at = now()
        "#,
        input.code,
        input.name,
        input.mode,
        input.owner_id,
        input.description,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"DELETE FROM proxy_group_members WHERE group_code = $1"#,
        input.code
    )
    .execute(&mut *tx)
    .await?;
    let ids: Vec<i64> = input.members.iter().map(|m| m.proxy_id).collect();
    let priorities: Vec<i32> = input.members.iter().map(|m| m.priority).collect();
    let weights: Vec<i32> = input.members.iter().map(|m| m.weight).collect();
    sqlx::query!(
        r#"
        INSERT INTO proxy_group_members (group_code, proxy_id, priority, weight)
        SELECT $1, m.proxy_id, m.priority, m.weight
        FROM unnest($2::bigint[], $3::int[], $4::int[]) AS m(proxy_id, priority, weight)
        "#,
        input.code,
        &ids,
        &priorities,
        &weights,
    )
    .execute(&mut *tx)
    .await?;
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// 往组里追加成员（已在组里的跳过，priority 0 / weight 1），同事务对账。
pub async fn add_group_members(
    pool: &PgPool,
    code: &str,
    proxy_ids: &[i64],
) -> Result<ReconcileReport, StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    sqlx::query!(
        r#"
        INSERT INTO proxy_group_members (group_code, proxy_id)
        SELECT $1, id FROM unnest($2::bigint[]) AS m(id)
        ON CONFLICT (group_code, proxy_id) DO NOTHING
        "#,
        code,
        proxy_ids
    )
    .execute(&mut *tx)
    .await?;
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// 删代理组。被渠道直接绑定或是全局默认 → `Conflict`。
pub async fn delete_group(pool: &PgPool, code: &str) -> Result<bool, StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let refs = sqlx::query!(
        r#"
        SELECT
            (SELECT COUNT(*) FROM channels
              WHERE egress_group_code = $1 AND deleted_at IS NULL) AS "channels!",
            EXISTS (SELECT 1 FROM settings
                     WHERE key = 'egress_default' AND value ->> 'mode' = 'group'
                       AND value ->> 'group_code' = $1) AS "is_default!"
        "#,
        code
    )
    .fetch_one(&mut *tx)
    .await?;
    if refs.channels > 0 {
        return Err(StoreError::Conflict("proxy_group_in_use"));
    }
    if refs.is_default {
        return Err(StoreError::Conflict("proxy_group_is_default"));
    }
    sqlx::query!(
        r#"UPDATE channels SET egress_mode = NULL, egress_group_code = NULL
           WHERE egress_group_code = $1 AND deleted_at IS NOT NULL"#,
        code
    )
    .execute(&mut *tx)
    .await?;
    let deleted = sqlx::query!(r#"DELETE FROM proxy_groups WHERE code = $1"#, code)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(deleted)
}

/// 代理组属主（外层 None = 不存在）。
pub async fn group_owner(pool: &PgPool, code: &str) -> Result<Option<Option<i64>>, StoreError> {
    let row = sqlx::query!(r#"SELECT owner_id FROM proxy_groups WHERE code = $1"#, code)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.owner_id))
}

/// 代理组列表（成员内联；组的数量是配置级规模，不分页也无妨，但仍按 `slice` 切）。
pub async fn list_groups(
    pool: &PgPool,
    owner: Option<i64>,
    slice: Slice,
) -> Result<Page<GroupRow>, StoreError> {
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
            SELECT g.code, g.name, g.mode, g.owner_id, g.description, g.created_at, g.updated_at,
                   COALESCE((
                       SELECT jsonb_agg(jsonb_build_object(
                                  'proxy_id', p.id, 'name', p.name, 'status', p.status,
                                  'cooling', COALESCE(p.cooldown_until > now(), false),
                                  'priority', m.priority, 'weight', m.weight,
                                  'max_keys', p.max_keys,
                                  'assigned_keys', (
                                      SELECT COUNT(*) FROM channel_keys k
                                      JOIN channel_egress e ON e.channel_id = k.channel_id
                                      JOIN channels c ON c.id = k.channel_id
                                      WHERE k.egress_proxy_id = p.id AND e.mode = 'group'
                                        AND e.group_code = g.code AND c.deleted_at IS NULL))
                              ORDER BY m.priority DESC, p.id)
                       FROM proxy_group_members m JOIN proxies p ON p.id = m.proxy_id
                       WHERE m.group_code = g.code),
                       '[]'::jsonb) AS "members!",
                   (SELECT COUNT(*) FROM channels c
                     WHERE c.egress_group_code = g.code AND c.deleted_at IS NULL) AS "channel_count!",
                   (SELECT COUNT(*) FROM channel_keys k
                      JOIN channel_egress e ON e.channel_id = k.channel_id
                      JOIN channels c ON c.id = k.channel_id
                     WHERE g.mode = 'pinned' AND e.mode = 'group' AND e.group_code = g.code
                       AND c.deleted_at IS NULL AND k.egress_proxy_id IS NULL) AS "unassigned_keys!",
                   EXISTS (SELECT 1 FROM settings
                            WHERE key = 'egress_default' AND value ->> 'mode' = 'group'
                              AND value ->> 'group_code' = g.code) AS "is_default!"
            FROM proxy_groups g
            WHERE ($1::bigint IS NULL OR g.owner_id = $1)
            ORDER BY g.code
            LIMIT $2 OFFSET $3
            "#,
            owner,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        count_unless_all(
            slice,
            sqlx::query_scalar!(
                r#"SELECT COUNT(*) AS "c!" FROM proxy_groups g
                   WHERE ($1::bigint IS NULL OR g.owner_id = $1)"#,
                owner
            )
            .fetch_one(pool)
        ),
    )?;
    let total = total.unwrap_or_else(|| len_as_total(rows.len()));
    let data = rows
        .into_iter()
        .map(|r| GroupRow {
            code: r.code,
            name: r.name,
            mode: r.mode,
            owner_id: r.owner_id,
            description: r.description,
            members: serde_json::from_value(r.members).unwrap_or_default(),
            channel_count: r.channel_count,
            unassigned_keys: r.unassigned_keys,
            is_default: r.is_default,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();
    Ok(Page { data, total })
}

/// 有效出口为该组的全部 key 及其分配（含经全局默认继承的），供组详情展示与手动调整。
pub async fn group_assignments(pool: &PgPool, code: &str) -> Result<Vec<Assignment>, StoreError> {
    let rows = sqlx::query_as!(
        Assignment,
        r#"
        SELECT k.id AS key_id, c.id AS channel_id, c.name AS channel_name,
               k.egress_proxy_id AS proxy_id
        FROM channel_keys k
        JOIN channels c ON c.id = k.channel_id
        JOIN channel_egress e ON e.channel_id = c.id
        WHERE c.deleted_at IS NULL AND e.mode = 'group' AND e.group_code = $1
        ORDER BY c.id, k.id
        "#,
        code
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---- 绑定 ----

/// 设置渠道出口绑定并对账。None = 渠道不存在。
/// 绑定目标的存在性与可见性由调用方先校验（FK 只兜底存在性）。
pub async fn set_channel_binding(
    pool: &PgPool,
    channel_id: i64,
    binding: &Binding,
) -> Result<Option<ReconcileReport>, StoreError> {
    let (affected, report) = set_channels_binding(pool, &[channel_id], binding).await?;
    Ok((affected > 0).then_some(report))
}

/// 批量设置渠道出口绑定并对账（一个事务）。返回命中的渠道数。
pub async fn set_channels_binding(
    pool: &PgPool,
    channel_ids: &[i64],
    binding: &Binding,
) -> Result<(u64, ReconcileReport), StoreError> {
    let (mode, proxy_id, group_code) = binding.columns();
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let affected = sqlx::query!(
        r#"
        UPDATE channels
        SET egress_mode = $2, egress_proxy_id = $3, egress_group_code = $4, updated_at = now()
        WHERE id = ANY($1) AND deleted_at IS NULL
        "#,
        channel_ids,
        mode,
        proxy_id,
        group_code,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok((affected, report))
}

/// 在建渠道的事务里落出口绑定（固定分配可带上换码前已选定的代理，见 [`pick_for_new_key`]）。
pub async fn bind_new_channel(
    conn: &mut PgConnection,
    channel_id: i64,
    key_id: i64,
    binding: &Binding,
    preassigned: Option<i64>,
) -> Result<ReconcileReport, StoreError> {
    let (mode, proxy_id, group_code) = binding.columns();
    lock_assignments(conn).await?;
    sqlx::query!(
        r#"UPDATE channels SET egress_mode = $2, egress_proxy_id = $3, egress_group_code = $4
           WHERE id = $1"#,
        channel_id,
        mode,
        proxy_id,
        group_code,
    )
    .execute(&mut *conn)
    .await?;
    if let Some(proxy) = preassigned {
        sqlx::query!(
            r#"UPDATE channel_keys SET egress_proxy_id = $2 WHERE id = $1"#,
            key_id,
            proxy
        )
        .execute(&mut *conn)
        .await?;
    }
    reconcile_locked(conn).await
}

/// 全局默认出口（未配置 / 形状不对 = 直连）。
pub async fn default_binding(pool: &PgPool) -> Result<Binding, StoreError> {
    let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'egress_default'"#)
        .fetch_optional(pool)
        .await?;
    Ok(value
        .and_then(|v| serde_json::from_value::<Binding>(v).ok())
        .filter(|b| *b != Binding::Inherit)
        .unwrap_or(Binding::Direct))
}

/// 设置全局默认出口并对账（继承默认的渠道随之换出口）。`Inherit` 不是合法的默认值。
pub async fn set_default_binding(
    pool: &PgPool,
    binding: &Binding,
    updated_by: i64,
) -> Result<ReconcileReport, StoreError> {
    if *binding == Binding::Inherit {
        return Err(StoreError::InvalidData("egress_default_inherit"));
    }
    let value =
        serde_json::to_value(binding).map_err(|_| StoreError::InvalidData("egress_default"))?;
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    // settings 没有 FK：目标是否存在在这里、在锁内判，避免与并发删除交错出悬空默认值
    let exists = match binding {
        Binding::Inherit | Binding::Direct => true,
        Binding::Proxy { proxy_id } => {
            sqlx::query_scalar!(
                r#"SELECT EXISTS (SELECT 1 FROM proxies WHERE id = $1) AS "e!""#,
                proxy_id
            )
            .fetch_one(&mut *tx)
            .await?
        }
        Binding::Group { group_code } => {
            sqlx::query_scalar!(
                r#"SELECT EXISTS (SELECT 1 FROM proxy_groups WHERE code = $1) AS "e!""#,
                group_code
            )
            .fetch_one(&mut *tx)
            .await?
        }
    };
    if !exists {
        return Err(StoreError::Conflict("egress_target_missing"));
    }
    sqlx::query!(
        r#"
        INSERT INTO settings (key, value, updated_by) VALUES ('egress_default', $1, $2)
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
            updated_by = EXCLUDED.updated_by, updated_at = now()
        "#,
        value,
        updated_by
    )
    .execute(&mut *tx)
    .await?;
    let report = reconcile_locked(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}
