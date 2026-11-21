use super::DEFAULT_POOL;
use crate::error::StoreError;
use sqlx::PgPool;

/// 渠道 key 失败类别（状态机分支，IMPLEMENTATION §3.4/§3.6）。
#[derive(Debug, Clone, Copy)]
pub enum KeyFailure {
    /// 请求/传输级故障：不改变凭证的失败计数或状态。
    Request,
    /// 连接阶段失败（TCP / TLS / 代理隧道 / 连接超时）：请求没送到上游，凭证无从判断，
    /// 同样不动 key。走了代理时：`proxy_hop`（确定坏在代理这一跳）直接记到代理的被动熔断上
    /// （`egress::mark_failure`）；分不清是代理还是目标的，由网关后台核实代理后再定。
    Unreachable { proxy_hop: bool },
    /// 上游 5xx/空回复：连续 `failure_threshold` 次（成功即清零，见 `clear_key_failures`）
    /// 进入 cooling。冷却到期恢复后若很快再失败，按轮次指数退避（60s 起，封顶 2h）；
    /// 冷却中迟到的失败（缓存 / 在途请求）不计数，不能把一次短暂故障放大成长冷却。
    Transient,
    /// 429：rate_limited，按 Retry-After 冷却（缺省 60s，显式期限最多 7 天），到期自动恢复。
    RateLimited { retry_after_secs: Option<i64> },
    /// 上游配额/余额耗尽：quota_exhausted，冷却到次日 0 点（UTC），到期自动恢复。
    QuotaExhausted,
    /// 401/403 凭证无效：invalid，仅人工恢复。
    Invalid,
}

/// 从 cooling 恢复后的「半开」窗口：冷却结束后这么久之内再失败才按轮次升级退避。
const HALF_OPEN_WINDOW_SECS: i64 = 600;

/// 一次成功的上游调用清零连续失败计数（含半开态），只动 active key、且仅在确有计数时写。
pub async fn clear_key_failures(pool: &PgPool, channel_key_id: i64) -> Result<bool, StoreError> {
    let cleared = sqlx::query(
        "UPDATE channel_keys SET failed_count = 0 WHERE id = $1 AND status = 1 AND failed_count > 0",
    )
    .bind(channel_key_id)
    .execute(pool)
    .await?;
    Ok(cleared.rows_affected() > 0)
}

/// key 级失败登记：按类别驱动状态机转移（cooling/rate_limited/quota_exhausted/invalid）。
pub async fn mark_key_failure(
    pool: &PgPool,
    channel_key_id: i64,
    error: &str,
    failure: KeyFailure,
) -> Result<(), StoreError> {
    match failure {
        KeyFailure::Request | KeyFailure::Unreachable { .. } => return Ok(()),
        KeyFailure::Transient => {
            let settings: serde_json::Value = sqlx::query_scalar("SELECT c.settings FROM channels c JOIN channel_keys k ON k.channel_id=c.id WHERE k.id=$1").bind(channel_key_id).fetch_one(pool).await?;
            let control = settings.get("account_control");
            let threshold = control
                .and_then(|p| p.get("failure_threshold"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(3)
                .clamp(1, 20);
            let base = control
                .and_then(|p| p.get("failure_cooldown_secs"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(60)
                .clamp(1, 7200);
            // 只有 active(1) 计数：冷却中的 key 已出轮转，迟到失败不能续长冷却。
            // 从冷却恢复的 key 保留计数（半开）：恢复后很快再失败 → 下一轮冷却翻倍；
            // 距上次冷却结束已久的旧计数从 1 重新开始，避免陈旧计数一击即冷却。
            sqlx::query(
                r"WITH next AS (
                    SELECT id, CASE
                        WHEN failed_count >= $3::bigint AND (cooldown_until IS NULL
                             OR cooldown_until < now() - make_interval(secs => $5::bigint::double precision))
                        THEN 1 ELSE failed_count + 1 END AS failed
                    FROM channel_keys WHERE id = $1 AND status = 1 FOR UPDATE)
                UPDATE channel_keys k SET failed_count = next.failed, last_error = $2,
                    status = CASE WHEN next.failed >= $3::bigint THEN 2 ELSE k.status END,
                    cooldown_until = CASE WHEN next.failed >= $3::bigint
                        THEN now() + make_interval(secs => least(7200, $4::bigint::double precision
                             * power(2, least(20, greatest(0, next.failed - $3::bigint)))))
                        ELSE k.cooldown_until END,
                    updated_at = now()
                FROM next WHERE k.id = next.id",
            )
            .bind(channel_key_id)
            .bind(error)
            .bind(threshold)
            .bind(base)
            .bind(HALF_OPEN_WINDOW_SECS)
            .execute(pool)
            .await?;
        }
        KeyFailure::RateLimited { retry_after_secs } => {
            let settings: serde_json::Value = sqlx::query_scalar("SELECT c.settings FROM channels c JOIN channel_keys k ON k.channel_id=c.id WHERE k.id=$1").bind(channel_key_id).fetch_one(pool).await?;
            let fallback = settings
                .pointer("/account_control/rate_limit_cooldown_secs")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(60);
            let secs = retry_after_secs.unwrap_or(fallback).clamp(1, 7 * 24 * 3600);
            sqlx::query(r"UPDATE channel_keys SET failed_count=failed_count+1,last_error=$2,status=3,
                cooldown_until=greatest(cooldown_until,now()+make_interval(secs=>$3::bigint::double precision)),updated_at=now()
                WHERE id=$1 AND status IN (1,2,3,4)").bind(channel_key_id).bind(error).bind(secs).execute(pool).await?;
        }
        KeyFailure::QuotaExhausted => {
            // 「次日 0 点」按 UTC 算，与文档、与 `spend:ck:*` 的 UTC 日同一条边界。连接池的会话时区是本机时区
            // （pg.rs），`date_trunc('day', now())` 会落在本地午夜；先换成 UTC 的无时区时间再截断 / 加一天，
            // 本地时区的夏令时也就碰不到这个「+1 day」
            sqlx::query!(
                r#"
                UPDATE channel_keys
                SET failed_count = failed_count + 1,
                    last_error = $2,
                    status = 4,
                    cooldown_until = greatest(
                        cooldown_until,
                        (date_trunc('day', now() AT TIME ZONE 'UTC') + interval '1 day') AT TIME ZONE 'UTC'
                    ),
                    updated_at = now()
                WHERE id = $1 AND status IN (1, 2, 3, 4)
                "#,
                channel_key_id,
                error
            )
            .execute(pool)
            .await?;
        }
        KeyFailure::Invalid => {
            sqlx::query!(
                r#"
                UPDATE channel_keys
                SET failed_count = failed_count + 1,
                    last_error = $2,
                    status = 6,
                    cooldown_until = NULL,
                    updated_at = now()
                WHERE id = $1 AND status IN (1, 2, 3, 4)
                "#,
                channel_key_id,
                error
            )
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// 后台探测的一个目标：一把静态 key，加上探它所需的渠道信息和一个能探的模型。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProbeKey {
    pub channel_key_id: i64,
    pub channel_id: i64,
    /// `channel_keys.status`：1 正常 / 2 冷却（有冷却期）或人工停用（冷却期为空）/ 3 限流 / 4 配额耗尽 / 6 凭证失效。
    pub key_status: i16,
    pub provider: String,
    pub api_base: Option<String>,
    pub model_mapping: serde_json::Value,
    pub settings: serde_json::Value,
    pub credential_ciphertext: Vec<u8>,
    /// 渠道里排第一个、此刻真能被调用的对话模型（模型表里启用、种类是 chat、在该 key 的 `model_subset` 内）。
    pub probe_model: String,
}

/// 给后台探测挑 key：渠道启用且在某个池里（孤儿渠道谁都打不到，别白花上游额度）、key 是静态凭证（订阅 OAuth 另有刷新流程）、
/// 协议在 `providers` 里、渠道没把 `settings.health_probe` 设成 `"off"`，且渠道里有能探的模型。
///
/// 状态：`healthy` 为真时连正常的 key（1）一起取；任何时候都取被系统摘出轮转的——限流（3）、配额耗尽（4）、凭证失效（6），
/// 以及**带冷却期**的冷却（2）。控制面「停用」一把 key 也是写 2，但冷却期是空的（`patch_channel_key` 会清掉），
/// 那是人工的决定，探测绝不碰。
/// 按 id 升序，从 `after_id` 之后取 `limit` 把，让游标能分页走完整个舰队。
pub async fn probe_keys(
    pool: &PgPool,
    healthy: bool,
    providers: &[&str],
    after_id: i64,
    limit: i64,
) -> Result<Vec<ProbeKey>, StoreError> {
    let providers: Vec<String> = providers.iter().map(|p| (*p).to_owned()).collect();
    let rows = sqlx::query_as::<_, ProbeKey>(
        r"SELECT ck.id AS channel_key_id, ck.channel_id, ck.status AS key_status, c.provider, c.api_base,
                 c.model_mapping, c.settings, ck.credential_ciphertext, pick.model_name AS probe_model
          FROM channel_keys ck
          JOIN channels c ON c.id = ck.channel_id
          CROSS JOIN LATERAL (
              SELECT m.model_name
              FROM jsonb_array_elements_text(c.models) WITH ORDINALITY AS served(name, ord)
              JOIN models m ON m.model_name = served.name AND m.status = 1
              WHERE COALESCE(NULLIF(m.catalog_config ->> 'kind', ''), 'chat') = 'chat'
                AND (ck.model_subset IS NULL OR ck.model_subset @> to_jsonb(served.name))
              ORDER BY served.ord
              LIMIT 1
          ) pick
          WHERE c.status = 1 AND c.deleted_at IS NULL
            AND EXISTS (SELECT 1 FROM pool_channels pc WHERE pc.channel_id = c.id)
            AND ck.credential_kind = 0
            AND (ck.status IN (3, 4, 6)
                 OR (ck.status = 2 AND ck.cooldown_until IS NOT NULL)
                 OR ($1 AND ck.status = 1))
            AND c.provider = ANY($2::varchar[])
            AND COALESCE(c.settings ->> 'health_probe', '') <> 'off'
            AND ck.id > $3
          ORDER BY ck.id
          LIMIT $4",
    )
    .bind(healthy)
    .bind(&providers)
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 探通的 key 放回轮转，口径同「冷却到期自动恢复」：冷却（2）保留失败计数（半开——恢复后很快再失败，下一轮冷却照样翻倍，
/// 不会因为被探测救回来一次就从头算），限流 / 配额 / 失效清零；失效的同时清掉最近错误，和控制面的「重新启用」一样。
/// 条件与 [`probe_keys`] 的选取一致：人工停用（冷却期为空的 2）和已经正常的 key 不碰，探测期间管理员的手动操作优先。
/// 返回是否真的改了。
pub async fn restore_key_after_probe(
    pool: &PgPool,
    channel_key_id: i64,
) -> Result<bool, StoreError> {
    let restored = sqlx::query(
        "UPDATE channel_keys SET status = 1, cooldown_until = NULL, \
                failed_count = CASE WHEN status = 2 THEN failed_count ELSE 0 END, \
                last_error = CASE WHEN status = 6 THEN NULL ELSE last_error END, updated_at = now() \
         WHERE id = $1 AND (status IN (3, 4, 6) OR (status = 2 AND cooldown_until IS NOT NULL))",
    )
    .bind(channel_key_id)
    .execute(pool)
    .await?;
    Ok(restored.rows_affected() > 0)
}

/// 定时刷新上游余额的一个目标：渠道里第一把静态 key 的凭证（和控制台「查询余额」同口径）加上查询所需的渠道信息。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BalanceTarget {
    pub channel_id: i64,
    pub name: String,
    /// `channels.status`：1 启用 / 2 手动停用。
    pub status: i16,
    pub provider: String,
    pub api_base: Option<String>,
    pub settings: serde_json::Value,
    pub key_id: i64,
    pub credential_ciphertext: Vec<u8>,
}

/// 给余额刷新挑渠道：没删除、有 `api_base`、协议在 `providers` 里、渠道没把 `settings.balance_refresh` 设成 `"off"`，
/// 取渠道里 id 最小的**静态**（`credential_kind = 0`）key。启用（1）和手动停用（2）的都取——停用的渠道也要刷新，
/// 才看得出「因余额低停掉的」何时可以回来。按渠道 id 升序，从 `after_id` 之后取 `limit` 个，让游标能分页走完整个舰队。
pub async fn balance_targets(
    pool: &PgPool,
    providers: &[&str],
    after_id: i64,
    limit: i64,
) -> Result<Vec<BalanceTarget>, StoreError> {
    let providers: Vec<String> = providers.iter().map(|p| (*p).to_owned()).collect();
    let rows = sqlx::query_as::<_, BalanceTarget>(
        r"SELECT c.id AS channel_id, c.name, c.status, c.provider, c.api_base, c.settings,
                 ck.id AS key_id, ck.credential_ciphertext
          FROM channels c
          CROSS JOIN LATERAL (
              SELECT id, credential_ciphertext FROM channel_keys
              WHERE channel_id = c.id AND credential_kind = 0
              ORDER BY id
              LIMIT 1
          ) ck
          WHERE c.deleted_at IS NULL AND c.status IN (1, 2)
            AND c.provider = ANY($1::varchar[])
            AND COALESCE(c.api_base, '') <> ''
            AND COALESCE(c.settings ->> 'balance_refresh', '') <> 'off'
            AND c.id > $2
          ORDER BY c.id
          LIMIT $3",
    )
    .bind(&providers)
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 渠道状态只在**当前确实是 `from`** 时才改成 `to`（1 启用 / 2 手动停用）：后台任务据余额停用 / 恢复渠道时，
/// 读状态到写状态之间管理员可能已经动过它，条件更新保证不把人的决定覆盖掉。返回是否真的改了。
pub async fn transition_channel_status(
    pool: &PgPool,
    channel_id: i64,
    from: i16,
    to: i16,
) -> Result<bool, StoreError> {
    let result = sqlx::query(
        "UPDATE channels SET status = $3, updated_at = now() \
         WHERE id = $1 AND status = $2 AND deleted_at IS NULL",
    )
    .bind(channel_id)
    .bind(from)
    .bind(to)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// 池链里能服务该模型、正在冷却（cooling / 限流 / 配额，worker 到点恢复）的 key 最早何时回来；
/// 没有冷却中的 key 时为 None。候选全空时据此给客户端 `Retry-After`。
/// 条件与 [`candidates_for_model`] 的渠道 / key 过滤一致，只把「已冷却完」换成「冷却中」。
pub async fn next_key_recovery(
    pool: &PgPool,
    model: &str,
    pools: &[&str],
) -> Result<Option<chrono::DateTime<chrono::Utc>>, StoreError> {
    let model_json = serde_json::json!([model]);
    let chain: Vec<String> = if pools.is_empty() {
        vec![DEFAULT_POOL.to_owned()]
    } else {
        pools.iter().map(|p| (*p).to_owned()).collect()
    };
    let at = sqlx::query_scalar::<_, Option<chrono::DateTime<chrono::Utc>>>(
        r"SELECT min(ck.cooldown_until)
          FROM channels c
          JOIN channel_keys ck ON ck.channel_id = c.id
          JOIN pool_channels pc ON pc.channel_id = c.id AND pc.pool_code = ANY($2::varchar[])
          WHERE c.status = 1
            AND c.deleted_at IS NULL
            AND c.models @> $1
            AND ck.status IN (1, 2, 3, 4)
            AND (ck.status <> 1 OR ck.cooldown_until > now())
            AND ck.cooldown_until IS NOT NULL
            AND (ck.model_subset IS NULL OR ck.model_subset @> $1)",
    )
    .bind(model_json)
    .bind(&chain)
    .fetch_one(pool)
    .await?;
    Ok(at)
}

/// 路由诊断的 key 视图（不过滤，静态事实 + 配置的运行期闸）。
#[derive(Debug, serde::Serialize)]
pub struct DiagKey {
    pub key_id: i64,
    pub status: i16,
    pub cooldown_until: Option<chrono::DateTime<chrono::Utc>>,
    pub weight: i32,
    /// model_subset 为 NULL（继承渠道）或包含目标模型。
    pub subset_ok: bool,
    pub rpm_limit: Option<i32>,
    pub daily_spend_cap_micro: Option<i64>,
    pub max_concurrency: Option<i32>,
    /// 出口不可用的原因（§11.41）：`egress_cooling` 代理熔断中 / `egress_unassigned` 固定分配
    /// 没分到代理（容量满）/ `egress_unavailable` 代理停用、缺失或组里无可用成员；None = 出口可用。
    pub egress_block: Option<String>,
}

/// 路由诊断的渠道视图（服务目标模型的全集，含被淘汰者）。
#[derive(Debug, serde::Serialize)]
pub struct DiagChannel {
    pub channel_id: i64,
    pub name: String,
    pub provider: String,
    pub status: i16,
    pub priority: i32,
    /// 渠道所属的池（空 = 未入池）。
    pub pools: Vec<String>,
    pub keys: Vec<DiagKey>,
}

/// 路由诊断（console 只读）：返回**声称服务该模型**的渠道全集——与生产查询
/// `candidates_for_model` 相反，不过滤状态/池/冷却，专供"为什么没有候选"
/// 生成逐环淘汰原因。幸存者判定仍以生产查询为准，本函数只提供事实底座。
pub async fn diagnose_channels(pool: &PgPool, model: &str) -> Result<Vec<DiagChannel>, StoreError> {
    let model_json = serde_json::json!([model]);
    let rows = sqlx::query!(
        r#"
        SELECT c.id AS channel_id,
               c.name,
               c.provider,
               c.status,
               c.priority,
               COALESCE(
                   (SELECT array_agg(pc.pool_code ORDER BY pc.pool_code)
                    FROM pool_channels pc WHERE pc.channel_id = c.id),
                   '{}'
               ) AS "pools!",
               ck.id AS "key_id?",
               ck.status AS "key_status?",
               ck.cooldown_until,
               ck.weight AS "key_weight?",
               (ck.model_subset IS NULL OR ck.model_subset @> $1) AS "subset_ok?",
               ck.rpm_limit,
               ck.daily_spend_cap_micro,
               ck.max_concurrency,
               CASE
                   WHEN ck.id IS NULL OR ep.mode = 'direct' OR ep.proxy_id IS NOT NULL THEN NULL
                   WHEN ea.proxy_id IS NOT NULL THEN 'egress_cooling'
                   WHEN g.mode = 'pinned' AND ck.egress_proxy_id IS NULL THEN 'egress_unassigned'
                   ELSE 'egress_unavailable'
               END AS egress_block
        FROM channels c
        LEFT JOIN channel_keys ck ON ck.channel_id = c.id
        LEFT JOIN channel_egress e ON e.channel_id = c.id
        LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
        LEFT JOIN LATERAL egress_pick(c.id, ck.id, true) ep ON ck.id IS NOT NULL
        LEFT JOIN LATERAL egress_pick(c.id, ck.id, false) ea ON ck.id IS NOT NULL
        WHERE c.deleted_at IS NULL AND c.models @> $1
        ORDER BY c.priority DESC, c.id, ck.id
        "#,
        model_json
    )
    .fetch_all(pool)
    .await?;

    let mut channels: Vec<DiagChannel> = Vec::new();
    for r in rows {
        if channels.last().is_none_or(|c| c.channel_id != r.channel_id) {
            channels.push(DiagChannel {
                channel_id: r.channel_id,
                name: r.name,
                provider: r.provider,
                status: r.status,
                priority: r.priority,
                pools: r.pools,
                keys: Vec::new(),
            });
        }
        if let (Some(key_id), Some(status), Some(weight), Some(subset_ok)) =
            (r.key_id, r.key_status, r.key_weight, r.subset_ok)
            && let Some(ch) = channels.last_mut()
        {
            ch.keys.push(DiagKey {
                key_id,
                status,
                cooldown_until: r.cooldown_until,
                weight,
                subset_ok,
                rpm_limit: r.rpm_limit,
                daily_spend_cap_micro: r.daily_spend_cap_micro,
                max_concurrency: r.max_concurrency,
                egress_block: r.egress_block,
            });
        }
    }
    Ok(channels)
}

/// 模型解析结果（canonical 名 + 预扣估算用的补全上限 + 模型级降级链）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedModel {
    pub canonical: String,
    pub max_output: Option<i32>,
    /// 降级链（DESIGN §3.4.1）：本模型零可用候选时按序改投；单跳不递归。
    pub fallback_models: Vec<String>,
}

/// 模型解析（#3001 + §5.1 预扣缺省）：模型真名直命中优先；
/// 否则走别名（精确 > 通配，priority 降序）。返回 None = 模型不存在（404）。
/// 通配只认 `*`：模式里的 `\` `%` `_` 先转义成字面再换 `*`，否则 `gpt_4*` 也会命中 `gpt-4o`。
pub async fn resolve_model(
    pool: &PgPool,
    requested: &str,
) -> Result<Option<ResolvedModel>, StoreError> {
    let row = sqlx::query!(
        r#"
        SELECT m.model_name AS canonical, m.max_output, m.fallback_models
        FROM models m
        WHERE m.status = 1
          AND (
                m.model_name = $1
                OR m.model_name = (
                    SELECT target_model FROM model_aliases
                    WHERE enabled AND (pattern = $1 OR $1 LIKE REPLACE(REPLACE(REPLACE(REPLACE(pattern, '\', '\\'), '%', '\%'), '_', '\_'), '*', '%'))
                    ORDER BY (pattern = $1) DESC, priority DESC, pattern
                    LIMIT 1
                )
              )
        ORDER BY (m.model_name = $1) DESC
        LIMIT 1
        "#,
        requested
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| ResolvedModel {
        canonical: r.canonical,
        max_output: r.max_output,
        fallback_models: serde_json::from_value(r.fallback_models).unwrap_or_default(),
    }))
}
