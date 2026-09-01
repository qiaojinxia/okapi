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
}

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
/// 渠道启用 + key active + 不在冷却期 + 在池内 + key 模型子集允许，按渠道 priority 降序。
///
/// 可见性由渠道池表达（docs/database.md §3.7）：
/// - `pool_code = Some(p)`：只有池 p 内的渠道是候选；
/// - `pool_code = None`：只看未入任何池的渠道（宽松，默认）；
///   `settings.strict_group_isolation = true` 时无候选。
///
/// "入池即专属"是刻意的：否则把高价渠道放进 vip 池后，无池用户照样打得到，
/// 池就只是个标签而不是隔离手段。
pub async fn candidates_for_model(
    pool: &PgPool,
    model: &str,
    pool_code: Option<&str>,
    master_key: Option<&str>,
) -> Result<Vec<ChannelCandidate>, StoreError> {
    let model_json = serde_json::json!([model]);
    let rows = sqlx::query!(
        r#"
        WITH cfg AS (
            SELECT COALESCE(
                (SELECT (value #>> '{}')::boolean FROM settings WHERE key = 'strict_group_isolation'),
                false
            ) AS strict
        )
        SELECT c.id AS channel_id,
               c.name AS channel_name,
               c.provider,
               c.api_base,
               c.priority,
               c.trust_upstream_usage,
               c.model_mapping,
               COALESCE((c.settings ->> 'thinking_to_content')::boolean, false) AS "thinking_to_content!",
               COALESCE((c.settings ->> 'bill_by_response_model')::boolean, false) AS "bill_by_response_model!",
               c.settings -> 'strip_request_fields' AS strip_request_fields,
               c.capabilities,
               GREATEST(COALESCE((c.upstream_unit_cost ->> 'relative_cost_milli')::bigint, 1000), 1) AS "cost_milli!",
               ck.id AS channel_key_id,
               ck.weight,
               ck.max_concurrency,
               ck.rpm_limit,
               ck.daily_spend_cap_micro,
               ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id, cfg
        WHERE c.status = 1
          AND c.deleted_at IS NULL
          AND c.models @> $1
          AND ck.status = 1
          AND (ck.cooldown_until IS NULL OR ck.cooldown_until < now())
          AND (ck.model_subset IS NULL OR ck.model_subset @> $1)
          AND (
                -- 有池：只看池内渠道
                ($2::varchar IS NOT NULL AND EXISTS (
                    SELECT 1 FROM pool_channels pc
                    WHERE pc.channel_id = c.id AND pc.pool_code = $2
                ))
                -- 无池：只看"未被任何池认领"的渠道。入池即专属，否则把渠道放进
                -- vip 池后免费用户照样能打到它——池就失去了保护渠道的意义。
                OR (
                    $2::varchar IS NULL
                    AND NOT cfg.strict
                    AND NOT EXISTS (
                        SELECT 1 FROM pool_channels pc WHERE pc.channel_id = c.id
                    )
                )
              )
        ORDER BY c.priority DESC, ck.id
        "#,
        model_json,
        pool_code
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
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
    pool_code: Option<&str>,
    master_key: Option<&str>,
) -> Result<Option<PassChannel>, StoreError> {
    let row = sqlx::query!(
        r#"
        WITH cfg AS (
            SELECT COALESCE(
                (SELECT (value #>> '{}')::boolean FROM settings WHERE key = 'strict_group_isolation'),
                false
            ) AS strict
        )
        SELECT c.api_base, c.settings, ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id, cfg
        WHERE c.id = $1
          AND c.provider = 'custom_pass'
          AND c.status = 1
          AND c.deleted_at IS NULL
          AND ck.status = 1
          AND (
                -- 有池：只看池内渠道
                ($2::varchar IS NOT NULL AND EXISTS (
                    SELECT 1 FROM pool_channels pc
                    WHERE pc.channel_id = c.id AND pc.pool_code = $2
                ))
                -- 无池：只看"未被任何池认领"的渠道。入池即专属，否则把渠道放进
                -- vip 池后免费用户照样能打到它——池就失去了保护渠道的意义。
                OR (
                    $2::varchar IS NULL
                    AND NOT cfg.strict
                    AND NOT EXISTS (
                        SELECT 1 FROM pool_channels pc WHERE pc.channel_id = c.id
                    )
                )
              )
        ORDER BY ck.id
        LIMIT 1
        "#,
        channel_id,
        pool_code
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

/// 模型解析结果（canonical 名 + 预扣估算用的补全上限）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedModel {
    pub canonical: String,
    pub max_output: Option<i32>,
}

/// 模型解析（#3001 + §5.1 预扣缺省）：模型真名直命中优先；
/// 否则走别名（精确 > 通配，priority 降序）。返回 None = 模型不存在（404）。
pub async fn resolve_model(
    pool: &PgPool,
    requested: &str,
) -> Result<Option<ResolvedModel>, StoreError> {
    let row = sqlx::query!(
        r#"
        SELECT m.model_name AS canonical, m.max_output
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
    }))
}
