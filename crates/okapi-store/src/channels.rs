use crate::egress::Resolved;
use crate::error::StoreError;
use rand::RngExt;
use sqlx::PgPool;
use std::sync::Arc;

/// 渠道候选（channel × channel_key 展开行，按 priority 降序返回）。
#[derive(Debug, Clone)]
// 四个 bool 是彼此独立的渠道开关（settings JSON 的镜像），不是状态机——收成 enum 只会让
// 每处 `cand.xxx` 读法变长而换不来任何不变量。
#[allow(clippy::struct_excessive_bools)]
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
    /// 上游数据留存声明（channels.settings.data_retention）：
    /// `none` 不留存 / `transient` 短期留存不训练 / `trains` 可能用于训练；
    /// **None = 未声明**——请求要求零留存时按"不知道"处理，即排除（fail-closed）。
    pub data_retention: Option<String>,
    /// 渠道开关：思维链转 <think> 正文（channels.settings.thinking_to_content）。
    pub thinking_to_content: bool,
    /// 渠道开关：按上游响应报告的模型计费（channels.settings.bill_by_response_model，
    /// Sub2API 0.1.175 对齐；映射改名场景计费跟实际模型，价簿无价则回退请求模型）。
    pub bill_by_response_model: bool,
    /// 不透传给上游的请求顶层字段（channels.settings.strip_request_fields，
    /// new-api rc.23 #6847 对齐；model/messages/stream 受保护不可剥）。
    pub strip_request_fields: Vec<String>,
    /// 强制写入请求顶层的字段（channels.settings.inject_request_fields）；
    /// 在 strip 之后浅合并。model/messages/stream/provider 受保护。
    pub inject_request_fields: serde_json::Map<String, serde_json::Value>,
    /// `/v1/responses` 入口对该渠道是否同方言直转（channels.settings.responses_native）。
    /// 缺省：provider=openai 为 true（官方必支持），openai_compat 为 false（兼容上游
    /// 多数只实现 chat，先降级保可用；确认支持的再显式打开）；其它 provider 恒 false。
    pub responses_native: bool,
    /// 上游数据面版本（channels.settings.api_version）：只对 `azure` 有意义——每个请求
    /// 都要带 `?api-version=`；None = 用 providers 侧缺省。其它 provider 忽略。
    pub api_version: Option<String>,
    /// SigV4 签名区域覆写（channels.settings.aws_region）：只对 `bedrock` 有意义；
    /// None = 从 api_base 主机名解析（IMPLEMENTATION §11.35）。
    pub aws_region: Option<String>,
    /// OAuth token 端点覆写（channels.settings.oauth_token_url）：只对 `anthropic_max` / `codex`
    /// 有意义，测试 mock / 企业代理用；None = 各家官方地址（IMPLEMENTATION §11.38）。
    pub oauth_token_url: Option<String>,
    /// Provider-owned request extensions. Store and scheduler do not interpret their payload.
    pub extensions: serde_json::Value,
    /// 出站代理 URL（已解密）：出口绑定解析的结果（IMPLEMENTATION §11.41），None = 直连。
    /// 解析不出代理的候选（停用 / 未分配 / 全组熔断）根本不会出现在候选里——不会退回直连。
    /// 轮换组每次尝试前由 [`ChannelCandidate::reroll_egress`] 重抽。
    pub proxy_url: Option<String>,
    /// 本次尝试走的代理（连接失败归因、会话固定用）；None = 直连。
    pub egress_proxy_id: Option<i64>,
    /// 该代理的在途并发上限（`proxies.max_concurrency`，准入时与 key 租约一起占）；None = 不限。
    pub egress_max_concurrency: Option<i32>,
    /// 轮换组的健康成员（priority 降序）；None = 直连 / 单个代理 / 固定分配。
    pub egress_rotation: Option<Arc<[EgressMember]>>,
    /// 额外请求头（channels.settings.extra_headers）：鉴权 / 逐跳 / Host 等受保护键
    /// 在写入时已拒，热路径再跳过一次。
    pub extra_headers: Vec<(String, String)>,
    /// 能力声明（显式 false 才排除，IMPLEMENTATION §3.8）。
    pub capabilities: serde_json::Value,
    /// 相对成本千分比（缺省 1000；明确零保留，调度除数另取至少 1）。
    pub cost_milli: i64,
    /// 瞬态失败时同一把 key 的重试次数（channels.retry_policy，缺省 1）。
    pub same_key_retries: i16,
    /// 首字窗口秒数（channels.retry_policy，缺省 30）。
    ///
    /// 这一项按渠道配是有实义的：直连官方与经两跳转售的上游，首 token 该等多久
    /// 差一个数量级，一个全局常数要么把慢渠道误判成超时，要么让快渠道的故障
    /// 拖满 30 秒才换。
    pub first_output_timeout_secs: u64,
    /// 所属池在池链里的序号（0 = 主池，1 = 降级池）。调度先耗尽低序号的全部层，
    /// 再进入下一序号——降级池的高优先级渠道也排在主池最低优先级之后。
    pub pool_rank: i32,
}

/// 内置默认池：新渠道缺省加入、未指定池的分组走这里。
pub const DEFAULT_POOL: &str = "default";

/// 轮换组的一个可选出口（已解密）。
#[derive(Debug, Clone)]
pub struct EgressMember {
    pub proxy_id: i64,
    pub url: String,
    pub priority: i32,
    pub weight: i32,
    pub max_concurrency: Option<i32>,
}

/// 最高 priority 层内按 weight 加权随机抽一个（指数时钟，与渠道调度同一抽样法）。
/// `members` 已按 priority 降序。
#[allow(clippy::cast_precision_loss)] // weight 是 i32，f64 精确表示
fn pick_member(members: &[EgressMember]) -> Option<&EgressMember> {
    let top = members.first()?.priority;
    let mut rng = rand::rng();
    members
        .iter()
        .take_while(|m| m.priority == top)
        .map(|m| {
            let uniform = 1.0 - rng.random::<f64>(); // (0,1]，ln 恒有限
            (-uniform.ln() / f64::from(m.weight.max(1)), m)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, m)| m)
}

/// 候选查询带回的轮换成员 JSON → 解密后的成员表。
fn rotation_members(
    master_key: Option<&str>,
    value: serde_json::Value,
) -> Result<Vec<EgressMember>, StoreError> {
    #[derive(serde::Deserialize)]
    struct Raw {
        id: i64,
        url: String,
        priority: i32,
        weight: i32,
        #[serde(default)]
        max_concurrency: Option<i32>,
    }
    let raw: Vec<Raw> = serde_json::from_value(value)
        .map_err(|_| StoreError::InvalidData("egress_rotation_members"))?;
    raw.into_iter()
        .map(|m| {
            let stored =
                hex::decode(&m.url).map_err(|_| StoreError::InvalidData("egress_rotation_url"))?;
            Ok(EgressMember {
                proxy_id: m.id,
                url: crate::credential::open(master_key, &stored)?,
                priority: m.priority,
                weight: m.weight,
                max_concurrency: m.max_concurrency,
            })
        })
        .collect()
}

fn extra_headers_from(value: Option<serde_json::Value>) -> Vec<(String, String)> {
    value
        .and_then(|v| v.as_object().cloned())
        .map(|obj| {
            obj.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

impl ChannelCandidate {
    /// 轮换组：每次尝试前重抽一个成员（缓存的候选也逐次重抽，不会一个 TTL 内钉死一个出口）。
    /// 其他绑定不变。
    pub fn reroll_egress(&mut self) {
        if let Some(member) = self.egress_rotation.as_deref().and_then(pick_member) {
            self.proxy_url = Some(member.url.clone());
            self.egress_proxy_id = Some(member.proxy_id);
            self.egress_max_concurrency = member.max_concurrency;
        }
    }

    /// 该候选此刻能不能走 `proxy_id` 这个出口（None = 直连）。会话型连接据此沿用首轮出口：
    /// 轮换组只要它还是健康成员就行，其他绑定必须正好是它。
    #[must_use]
    pub fn egress_admits(&self, proxy_id: Option<i64>) -> bool {
        match &self.egress_rotation {
            Some(members) => proxy_id.is_some_and(|id| members.iter().any(|m| m.proxy_id == id)),
            None => self.egress_proxy_id == proxy_id,
        }
    }

    /// 把出口钉成给定代理（调用方先用 [`Self::egress_admits`] 判过）。并发上限取候选里
    /// 该代理的现值（轮换成员表或本身的出口），钉住的代理已不在其中时沿用候选原值。
    pub fn pin_egress(&mut self, proxy_id: Option<i64>, proxy_url: Option<String>) {
        if let Some(member) = self
            .egress_rotation
            .as_deref()
            .and_then(|members| members.iter().find(|m| Some(m.proxy_id) == proxy_id))
        {
            self.egress_max_concurrency = member.max_concurrency;
        }
        self.egress_proxy_id = proxy_id;
        self.proxy_url = proxy_url;
    }

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
// 一条大查询 + 逐列装配：拆开只会让 SQL 与结构体字段两处失联
#[allow(clippy::too_many_lines)]
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
               c.settings -> 'inject_request_fields' AS inject_request_fields,
               (c.settings ->> 'responses_native')::boolean AS responses_native,
               c.settings ->> 'data_retention' AS data_retention,
               NULLIF(c.settings ->> 'api_version', '') AS api_version,
               NULLIF(c.settings ->> 'aws_region', '') AS aws_region,
               NULLIF(c.settings ->> 'oauth_token_url', '') AS oauth_token_url,
               c.settings -> 'extensions' AS extensions,
               ep.mode AS "egress_mode!",
               ep.proxy_id AS "egress_proxy_id?",
               ep.url_ciphertext AS "egress_url?",
               ep.max_concurrency AS "egress_max_concurrency?",
               rot.members AS "egress_members?",
               c.settings -> 'extra_headers' AS extra_headers,
               c.capabilities,
               GREATEST(COALESCE((c.upstream_unit_cost ->> 'relative_cost_milli')::bigint, 1000), 0) AS "cost_milli!",
               c.retry_policy,
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
        -- 出口（§11.41）：初抽一个；轮换组另带健康成员表供每次尝试重抽
        CROSS JOIN LATERAL egress_pick(c.id, ck.id, true) ep
        LEFT JOIN LATERAL (
            SELECT json_agg(json_build_object(
                       'id', p.id, 'url', encode(p.url_ciphertext, 'hex'),
                       'priority', m.priority, 'weight', m.weight,
                       'max_concurrency', p.max_concurrency)
                   ORDER BY m.priority DESC, p.id) AS members
            FROM channel_egress e
            JOIN proxy_groups g ON g.code = e.group_code AND g.mode = 'rotate'
            JOIN proxy_group_members m ON m.group_code = g.code
            JOIN proxies p ON p.id = m.proxy_id
            WHERE e.channel_id = c.id AND e.mode = 'group' AND p.status = 1
              AND (p.cooldown_until IS NULL OR p.cooldown_until <= now())
        ) rot ON true
        WHERE c.status = 1
          AND c.deleted_at IS NULL
          AND c.models @> $1
          AND ck.status = 1
          AND (ck.cooldown_until IS NULL OR ck.cooldown_until < now())
          AND (ck.model_subset IS NULL OR ck.model_subset @> $1)
          -- 绑了出口却解析不出代理：不可调度（绝不退回直连）
          AND (ep.mode = 'direct' OR ep.proxy_id IS NOT NULL)
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
            let responses_native = responses_native_for(&r.provider, r.responses_native);
            let (same_key_retries, first_output_timeout_secs) =
                retry_knobs(r.retry_policy.as_ref());
            let (proxy_url, egress_proxy_id) = match crate::egress::resolved(
                master_key,
                &r.egress_mode,
                r.egress_proxy_id,
                r.egress_url.as_deref(),
            )? {
                Resolved::Proxy { id, url } => (Some(url), Some(id)),
                Resolved::Direct | Resolved::Unavailable => (None, None),
            };
            let egress_rotation = r
                .egress_members
                .map(|members| rotation_members(master_key, members))
                .transpose()?
                .filter(|members| !members.is_empty())
                .map(Arc::from);
            Ok(ChannelCandidate {
                api_version: r.api_version,
                aws_region: r.aws_region,
                oauth_token_url: r.oauth_token_url,
                extensions: r.extensions.unwrap_or_default(),
                proxy_url,
                egress_proxy_id,
                egress_max_concurrency: egress_proxy_id.and(r.egress_max_concurrency),
                egress_rotation,
                extra_headers: extra_headers_from(r.extra_headers),
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
                data_retention: r.data_retention,
                thinking_to_content: r.thinking_to_content,
                bill_by_response_model: r.bill_by_response_model,
                strip_request_fields: r
                    .strip_request_fields
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default(),
                inject_request_fields: r
                    .inject_request_fields
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default(),
                responses_native,
                capabilities: r.capabilities,
                cost_milli: r.cost_milli,
                same_key_retries,
                first_output_timeout_secs,
                pool_rank: r.pool_rank,
            })
        })
        .collect()
}

/// `channels.retry_policy` 的两个生效值：(同 key 重试次数, 首字窗口秒)。
fn retry_knobs(policy: Option<&serde_json::Value>) -> (i16, u64) {
    // 已 clamp 进 [min, max]，窄化不可能失败；try_from 只是免掉 `as` 的静默截断
    let same_key_retries =
        i16::try_from(retry_policy_i64(policy, "same_key_retries", 1, 0, 3)).unwrap_or(1);
    let first_output_timeout_secs = u64::try_from(retry_policy_i64(
        policy,
        "first_output_timeout_secs",
        30,
        5,
        300,
    ))
    .unwrap_or(30);
    (same_key_retries, first_output_timeout_secs)
}

/// `settings.responses_native` 的生效值：显式配置只在说 OpenAI 方言的渠道上有意义，
/// anthropic / gemini / custom_pass 渠道无论写什么都走降级链（它们根本没有 /responses）。
#[must_use]
pub fn responses_native_for(provider: &str, configured: Option<bool>) -> bool {
    okapi_api::provider_contract::native_responses_for(provider).enabled(configured)
}

/// 从 `channels.retry_policy` 取一个整数项，并夹在 [min, max]。
///
/// 夹取而不是照单全收：这是渠道级配置，写错一个 0 或多一个零，代价分别是
/// "永不重试"和"一个坏渠道把请求吊死几分钟"。缺省值 = 夹取前的历史常数，
/// 未配置的渠道行为一字不变。
fn retry_policy_i64(
    policy: Option<&serde_json::Value>,
    key: &str,
    default: i64,
    min: i64,
    max: i64,
) -> i64 {
    policy
        .and_then(|p| p.get(key))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(default)
        .clamp(min, max)
}

/// custom_pass 渠道点查结果。
pub struct PassChannel {
    pub key_id: i64,
    pub max_concurrency: Option<i32>,
    pub api_base: String,
    pub credential: String,
    pub settings: serde_json::Value,
    /// 出口代理（已解密，§11.41）；None = 直连。
    pub proxy_url: Option<String>,
    pub egress_proxy_id: Option<i64>,
    pub egress_max_concurrency: Option<i32>,
}

/// custom_pass 渠道点查（可见性矩阵与候选查询同语义）。
/// videos 任务回源点查结果（channel_key_id → 连接信息）。
#[derive(Debug)]
pub struct ChannelKeyRef {
    pub channel_id: i64,
    pub api_base: Option<String>,
    pub credential: String,
    pub proxy_url: Option<String>,
    pub extra_headers: Vec<(String, String)>,
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
        SELECT c.id AS channel_id, c.api_base, ck.credential_ciphertext,
               ep.mode AS "egress_mode!", ep.proxy_id AS "egress_proxy_id?",
               ep.url_ciphertext AS "egress_url?",
               c.settings -> 'extra_headers' AS extra_headers
        FROM channel_keys ck
        JOIN channels c ON c.id = ck.channel_id
        CROSS JOIN LATERAL egress_pick(c.id, ck.id, false) ep
        WHERE ck.id = $1 AND ck.status = 1 AND c.status = 1 AND c.deleted_at IS NULL
        "#,
        channel_key_id
    )
    .fetch_optional(pool)
    .await?;
    // 曾经是 from_utf8_lossy：解不出就悄悄发一串替换字符给上游，只会换来一个
    // 难查的 401。信封化后一律走 open，失败即 Err。
    // 回源与创建任务同一出口；出口不可用即视同渠道不可用（返回 None），绝不改走直连
    let Some(r) = row else {
        return Ok(None);
    };
    let proxy_url = match crate::egress::resolved(
        master_key,
        &r.egress_mode,
        r.egress_proxy_id,
        r.egress_url.as_deref(),
    )? {
        Resolved::Direct => None,
        Resolved::Proxy { url, .. } => Some(url),
        Resolved::Unavailable => return Ok(None),
    };
    Ok(Some(ChannelKeyRef {
        channel_id: r.channel_id,
        api_base: r.api_base,
        credential: crate::credential::open(master_key, &r.credential_ciphertext)?,
        proxy_url,
        extra_headers: extra_headers_from(r.extra_headers),
    }))
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
        SELECT c.api_base, c.settings, ck.credential_ciphertext, ck.id AS key_id, ck.max_concurrency,
               ep.mode AS "egress_mode!", ep.proxy_id AS "egress_proxy_id?",
               ep.url_ciphertext AS "egress_url?", ep.max_concurrency AS "egress_max_concurrency?"
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        CROSS JOIN LATERAL egress_pick(c.id, ck.id, true) ep
        WHERE c.id = $1
          AND c.provider = 'custom_pass'
          AND c.status = 1
          AND c.deleted_at IS NULL
          AND ck.status = 1
          AND (ep.mode = 'direct' OR ep.proxy_id IS NOT NULL)
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
        let (proxy_url, egress_proxy_id) = match crate::egress::resolved(
            master_key,
            &r.egress_mode,
            r.egress_proxy_id,
            r.egress_url.as_deref(),
        )? {
            Resolved::Proxy { id, url } => (Some(url), Some(id)),
            Resolved::Direct | Resolved::Unavailable => (None, None),
        };
        Ok(PassChannel {
            key_id: r.key_id,
            max_concurrency: r.max_concurrency,
            api_base: r.api_base.unwrap_or_default(),
            credential,
            settings: r.settings,
            proxy_url,
            egress_proxy_id,
            egress_max_concurrency: egress_proxy_id.and(r.egress_max_concurrency),
        })
    })
    .transpose()
}

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
            sqlx::query!(
                r#"
                UPDATE channel_keys
                SET failed_count = failed_count + 1,
                    last_error = $2,
                    status = 4,
                    cooldown_until = greatest(cooldown_until, date_trunc('day', now() + interval '1 day')),
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
