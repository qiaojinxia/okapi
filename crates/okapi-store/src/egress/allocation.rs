use super::{ASSIGN_LOCK, Binding, ReconcileReport, Resolved, default_binding};
use crate::error::StoreError;
use sqlx::{PgConnection, PgPool};
use std::collections::HashMap;

/// 手动把一把 key 改分到组内另一个代理。key 的有效出口必须是 pinned 组且目标是组员；
/// 目标若已满（不计这把 key 自己）→ `Conflict("proxy_full")`。
pub async fn assign_key(pool: &PgPool, key_id: i64, proxy_id: i64) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    lock_assignments(&mut tx).await?;
    let row = sqlx::query!(
        r#"
        SELECT e.mode AS "mode!", g.mode AS "group_mode?",
               EXISTS (SELECT 1 FROM proxy_group_members m
                        WHERE m.group_code = e.group_code AND m.proxy_id = $2) AS "member!",
               (SELECT p.max_keys FROM proxies p WHERE p.id = $2) AS max_keys,
               (SELECT COUNT(*) FROM channel_keys o JOIN channels oc ON oc.id = o.channel_id
                 WHERE o.egress_proxy_id = $2 AND o.id <> $1 AND oc.deleted_at IS NULL) AS "load!"
        FROM channel_keys k
        JOIN channels c ON c.id = k.channel_id AND c.deleted_at IS NULL
        JOIN channel_egress e ON e.channel_id = c.id
        LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
        WHERE k.id = $1
        "#,
        key_id,
        proxy_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(StoreError::Conflict("key_not_found"))?;
    if row.mode != "group" || row.group_mode.as_deref() != Some("pinned") {
        return Err(StoreError::Conflict("egress_not_pinned"));
    }
    if !row.member {
        return Err(StoreError::Conflict("proxy_not_in_group"));
    }
    if row.max_keys.is_some_and(|cap| row.load >= i64::from(cap)) {
        return Err(StoreError::Conflict("proxy_full"));
    }
    sqlx::query!(
        r#"UPDATE channel_keys SET egress_proxy_id = $2, updated_at = now() WHERE id = $1"#,
        key_id,
        proxy_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---- 固定分配对账 ----

pub(super) async fn lock_assignments(conn: &mut PgConnection) -> Result<(), StoreError> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", ASSIGN_LOCK)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// 全量对账（自带锁；须在事务内调用，锁随事务释放）。
pub async fn reconcile(conn: &mut PgConnection) -> Result<ReconcileReport, StoreError> {
    lock_assignments(conn).await?;
    reconcile_locked(conn).await
}

/// 组内成员候选（分配用）。
struct Slot {
    proxy_id: i64,
    usable: bool,
    healthy: bool,
    priority: i32,
    capacity: Option<i64>,
}

pub(super) async fn reconcile_locked(
    conn: &mut PgConnection,
) -> Result<ReconcileReport, StoreError> {
    // 1. 释放失效分配：渠道已删 / 有效出口不再是 pinned 组 / 分到的代理已不是组员
    let released = sqlx::query!(
        r#"
        UPDATE channel_keys k SET egress_proxy_id = NULL
        FROM channels c
        JOIN channel_egress e ON e.channel_id = c.id
        LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
        WHERE k.channel_id = c.id AND k.egress_proxy_id IS NOT NULL
          AND (c.deleted_at IS NOT NULL
               OR g.mode IS DISTINCT FROM 'pinned'
               OR NOT EXISTS (SELECT 1 FROM proxy_group_members m
                               WHERE m.group_code = g.code AND m.proxy_id = k.egress_proxy_id))
        "#
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();

    // 2. 待分配的 key（有效出口是 pinned 组、尚无分配），按 id 先来先分
    let pending: Vec<(i64, String)> = sqlx::query!(
        r#"
        SELECT k.id, e.group_code AS "group_code!"
        FROM channel_keys k
        JOIN channels c ON c.id = k.channel_id
        JOIN channel_egress e ON e.channel_id = c.id
        JOIN proxy_groups g ON g.code = e.group_code
        WHERE c.deleted_at IS NULL AND e.mode = 'group' AND g.mode = 'pinned'
          AND k.egress_proxy_id IS NULL
        ORDER BY k.id
        "#
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.group_code))
    .collect();
    if pending.is_empty() {
        return Ok(ReconcileReport {
            released,
            ..ReconcileReport::default()
        });
    }

    // 3. 逐把分配，一次写回
    let (slots, mut load) = allocation_state(conn, &pending).await?;
    let mut keys = Vec::new();
    let mut proxies = Vec::new();
    let mut unassigned = 0u64;
    for (key_id, group_code) in &pending {
        match choose(slots.get(group_code).map(Vec::as_slice), &load) {
            Some(proxy_id) => {
                *load.entry(proxy_id).or_default() += 1;
                keys.push(*key_id);
                proxies.push(proxy_id);
            }
            None => unassigned += 1,
        }
    }
    if !keys.is_empty() {
        sqlx::query!(
            r#"
            UPDATE channel_keys k SET egress_proxy_id = v.proxy_id
            FROM unnest($1::bigint[], $2::bigint[]) AS v(key_id, proxy_id)
            WHERE k.id = v.key_id
            "#,
            &keys,
            &proxies
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(ReconcileReport {
        assigned: u64::try_from(keys.len()).unwrap_or(u64::MAX),
        released,
        unassigned,
    })
}

/// 涉及组的成员表与每个代理的全局已分配数（`max_keys` 跨组共享）。
async fn allocation_state(
    conn: &mut PgConnection,
    pending: &[(i64, String)],
) -> Result<(HashMap<String, Vec<Slot>>, HashMap<i64, i64>), StoreError> {
    let mut groups: Vec<String> = pending.iter().map(|(_, g)| g.clone()).collect();
    groups.sort_unstable();
    groups.dedup();
    let members = sqlx::query!(
        r#"
        SELECT m.group_code, p.id, p.status, m.priority, p.max_keys,
               COALESCE(p.cooldown_until > now(), false) AS "cooling!"
        FROM proxy_group_members m JOIN proxies p ON p.id = m.proxy_id
        WHERE m.group_code = ANY($1)
        ORDER BY m.group_code, p.id
        "#,
        &groups
    )
    .fetch_all(&mut *conn)
    .await?;
    let loads = sqlx::query!(
        r#"
        SELECT k.egress_proxy_id AS "proxy_id!", COUNT(*) AS "n!"
        FROM channel_keys k JOIN channels c ON c.id = k.channel_id
        WHERE k.egress_proxy_id IS NOT NULL AND c.deleted_at IS NULL
        GROUP BY k.egress_proxy_id
        "#
    )
    .fetch_all(&mut *conn)
    .await?;
    let load: HashMap<i64, i64> = loads.into_iter().map(|r| (r.proxy_id, r.n)).collect();
    let mut slots: HashMap<String, Vec<Slot>> = HashMap::new();
    for m in members {
        slots.entry(m.group_code).or_default().push(Slot {
            proxy_id: m.id,
            usable: m.status == 1,
            healthy: m.status == 1 && !m.cooling,
            priority: m.priority,
            capacity: m.max_keys.map(i64::from),
        });
    }
    Ok((slots, load))
}

/// 启用且未满的成员里：健康优先 → 已分配最少 → priority 高 → id 小。
/// 熔断中的成员仍可分（它会恢复，固定分配本就是等恢复的语义）；停用的不分。
fn choose(candidates: Option<&[Slot]>, load: &HashMap<i64, i64>) -> Option<i64> {
    let used = |s: &Slot| load.get(&s.proxy_id).copied().unwrap_or(0);
    candidates?
        .iter()
        .filter(|s| s.usable && s.capacity.is_none_or(|cap| used(s) < cap))
        .min_by_key(|s| {
            (
                !s.healthy,
                used(s),
                std::cmp::Reverse(s.priority),
                s.proxy_id,
            )
        })
        .map(|s| s.proxy_id)
}

/// OAuth 换码前给「即将新建的 key」选定出口：换码请求就得从它日后的 IP 出去。
/// 只读（不占位）：建渠道时以 `bind_new_channel(.., preassigned)` 落库，并发下最多
/// 让某个代理超出 `max_keys` 一把——对账从不驱逐已有分配，宁可超一点也不在换码后改 IP。
pub async fn pick_for_new_key(
    pool: &PgPool,
    binding: &Binding,
    master_key: Option<&str>,
) -> Result<(Resolved, Option<i64>), StoreError> {
    let effective = match binding {
        Binding::Inherit => default_binding(pool).await?,
        other => other.clone(),
    };
    match effective {
        Binding::Inherit | Binding::Direct => Ok((Resolved::Direct, None)),
        Binding::Proxy { proxy_id } => {
            let row = sqlx::query!(
                r#"SELECT url_ciphertext FROM proxies WHERE id = $1 AND status = 1"#,
                proxy_id
            )
            .fetch_optional(pool)
            .await?;
            Ok(match row {
                Some(r) => (
                    Resolved::Proxy {
                        id: proxy_id,
                        url: crate::credential::open(master_key, &r.url_ciphertext)?,
                    },
                    None,
                ),
                None => (Resolved::Unavailable, None),
            })
        }
        Binding::Group { group_code } => {
            let row = sqlx::query!(
                r#"
                SELECT g.mode, p.id, p.url_ciphertext
                FROM proxy_groups g
                JOIN proxy_group_members m ON m.group_code = g.code
                JOIN proxies p ON p.id = m.proxy_id AND p.status = 1
                WHERE g.code = $1
                  AND (g.mode = 'rotate' OR p.max_keys IS NULL OR p.max_keys > (
                       SELECT COUNT(*) FROM channel_keys k JOIN channels c ON c.id = k.channel_id
                       WHERE k.egress_proxy_id = p.id AND c.deleted_at IS NULL))
                ORDER BY COALESCE(p.cooldown_until > now(), false),
                         CASE WHEN g.mode = 'pinned' THEN (
                             SELECT COUNT(*) FROM channel_keys k JOIN channels c ON c.id = k.channel_id
                             WHERE k.egress_proxy_id = p.id AND c.deleted_at IS NULL) ELSE 0 END,
                         m.priority DESC,
                         CASE WHEN g.mode = 'rotate'
                              THEN -ln(1 - random()) / GREATEST(m.weight, 1) ELSE 0 END,
                         p.id
                LIMIT 1
                "#,
                group_code
            )
            .fetch_optional(pool)
            .await?;
            Ok(match row {
                Some(r) => {
                    let url = crate::credential::open(master_key, &r.url_ciphertext)?;
                    let preassigned = (r.mode == "pinned").then_some(r.id);
                    (Resolved::Proxy { id: r.id, url }, preassigned)
                }
                None => (Resolved::Unavailable, None),
            })
        }
    }
}

// ---- 控制面解析 ----

/// 一把 key 此刻的出口（控制面：刷新 token、配额轮询、测活、余额、视频回源）。
/// 固定出口熔断中照样返回它（宁可失败也不换 IP）；轮换组健康成员优先。None = key 不存在。
pub async fn resolve_for_key(
    pool: &PgPool,
    key_id: i64,
    master_key: Option<&str>,
) -> Result<Option<Resolved>, StoreError> {
    let row = sqlx::query!(
        r#"
        SELECT ep.mode AS "mode!", ep.proxy_id, ep.url_ciphertext
        FROM channel_keys k
        CROSS JOIN LATERAL egress_pick(k.channel_id, k.id, false) ep
        WHERE k.id = $1
        "#,
        key_id
    )
    .fetch_optional(pool)
    .await?;
    row.map(|r| resolved(master_key, &r.mode, r.proxy_id, r.url_ciphertext.as_deref()))
        .transpose()
}

/// `egress_pick` 一行 → 解析结果。
pub(crate) fn resolved(
    master_key: Option<&str>,
    mode: &str,
    proxy_id: Option<i64>,
    url_ciphertext: Option<&[u8]>,
) -> Result<Resolved, StoreError> {
    match (proxy_id, url_ciphertext) {
        (Some(id), Some(stored)) => Ok(Resolved::Proxy {
            id,
            url: crate::credential::open(master_key, stored)?,
        }),
        _ if mode == "direct" => Ok(Resolved::Direct),
        _ => Ok(Resolved::Unavailable),
    }
}

/// `okapi seal-credentials` 的代理部分：把迁移来的明文代理 URL 封成信封。幂等。
pub async fn seal_existing(
    pool: &PgPool,
    master_key_hex: &str,
) -> Result<crate::credential::SealStats, StoreError> {
    let rows = sqlx::query!(r#"SELECT id, url_ciphertext FROM proxies ORDER BY id"#)
        .fetch_all(pool)
        .await?;
    let mut stats = crate::credential::SealStats::default();
    for row in rows {
        if crate::credential::is_sealed(&row.url_ciphertext) {
            stats.already_sealed += 1;
            continue;
        }
        let Ok(plain) = std::str::from_utf8(&row.url_ciphertext) else {
            stats.unreadable.push(row.id);
            continue;
        };
        let sealed = crate::credential::seal(master_key_hex, plain)?;
        stats.sealed += sqlx::query!(
            r#"UPDATE proxies SET url_ciphertext = $2, updated_at = now()
               WHERE id = $1 AND url_ciphertext = $3"#,
            row.id,
            sealed,
            row.url_ciphertext
        )
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binding_json_shape_round_trips_and_maps_to_columns() {
        for (value, columns) in [
            (json!({"mode":"inherit"}), (None, None, None)),
            (json!({"mode":"direct"}), (Some("direct"), None, None)),
            (
                json!({"mode":"proxy","proxy_id":7}),
                (Some("proxy"), Some(7), None),
            ),
            (
                json!({"mode":"group","group_code":"hk"}),
                (Some("group"), None, Some("hk")),
            ),
        ] {
            let binding: Binding = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&binding).unwrap(), value);
            assert_eq!(binding.columns(), columns);
            let (mode, proxy, group) = columns;
            assert_eq!(
                Binding::from_columns(mode, proxy, group.map(str::to_owned)),
                binding
            );
        }
        // 缺目标的形状不能被当成合法绑定
        assert!(serde_json::from_value::<Binding>(json!({"mode":"proxy"})).is_err());
        assert!(serde_json::from_value::<Binding>(json!({"mode":"vpn"})).is_err());
    }

    #[test]
    fn allocation_prefers_healthy_then_least_loaded_and_respects_capacity() {
        let slot = |proxy_id, usable, healthy, priority, capacity| Slot {
            proxy_id,
            usable,
            healthy,
            priority,
            capacity,
        };
        let slots = [
            slot(1, true, true, 0, Some(1)),
            slot(2, true, true, 0, None),
            slot(3, true, false, 9, None),
            slot(4, false, false, 9, None),
        ];
        let mut load = HashMap::new();
        // 都空：健康里 priority 相同取 id 小的
        assert_eq!(choose(Some(&slots), &load), Some(1));
        // 1 满了（容量 1）→ 2；不选熔断中的 3、停用的 4
        load.insert(1, 1);
        assert_eq!(choose(Some(&slots), &load), Some(2));
        // 健康成员都满了才落到熔断中的（等它恢复），停用的永远不分
        let busy = [
            slot(1, true, true, 0, Some(1)),
            slot(3, true, false, 0, None),
            slot(4, false, false, 0, None),
        ];
        assert_eq!(choose(Some(&busy), &load), Some(3));
        let full = [
            slot(1, true, true, 0, Some(1)),
            slot(4, false, true, 0, None),
        ];
        assert_eq!(choose(Some(&full), &load), None);
        assert_eq!(choose(None, &load), None);
        // 负载均衡：已分配少的优先于 priority
        let mut balanced = HashMap::new();
        balanced.insert(5, 3);
        let pair = [slot(5, true, true, 9, None), slot(6, true, true, 0, None)];
        assert_eq!(choose(Some(&pair), &balanced), Some(6));
    }

    #[test]
    fn unavailable_egress_is_an_error_never_direct() {
        assert_eq!(Resolved::Direct.proxy_url().unwrap(), None);
        assert_eq!(
            Resolved::Proxy {
                id: 1,
                url: "http://p:1".into()
            }
            .proxy_url()
            .unwrap()
            .as_deref(),
            Some("http://p:1")
        );
        assert!(Resolved::Unavailable.proxy_url().is_err());
        assert_eq!(
            resolved(None, "direct", None, None).unwrap(),
            Resolved::Direct
        );
        assert_eq!(
            resolved(None, "group", None, None).unwrap(),
            Resolved::Unavailable
        );
        assert_eq!(
            resolved(None, "proxy", None, None).unwrap(),
            Resolved::Unavailable
        );
    }
}
