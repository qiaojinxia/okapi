use crate::error::StoreError;
use sqlx::PgPool;

/// 渠道候选（channel × channel_key 展开行，按 priority 降序返回）。
#[derive(Debug, Clone)]
pub struct ChannelCandidate {
    pub channel_id: i64,
    pub channel_key_id: i64,
    pub channel_name: String,
    pub provider: String,
    pub api_base: Option<String>,
    /// 已解封的上游凭证（落库为 AES-256-GCM 信封，见 `crate::credential`）。
    pub credential: String,
    pub priority: i32,
    pub weight: i32,
    pub trust_upstream_usage: bool,
    /// key 级并发上限（null = 不限；在途计数在 Redis conc:ck:*）。
    pub max_concurrency: Option<i32>,
    /// key 级 RPM 上限（null = 不限；固定分钟窗在 Redis rpm:ck:*）。
    pub rpm_limit: Option<i32>,
    /// key 级当日消费上限 micro（null = 不限；累计在 Redis spend:ck:*）。
    pub daily_spend_cap_micro: Option<i64>,
    /// 对外模型名 → 上游模型名（无映射则原名透传）。
    pub model_mapping: serde_json::Value,
    /// 渠道开关：思维链转 <think> 正文（channels.settings.thinking_to_content）。
    pub thinking_to_content: bool,
    /// 渠道开关：按上游响应报告的模型计费（channels.settings.bill_by_response_model，
    /// Sub2API 0.1.175 对齐；映射改名场景计费跟实际模型，价簿无价则回退请求模型）。
    pub bill_by_response_model: bool,
    /// 不透传给上游的请求顶层字段（channels.settings.strip_request_fields，
    /// new-api rc.23 #6847 对齐；model/messages/stream 受保护不可剥）。
    pub strip_request_fields: Vec<String>,
    /// 能力声明（显式 false 才排除，IMPLEMENTATION §3.8）。
    pub capabilities: serde_json::Value,
    /// 相对成本千分比（层内权重除数，缺省 1000 = 中性）。
    pub cost_milli: i64,
    /// 所属池在池链里的序号（0 = 主池，1 = 降级池）。调度先耗尽低序号的全部层，
    /// 再进入下一序号——降级池的高优先级渠道也排在主池最低优先级之后。
    pub pool_rank: i32,
}

/// 内置默认池：新渠道缺省加入、未指定池的分组走这里。
pub const DEFAULT_POOL: &str = "default";

impl ChannelCandidate {
    /// 解析上游实际模型名。
    #[must_use]
    pub fn upstream_model<'a>(&'a self, model: &'a str) -> &'a str {
        self.model_mapping
            .get(model)
            .and_then(|v| v.as_str())
            .unwrap_or(model)
    }
}

/// 服务指定模型的可用渠道 key 候选：
/// 渠道启用 + key active + 不在冷却期 + 在池链内 + key 模型子集允许。
///
/// 可见性只有一条规则（IMPLEMENTATION §11.14）：**渠道只服务它所在的池**。
/// `pools` 是有序池链（主池 → 降级池，见 `channel_pools.fallback_pool_code`），
/// 返回序 = (池序, 有效优先级降序, key id)；同一渠道同时在两个池里只算进靠前的那个。
/// 有效优先级 / 权重取成员级覆盖（`pool_channels.priority_override / weight_override`），
/// 缺省继承渠道与 key 自身——同一渠道可以在 stable 池当主力、在 fast 池当备胎。
///
/// 未入任何池的渠道（孤儿）对谁都不可达；空池链按 `DEFAULT_POOL` 兜底。
pub async fn candidates_for_model(
    pool: &PgPool,
    model: &str,
    pools: &[&str],
    master_key: Option<&str>,
) -> Result<Vec<ChannelCandidate>, StoreError> {
    let model_json = serde_json::json!([model]);
    let chain: Vec<String> = if pools.is_empty() {
        vec![DEFAULT_POOL.to_owned()]
    } else {
        pools.iter().map(|p| (*p).to_owned()).collect()
    };
    let rows = sqlx::query!(
        r#"
        SELECT c.id AS channel_id,
               c.name AS channel_name,
               c.provider,
               c.api_base,
               COALESCE(pc.priority_override, c.priority) AS "priority!",
               c.trust_upstream_usage,
               c.model_mapping,
               COALESCE((c.settings ->> 'thinking_to_content')::boolean, false) AS "thinking_to_content!",
               COALESCE((c.settings ->> 'bill_by_response_model')::boolean, false) AS "bill_by_response_model!",
               c.settings -> 'strip_request_fields' AS strip_request_fields,
               c.capabilities,
               GREATEST(COALESCE((c.upstream_unit_cost ->> 'relative_cost_milli')::bigint, 1000), 1) AS "cost_milli!",
               ck.id AS channel_key_id,
               COALESCE(pc.weight_override, ck.weight) AS "weight!",
               ck.max_concurrency,
               ck.rpm_limit,
               ck.daily_spend_cap_micro,
               ck.credential_ciphertext,
               (array_position($2::varchar[], pc.pool_code::varchar) - 1) AS "pool_rank!"
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        JOIN pool_channels pc ON pc.channel_id = c.id AND pc.pool_code = ANY($2::varchar[])
        WHERE c.status = 1
          AND c.deleted_at IS NULL
          AND c.models @> $1
          AND ck.status = 1
          AND (ck.cooldown_until IS NULL OR ck.cooldown_until < now())
          AND (ck.model_subset IS NULL OR ck.model_subset @> $1)
        ORDER BY array_position($2::varchar[], pc.pool_code::varchar),
                 COALESCE(pc.priority_override, c.priority) DESC,
                 ck.id
        "#,
        model_json,
        &chain
    )
    .fetch_all(pool)
    .await?;

    // 同一把 key 经两个池各出一行：保留池序靠前的那行（SQL 已按池序排好）
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|r| seen.insert(r.channel_key_id))
        .map(|r| {
            let credential = crate::credential::open(master_key, &r.credential_ciphertext)?;
            Ok(ChannelCandidate {
                channel_id: r.channel_id,
                channel_key_id: r.channel_key_id,
                channel_name: r.channel_name,
                provider: r.provider,
                api_base: r.api_base,
                credential,
                priority: r.priority,
                weight: r.weight,
                trust_upstream_usage: r.trust_upstream_usage,
                max_concurrency: r.max_concurrency,
                rpm_limit: r.rpm_limit,
                daily_spend_cap_micro: r.daily_spend_cap_micro,
                model_mapping: r.model_mapping,
                thinking_to_content: r.thinking_to_content,
                bill_by_response_model: r.bill_by_response_model,
                strip_request_fields: r
                    .strip_request_fields
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default(),
                capabilities: r.capabilities,
                cost_milli: r.cost_milli,
                pool_rank: r.pool_rank,
            })
        })
        .collect()
}

/// custom_pass 渠道点查结果。
pub struct PassChannel {
    pub api_base: String,
    pub credential: String,
    pub settings: serde_json::Value,
}

/// custom_pass 渠道点查（可见性矩阵与候选查询同语义）。
/// videos 任务回源点查结果（channel_key_id → 连接信息）。
#[derive(Debug)]
pub struct ChannelKeyRef {
    pub channel_id: i64,
    pub api_base: Option<String>,
    pub credential: String,
}

/// videos 轮询/下载回源：按 channel_key_id 点查渠道连接信息（低频路径）。
/// 可见性不复查（任务映射已绑定创建者 user_id）；渠道/key 停用即拒（返回 None）。
pub async fn channel_key_ref(
    pool: &PgPool,
    channel_key_id: i64,
    master_key: Option<&str>,
) -> Result<Option<ChannelKeyRef>, StoreError> {
    let row = sqlx::query!(
        r#"
        SELECT c.id AS channel_id, c.api_base, ck.credential_ciphertext
        FROM channel_keys ck
        JOIN channels c ON c.id = ck.channel_id
        WHERE ck.id = $1 AND ck.status = 1 AND c.status = 1 AND c.deleted_at IS NULL
        "#,
        channel_key_id
    )
    .fetch_optional(pool)
    .await?;
    // 曾经是 from_utf8_lossy：解不出就悄悄发一串替换字符给上游，只会换来一个
    // 难查的 401。信封化后一律走 open，失败即 Err。
    row.map(|r| {
        Ok(ChannelKeyRef {
            channel_id: r.channel_id,
            api_base: r.api_base,
            credential: crate::credential::open(master_key, &r.credential_ciphertext)?,
        })
    })
    .transpose()
}

pub async fn custom_pass_channel(
    pool: &PgPool,
    channel_id: i64,
    pools: &[&str],
    master_key: Option<&str>,
) -> Result<Option<PassChannel>, StoreError> {
    let chain: Vec<String> = if pools.is_empty() {
        vec![DEFAULT_POOL.to_owned()]
    } else {
        pools.iter().map(|p| (*p).to_owned()).collect()
    };
    let row = sqlx::query!(
        r#"
        SELECT c.api_base, c.settings, ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        WHERE c.id = $1
          AND c.provider = 'custom_pass'
          AND c.status = 1
          AND c.deleted_at IS NULL
          AND ck.status = 1
          -- 可见性与候选查询同一条规则：渠道只服务它所在的池（链内任一池即可）
          AND EXISTS (
                SELECT 1 FROM pool_channels pc
                WHERE pc.channel_id = c.id AND pc.pool_code = ANY($2::varchar[])
          )
        ORDER BY ck.id
        LIMIT 1
        "#,
        channel_id,
        &chain
    )
    .fetch_optional(pool)
    .await?;
    row.map(|r| {
        let credential = crate::credential::open(master_key, &r.credential_ciphertext)?;
        Ok(PassChannel {
            api_base: r.api_base.unwrap_or_default(),
            credential,
            settings: r.settings,
        })
    })
    .transpose()
}

/// 渠道 key 失败类别（状态机分支，IMPLEMENTATION §3.4/§3.6）。
#[derive(Debug, Clone, Copy)]
pub enum KeyFailure {
    /// 网络/超时/5xx：连续 3 次进入 cooling，指数退避（60s 起，封顶 2h）。
    Transient,
    /// 429：rate_limited，按 Retry-After 冷却（缺省 60s），到期自动恢复。
    RateLimited { retry_after_secs: Option<i64> },
    /// 上游配额/余额耗尽：quota_exhausted，冷却到次日 0 点（UTC），到期自动恢复。
    QuotaExhausted,
    /// 401/403 凭证无效：invalid，仅人工恢复。
    Invalid,
}

/// key 级失败登记：按类别驱动状态机转移（cooling/rate_limited/quota_exhausted/invalid）。
pub async fn mark_key_failure(
    pool: &PgPool,
    channel_key_id: i64,
    error: &str,
    failure: KeyFailure,
) -> Result<(), StoreError> {
    match failure {
        KeyFailure::Transient => {
            // 连续 3 次转 cooling；退避 = 60 * 2^(超出阈值次数)，封顶 7200s
            sqlx::query!(
                r#"
                UPDATE channel_keys
                SET failed_count = failed_count + 1,
                    last_error = $2,
                    status = CASE WHEN failed_count + 1 >= 3 THEN 2 ELSE status END,
                    cooldown_until = CASE WHEN failed_count + 1 >= 3
                        THEN now() + make_interval(secs => least(7200, 60 * power(2, failed_count + 1 - 3)))
                        ELSE cooldown_until END,
                    updated_at = now()
                WHERE id = $1
                "#,
                channel_key_id,
                error
            )
            .execute(pool)
            .await?;
        }
        KeyFailure::RateLimited { retry_after_secs } => {
            let secs = retry_after_secs.unwrap_or(60).clamp(1, 3600);
            sqlx::query!(
                r#"
                UPDATE channel_keys
                SET failed_count = failed_count + 1,
                    last_error = $2,
                    status = 3,
                    cooldown_until = now() + make_interval(secs => $3::bigint::double precision),
                    updated_at = now()
                WHERE id = $1
                "#,
                channel_key_id,
                error,
                secs
            )
            .execute(pool)
            .await?;
        }
        KeyFailure::QuotaExhausted => {
            sqlx::query!(
                r#"
                UPDATE channel_keys
                SET failed_count = failed_count + 1,
                    last_error = $2,
                    status = 4,
                    cooldown_until = date_trunc('day', now() + interval '1 day'),
                    updated_at = now()
                WHERE id = $1
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
                WHERE id = $1
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
               ck.max_concurrency
        FROM channels c
        LEFT JOIN channel_keys ck ON ck.channel_id = c.id
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
                    WHERE enabled AND (pattern = $1 OR $1 LIKE REPLACE(pattern, '*', '%'))
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
