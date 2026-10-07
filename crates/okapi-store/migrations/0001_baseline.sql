-- Okapi 数据库基线（2026-10-06 由原 0001–0039 号迁移压成单个文件）。
--
-- 内容等价于依次执行原来的 39 个迁移后的最终结构与种子数据（已用两库 pg_dump 逐项比对）；
-- 只对旧数据有意义的修补（历史回填、旧配置改写、作废旧登录 key 等）不再保留。
-- 各表与列的设计说明见 docs/database.md 与 IMPLEMENTATION.md；逐次演进见 git 历史。
-- 以后的结构变更照常追加新的编号迁移（0002 起）。

-- ════════ 身份、密钥与权限 ════════

CREATE TABLE users (
    id bigint GENERATED ALWAYS AS IDENTITY,
    email varchar(255),
    username varchar(64) NOT NULL,
    password_hash varchar(255),
    role smallint DEFAULT 1 NOT NULL,
    admin_role_id bigint,
    status smallint DEFAULT 1 NOT NULL,
    kind varchar(8) DEFAULT 'user' NOT NULL,
    price_multiplier numeric(8,4) DEFAULT 1 NOT NULL,
    balance_micro bigint DEFAULT 0 NOT NULL,
    balance_expires_at timestamptz,
    language varchar(8) DEFAULT 'auto' NOT NULL,
    totp_secret_ciphertext bytea,
    aff_code varchar(16),
    inviter_id bigint,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    deleted_at timestamptz,
    totp_last_counter bigint,
    CONSTRAINT users_aff_code_key UNIQUE (aff_code),
    CONSTRAINT users_email_key UNIQUE (email),
    CONSTRAINT users_pkey PRIMARY KEY (id),
    CONSTRAINT users_username_key UNIQUE (username)
);
CREATE UNIQUE INDEX idx_users_aff_code ON users (aff_code) WHERE (aff_code IS NOT NULL);
CREATE INDEX idx_users_inviter ON users (inviter_id) WHERE (inviter_id IS NOT NULL);

CREATE TABLE admin_roles (
    id bigint GENERATED ALWAYS AS IDENTITY,
    role_code varchar(64) NOT NULL,
    display_name varchar(128) NOT NULL,
    permissions jsonb DEFAULT '[]'::jsonb NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT admin_roles_pkey PRIMARY KEY (id),
    CONSTRAINT admin_roles_role_code_key UNIQUE (role_code)
);

CREATE TABLE oauth_identities (
    id bigint GENERATED ALWAYS AS IDENTITY,
    provider varchar(32) NOT NULL,
    subject varchar(255) NOT NULL,
    user_id bigint NOT NULL,
    display varchar(255),
    created_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT oauth_identities_pkey PRIMARY KEY (id),
    CONSTRAINT oauth_identities_provider_subject_key UNIQUE (provider, subject)
);
CREATE INDEX idx_oauth_identities_user ON oauth_identities (user_id);

CREATE TABLE user_groups (
    user_id bigint NOT NULL,
    group_code varchar(32) NOT NULL,
    priority integer DEFAULT 0 NOT NULL,
    CONSTRAINT user_groups_pkey PRIMARY KEY (user_id, group_code)
);

CREATE TABLE user_pricing (
    id bigint GENERATED ALWAYS AS IDENTITY,
    user_id bigint NOT NULL,
    model_id bigint NOT NULL,
    override_kind varchar(8) NOT NULL,
    custom_model_ratio numeric(12,6),
    custom_completion_ratio numeric(12,6),
    custom_cache_ratio numeric(6,4),
    custom_cache_write_ratio numeric(6,4),
    custom_input_per_1m_micro bigint,
    custom_output_per_1m_micro bigint,
    reason varchar(255),
    expires_at timestamptz,
    CONSTRAINT user_pricing_pkey PRIMARY KEY (id),
    CONSTRAINT user_pricing_user_id_model_id_key UNIQUE (user_id, model_id)
);
COMMENT ON COLUMN user_pricing.custom_cache_write_ratio IS '用户专属缓存写入倍率覆盖；NULL = 用模型级值';

CREATE TABLE team_members (
    team_user_id bigint NOT NULL,
    member_user_id bigint NOT NULL,
    role varchar(16) DEFAULT 'member' NOT NULL,
    monthly_spend_limit_micro bigint,
    created_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT team_members_pkey PRIMARY KEY (team_user_id, member_user_id)
);
CREATE INDEX idx_team_members_member ON team_members (member_user_id);

CREATE TABLE api_keys (
    id bigint GENERATED ALWAYS AS IDENTITY,
    user_id bigint NOT NULL,
    team_id bigint,
    member_user_id bigint,
    name varchar(128) DEFAULT '' NOT NULL,
    key_hash character(64) NOT NULL,
    key_prefix varchar(16) NOT NULL,
    status smallint DEFAULT 1 NOT NULL,
    quota_mode smallint DEFAULT 0 NOT NULL,
    quota_micro bigint,
    used_micro bigint DEFAULT 0 NOT NULL,
    model_allowlist jsonb,
    group_override varchar(32),
    pool_override varchar(32),
    rpm_limit integer,
    tpm_limit integer,
    rpd_limit integer,
    daily_token_limit bigint,
    max_concurrency integer,
    ip_allowlist jsonb,
    expires_at timestamptz,
    last_used_at timestamptz,
    created_at timestamptz DEFAULT now() NOT NULL,
    deleted_at timestamptz,
    key_ciphertext bytea,
    session_hash text,
    CONSTRAINT api_keys_session_hash_shape CHECK (((session_hash IS NULL) OR (session_hash ~ '^[0-9a-f]{64}$'::text))),
    CONSTRAINT api_keys_key_hash_key UNIQUE (key_hash),
    CONSTRAINT api_keys_pkey PRIMARY KEY (id)
);
CREATE UNIQUE INDEX api_keys_live_session_key ON api_keys (session_hash) WHERE ((session_hash IS NOT NULL) AND (deleted_at IS NULL));
CREATE INDEX api_keys_user_session_keys ON api_keys (user_id) WHERE ((session_hash IS NOT NULL) AND (deleted_at IS NULL));
CREATE INDEX idx_api_keys_user ON api_keys (user_id) WHERE (deleted_at IS NULL);
COMMENT ON COLUMN api_keys.pool_override IS '令牌钉住某渠道池，优先于分组的池；null = 跟随分组';
COMMENT ON COLUMN api_keys.key_ciphertext IS 'Optional encrypted API key for owner-session copy; NULL for legacy/hash-only keys';

-- ════════ 站点设置与审计 ════════

CREATE TABLE settings (
    key varchar(128) NOT NULL,
    value jsonb NOT NULL,
    updated_by bigint,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT settings_pkey PRIMARY KEY (key)
);

CREATE TABLE audit_logs (
    id bigint GENERATED ALWAYS AS IDENTITY,
    actor varchar(64) NOT NULL,
    action varchar(64) NOT NULL,
    target varchar(128),
    detail jsonb,
    ip inet,
    created_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT audit_logs_pkey PRIMARY KEY (id, created_at)
)
PARTITION BY RANGE (created_at);

CREATE TABLE audit_logs_default PARTITION OF audit_logs DEFAULT;

CREATE INDEX idx_audit_actor_time ON audit_logs (actor, created_at DESC);

-- ════════ 模型与定价 ════════

CREATE TABLE models (
    id bigint GENERATED ALWAYS AS IDENTITY,
    model_name varchar(128) NOT NULL,
    display_name varchar(128),
    vendor varchar(64),
    capabilities jsonb DEFAULT '{}'::jsonb NOT NULL,
    context_window integer,
    max_output integer,
    status smallint DEFAULT 1 NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    fallback_models jsonb DEFAULT '[]'::jsonb NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    catalog_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    CONSTRAINT models_catalog_config_object CHECK ((jsonb_typeof(catalog_config) = 'object'::text)),
    CONSTRAINT models_model_name_key UNIQUE (model_name),
    CONSTRAINT models_pkey PRIMARY KEY (id)
);
COMMENT ON COLUMN models.fallback_models IS '零可用候选时的降级模型名数组，单跳不递归；不覆盖上游 4xx 与用户参数错误。计费按实际服务模型（DESIGN §3.4.1）';

CREATE TABLE model_aliases (
    id bigint GENERATED ALWAYS AS IDENTITY,
    pattern varchar(128) NOT NULL,
    target_model varchar(128) NOT NULL,
    priority integer DEFAULT 0 NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    CONSTRAINT model_aliases_pattern_key UNIQUE (pattern),
    CONSTRAINT model_aliases_pkey PRIMARY KEY (id)
);

CREATE TABLE model_pricing (
    model_id bigint NOT NULL,
    pricing_mode varchar(16) DEFAULT 'ratio' NOT NULL,
    model_ratio numeric(12,6),
    completion_ratio numeric(12,6) DEFAULT 1 NOT NULL,
    cache_ratio numeric(12,6) DEFAULT 1 NOT NULL,
    cache_write_ratio numeric(12,6) DEFAULT 1 NOT NULL,
    audio_ratio numeric(12,6) DEFAULT 1 NOT NULL,
    audio_completion_ratio numeric(12,6) DEFAULT 1 NOT NULL,
    image_ratio numeric(12,6) DEFAULT 1 NOT NULL,
    per_call_price_micro bigint,
    tier_expr text,
    tier_ratios jsonb,
    effective_from timestamptz,
    updated_by bigint,
    updated_at timestamptz DEFAULT now() NOT NULL,
    modality_ratios jsonb DEFAULT '{}'::jsonb NOT NULL,
    server_tool_prices jsonb,
    CONSTRAINT model_pricing_modality_ratios_check CHECK ((jsonb_typeof(modality_ratios) = 'object'::text)),
    CONSTRAINT server_tool_prices_object CHECK (((server_tool_prices IS NULL) OR (jsonb_typeof(server_tool_prices) = 'object'::text))),
    CONSTRAINT model_pricing_pkey PRIMARY KEY (model_id)
);
COMMENT ON COLUMN model_pricing.cache_write_ratio IS '缓存写入倍率（Anthropic cache_creation；1.0=按常规输入计，官方 1.25×@5m / 2.0×@1h）';
COMMENT ON COLUMN model_pricing.audio_ratio IS '音频输入倍率（相对文本；gpt-4o-audio 官方 16.0，缺省 1.0=按文本计）';
COMMENT ON COLUMN model_pricing.audio_completion_ratio IS '音频输出倍率，叠乘在 audio_ratio 之上（官方 2.0 → 输出 = 文本×16×2）';
COMMENT ON COLUMN model_pricing.image_ratio IS '图片输入倍率（相对文本，缺省 1.0）';
COMMENT ON COLUMN model_pricing.tier_ratios IS 'service_tier 档位倍率，如 {"flex":"0.5","priority":"2.0"}；NULL = 全档 1.0。结算档取请求声明档与上游响应档中倍率较低者（只降不升）';

CREATE TABLE price_groups (
    group_code varchar(32) NOT NULL,
    group_ratio numeric(6,4) DEFAULT 1 NOT NULL,
    description varchar(255),
    is_default boolean DEFAULT false NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    pool_code varchar(32) DEFAULT 'default' NOT NULL,
    self_select boolean DEFAULT false NOT NULL,
    rpm_limit integer,
    rph_limit integer,
    CONSTRAINT price_groups_rph_limit_check CHECK (((rph_limit IS NULL) OR (rph_limit > 0))),
    CONSTRAINT price_groups_rpm_limit_check CHECK (((rpm_limit IS NULL) OR (rpm_limit > 0))),
    CONSTRAINT price_groups_pkey PRIMARY KEY (group_code)
);
COMMENT ON COLUMN price_groups.pool_code IS '该分组的用户打哪个池；缺省 default。池的 fallback_pool_code 再决定池内无候选时退到哪';
COMMENT ON COLUMN price_groups.self_select IS '用户可在门户为自己的 key 选择此分组（价随组走）；false = 仅管理员可分配';
COMMENT ON COLUMN price_groups.rpm_limit IS '分组内每用户每分钟请求上限（固定分钟窗，reserve 前检查；超限 429 rate_limited/group_rpm）；NULL = 不限';
COMMENT ON COLUMN price_groups.rph_limit IS '分组内每用户每小时请求上限（固定小时窗）；NULL = 不限';

CREATE TABLE pricing_epochs (
    epoch bigint GENERATED ALWAYS AS IDENTITY,
    snapshot jsonb NOT NULL,
    diff_summary jsonb,
    published_by bigint,
    published_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT pricing_epochs_pkey PRIMARY KEY (epoch)
);

CREATE TABLE pricing_rules (
    rule_code varchar(64) NOT NULL,
    rule_type varchar(16) NOT NULL,
    scope jsonb DEFAULT '{}'::jsonb NOT NULL,
    params jsonb NOT NULL,
    priority integer DEFAULT 0 NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    valid_from timestamptz,
    valid_to timestamptz,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT pricing_rules_pkey PRIMARY KEY (rule_code)
);

CREATE TABLE plans (
    id bigint GENERATED ALWAYS AS IDENTITY,
    plan_code varchar(64) NOT NULL,
    display_name varchar(128) NOT NULL,
    grant_micro bigint NOT NULL,
    group_code varchar(64),
    balance_valid_days integer,
    status smallint DEFAULT 1 NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    kind smallint DEFAULT 0 NOT NULL,
    price_micro bigint DEFAULT 0 NOT NULL,
    period smallint,
    duration_days integer,
    sort_order integer DEFAULT 0 NOT NULL,
    description text,
    CONSTRAINT plans_balance_valid_days_check CHECK ((balance_valid_days > 0)),
    CONSTRAINT plans_duration_days_check CHECK ((duration_days > 0)),
    CONSTRAINT plans_grant_micro_check CHECK ((grant_micro > 0)),
    CONSTRAINT plans_price_micro_check CHECK ((price_micro >= 0)),
    CONSTRAINT plans_subscription_shape CHECK (((kind = 0) OR ((period = ANY (ARRAY[1, 2, 3])) AND (duration_days IS NOT NULL)))),
    CONSTRAINT plans_pkey PRIMARY KEY (id),
    CONSTRAINT plans_plan_code_key UNIQUE (plan_code)
);

-- ════════ 订阅、充值与兑换 ════════

CREATE TABLE user_subscriptions (
    id bigint GENERATED ALWAYS AS IDENTITY,
    user_id bigint NOT NULL,
    plan_id bigint NOT NULL,
    status smallint DEFAULT 1 NOT NULL,
    starts_at timestamptz DEFAULT now() NOT NULL,
    expires_at timestamptz NOT NULL,
    window_start timestamptz NOT NULL,
    window_end timestamptz NOT NULL,
    quota_micro bigint NOT NULL,
    granted_group boolean DEFAULT false NOT NULL,
    source varchar(96) NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    plan_code_snapshot varchar(64) NOT NULL,
    display_name_snapshot varchar(128) NOT NULL,
    period_snapshot smallint NOT NULL,
    group_code_snapshot varchar(64),
    maintenance_retry_after timestamptz,
    CONSTRAINT user_subscriptions_period_snapshot_check CHECK ((period_snapshot = ANY (ARRAY[1, 2, 3]))),
    CONSTRAINT user_subscriptions_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_user_sub_window ON user_subscriptions (window_end) WHERE (status = 1);
CREATE UNIQUE INDEX uq_user_sub_active ON user_subscriptions (user_id) WHERE (status = 1);

CREATE TABLE subscription_grants (
    id uuid NOT NULL,
    sequence bigserial,
    user_id bigint NOT NULL,
    source varchar(96) NOT NULL,
    plan_snapshot jsonb NOT NULL,
    actor varchar(64) NOT NULL,
    subscription_id bigint,
    outcome varchar(16),
    created_at timestamptz DEFAULT now() NOT NULL,
    applied_at timestamptz,
    last_error varchar(64),
    retry_after timestamptz,
    CONSTRAINT subscription_grants_check CHECK (((applied_at IS NULL) = (subscription_id IS NULL))),
    CONSTRAINT subscription_grants_check1 CHECK (((applied_at IS NULL) = (outcome IS NULL))),
    CONSTRAINT subscription_grants_outcome_check CHECK ((outcome IN ('activated', 'renewed'))),
    CONSTRAINT subscription_grants_pkey PRIMARY KEY (id),
    CONSTRAINT subscription_grants_sequence_key UNIQUE (sequence),
    CONSTRAINT subscription_grants_user_id_source_key UNIQUE (user_id, source)
);
CREATE INDEX subscription_grants_pending ON subscription_grants (sequence) WHERE (applied_at IS NULL);

CREATE TABLE subscription_sync (
    user_id bigint NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    retry_after timestamptz,
    CONSTRAINT subscription_sync_pkey PRIMARY KEY (user_id)
);

CREATE TABLE recharge_orders (
    id bigint GENERATED ALWAYS AS IDENTITY,
    order_no varchar(64) NOT NULL,
    user_id bigint NOT NULL,
    amount_micro bigint NOT NULL,
    currency varchar(8) DEFAULT 'USD' NOT NULL,
    pay_amount numeric(12,2),
    gateway varchar(32) NOT NULL,
    gateway_trade_no varchar(128),
    status smallint DEFAULT 0 NOT NULL,
    paid_at timestamptz,
    created_at timestamptz DEFAULT now() NOT NULL,
    plan_id bigint,
    subscription_snapshot jsonb,
    payment_contract_version smallint DEFAULT 0 NOT NULL,
    merchant_id varchar(128),
    checkout_session_id varchar(128),
    CONSTRAINT recharge_orders_amount_micro_check CHECK ((amount_micro > 0)),
    CONSTRAINT recharge_orders_order_no_key UNIQUE (order_no),
    CONSTRAINT recharge_orders_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_recharge_user ON recharge_orders (user_id, created_at DESC);
CREATE UNIQUE INDEX recharge_checkout_session_unique ON recharge_orders (gateway, checkout_session_id) WHERE (checkout_session_id IS NOT NULL);
CREATE INDEX recharge_paid_transaction_lookup ON recharge_orders (gateway, gateway_trade_no) WHERE (status = ANY (ARRAY[1, 3]));

CREATE TABLE payment_receipts (
    gateway varchar(32) NOT NULL,
    merchant_id varchar(128) NOT NULL,
    trade_no varchar(128) NOT NULL,
    order_id bigint NOT NULL,
    currency varchar(8) NOT NULL,
    amount_minor bigint NOT NULL,
    accepted_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT payment_receipts_amount_minor_check CHECK ((amount_minor > 0)),
    CONSTRAINT payment_receipts_order_id_key UNIQUE (order_id),
    CONSTRAINT payment_receipts_pkey PRIMARY KEY (gateway, merchant_id, trade_no)
);

CREATE TABLE redemption_codes (
    id bigint GENERATED ALWAYS AS IDENTITY,
    code_hash varchar(64) NOT NULL,
    amount_micro bigint NOT NULL,
    status smallint DEFAULT 1 NOT NULL,
    batch_id uuid NOT NULL,
    plan_id bigint,
    bind_user_id bigint,
    max_per_ip integer,
    created_by bigint,
    redeemed_by bigint,
    redeemed_at timestamptz,
    expires_at timestamptz,
    created_at timestamptz DEFAULT now() NOT NULL,
    subscription_snapshot jsonb,
    CONSTRAINT redemption_codes_amount_micro_check CHECK ((amount_micro > 0)),
    CONSTRAINT redemption_codes_max_per_ip_check CHECK ((max_per_ip > 0)),
    CONSTRAINT redemption_codes_code_hash_key UNIQUE (code_hash),
    CONSTRAINT redemption_codes_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_redemption_batch ON redemption_codes (batch_id);

CREATE TABLE fund_transfers (
    id uuid NOT NULL,
    user_id bigint NOT NULL,
    amount_micro bigint NOT NULL,
    pool smallint NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    applied_at timestamptz,
    cleaned_at timestamptz,
    sequence bigserial,
    CONSTRAINT fund_transfers_amount_micro_check CHECK (((amount_micro >= '-9007199254740991'::bigint) AND (amount_micro <= '9007199254740991'::bigint))),
    CONSTRAINT fund_transfers_check CHECK (((cleaned_at IS NULL) OR (applied_at IS NOT NULL))),
    CONSTRAINT fund_transfers_pool_check CHECK ((pool = ANY (ARRAY[0, 1]))),
    CONSTRAINT fund_transfers_sequence_check CHECK ((sequence > 0)),
    CONSTRAINT fund_transfers_pkey PRIMARY KEY (id),
    CONSTRAINT fund_transfers_sequence_key UNIQUE (sequence)
);
CREATE INDEX fund_transfers_pending ON fund_transfers (user_id, created_at, id) WHERE (cleaned_at IS NULL);
CREATE INDEX fund_transfers_user_sequence ON fund_transfers (user_id, sequence);

-- ════════ 渠道池与渠道 ════════

CREATE TABLE channel_pools (
    pool_code varchar(32) NOT NULL,
    description varchar(255),
    routing_strategy varchar(24) DEFAULT 'priority_weighted' NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    fallback_pool_code varchar(32),
    CONSTRAINT channel_pools_fallback_not_self CHECK (((fallback_pool_code IS NULL) OR ((fallback_pool_code)::text <> (pool_code)::text))),
    CONSTRAINT channel_pools_strategy_chk CHECK ((routing_strategy IN ('priority_weighted', 'least_latency'))),
    CONSTRAINT channel_pools_pkey PRIMARY KEY (pool_code)
);
COMMENT ON COLUMN channel_pools.routing_strategy IS 'priority_weighted：priority 分层 + 层内成本修正加权随机（历史行为）；least_latency：层内按 Redis lat:ck:* 的 EWMA 升序，无数据者按中位数处理';
COMMENT ON COLUMN channel_pools.fallback_pool_code IS '本池对某模型无可用候选时退到的池（单跳，不递归）；计费仍按请求者的分组倍率';

CREATE TABLE channels (
    id bigint GENERATED ALWAYS AS IDENTITY,
    name varchar(128) NOT NULL,
    provider varchar(32) NOT NULL,
    api_base varchar(255),
    status smallint DEFAULT 1 NOT NULL,
    priority integer DEFAULT 0 NOT NULL,
    weight integer DEFAULT 1 NOT NULL,
    models jsonb DEFAULT '[]'::jsonb NOT NULL,
    model_mapping jsonb DEFAULT '{}'::jsonb NOT NULL,
    capabilities jsonb DEFAULT '{}'::jsonb NOT NULL,
    trust_upstream_usage boolean DEFAULT false NOT NULL,
    retry_policy jsonb,
    settings jsonb DEFAULT '{}'::jsonb NOT NULL,
    owner_id bigint,
    upstream_unit_cost jsonb,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    deleted_at timestamptz,
    egress_mode varchar(8),
    egress_proxy_id bigint,
    egress_group_code varchar(32),
    CONSTRAINT channels_egress_shape CHECK (
CASE COALESCE(egress_mode, ''::character varying)
    WHEN 'proxy'::text THEN ((egress_proxy_id IS NOT NULL) AND (egress_group_code IS NULL))
    WHEN 'group'::text THEN ((egress_group_code IS NOT NULL) AND (egress_proxy_id IS NULL))
    WHEN 'direct'::text THEN ((egress_proxy_id IS NULL) AND (egress_group_code IS NULL))
    WHEN ''::text THEN ((egress_proxy_id IS NULL) AND (egress_group_code IS NULL))
    ELSE false
END),
    CONSTRAINT channels_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_channels_egress_group ON channels (egress_group_code) WHERE (egress_group_code IS NOT NULL);
CREATE INDEX idx_channels_egress_proxy ON channels (egress_proxy_id) WHERE (egress_proxy_id IS NOT NULL);
COMMENT ON COLUMN channels.egress_mode IS 'NULL 继承全局默认 / direct 直连 / proxy 单个代理 / group 代理组';

CREATE TABLE channel_keys (
    id bigint GENERATED ALWAYS AS IDENTITY,
    channel_id bigint NOT NULL,
    credential_ciphertext bytea NOT NULL,
    credential_kind smallint DEFAULT 0 NOT NULL,
    status smallint DEFAULT 1 NOT NULL,
    cooldown_until timestamptz,
    failed_count integer DEFAULT 0 NOT NULL,
    last_error varchar(255),
    weight integer DEFAULT 1 NOT NULL,
    max_concurrency integer,
    model_subset jsonb,
    rpm_limit integer,
    daily_spend_cap_micro bigint,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    egress_proxy_id bigint,
    CONSTRAINT channel_keys_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_channel_keys_channel ON channel_keys (channel_id, status);
CREATE INDEX idx_channel_keys_egress_proxy ON channel_keys (egress_proxy_id) WHERE (egress_proxy_id IS NOT NULL);
COMMENT ON COLUMN channel_keys.model_subset IS 'null = 继承 channels.models；非空 = 该 key 只服务这些模型';
COMMENT ON COLUMN channel_keys.rpm_limit IS 'null = 不限。固定分钟窗计数在 Redis rpm:ck:*；超限把该 key 摘出候选而非拒绝请求';
COMMENT ON COLUMN channel_keys.daily_spend_cap_micro IS 'null = 不限。当日累计消费在 Redis spend:ck:*，结算后累加、选路前比较（软实时）';
COMMENT ON COLUMN channel_keys.egress_proxy_id IS '固定分配组分给这把 key 的代理；仅当有效绑定是 pinned 组且该代理仍是组员时生效';

CREATE TABLE pool_channels (
    pool_code varchar(32) NOT NULL,
    channel_id bigint NOT NULL,
    priority_override integer,
    weight_override integer,
    CONSTRAINT pool_channels_pkey PRIMARY KEY (pool_code, channel_id)
);
CREATE INDEX idx_pool_channels_channel ON pool_channels (channel_id);
COMMENT ON COLUMN pool_channels.priority_override IS '同一渠道在不同池里可以是主力也可以是备胎：本池内的优先级覆盖，NULL = 用渠道自身 priority';
COMMENT ON COLUMN pool_channels.weight_override IS '本池内的抽样权重覆盖（作用于该渠道全部 key），NULL = 用各 key 自身 weight';

CREATE TABLE channel_usage_windows (
    channel_id bigint NOT NULL,
    period varchar(8) NOT NULL,
    window_start timestamptz NOT NULL,
    window_end timestamptz NOT NULL,
    requests bigint DEFAULT 0 NOT NULL,
    CONSTRAINT channel_usage_windows_check CHECK ((window_end > window_start)),
    CONSTRAINT channel_usage_windows_period_check CHECK ((period IN ('hour', 'day', 'week', 'month'))),
    CONSTRAINT channel_usage_windows_requests_check CHECK ((requests >= 0)),
    CONSTRAINT channel_usage_windows_pkey PRIMARY KEY (channel_id, period, window_start)
);
CREATE INDEX channel_usage_windows_expiry ON channel_usage_windows (window_end);

CREATE TABLE channel_token_totals (
    channel_id bigint NOT NULL,
    tokens numeric(38,0) NOT NULL,
    CONSTRAINT channel_token_totals_tokens_check CHECK ((tokens >= (0)::numeric)),
    CONSTRAINT channel_token_totals_pkey PRIMARY KEY (channel_id)
);

-- ════════ 出口代理 ════════

CREATE TABLE proxies (
    id bigint GENERATED ALWAYS AS IDENTITY,
    name varchar(128) NOT NULL,
    owner_id bigint,
    url_ciphertext bytea NOT NULL,
    scheme varchar(8) NOT NULL,
    host varchar(255) NOT NULL,
    port integer NOT NULL,
    username varchar(255),
    status smallint DEFAULT 1 NOT NULL,
    max_keys integer,
    failed_count integer DEFAULT 0 NOT NULL,
    cooldown_until timestamptz,
    last_error varchar(255),
    exit_ip varchar(64),
    exit_country varchar(8),
    latency_ms integer,
    checked_at timestamptz,
    note varchar(255),
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    max_concurrency integer,
    previous_exit_ip varchar(64),
    exit_ip_changed_at timestamptz,
    CONSTRAINT proxies_max_concurrency_chk CHECK (((max_concurrency IS NULL) OR (max_concurrency > 0))),
    CONSTRAINT proxies_max_keys_chk CHECK (((max_keys IS NULL) OR (max_keys > 0))),
    CONSTRAINT proxies_port_chk CHECK (((port >= 1) AND (port <= 65535))),
    CONSTRAINT proxies_scheme_chk CHECK ((scheme IN ('http', 'https', 'socks5', 'socks5h'))),
    CONSTRAINT proxies_status_chk CHECK ((status = ANY (ARRAY[1, 2]))),
    CONSTRAINT proxies_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_proxies_owner ON proxies (owner_id);
COMMENT ON COLUMN proxies.status IS '1 启用 / 2 手动停用；熔断不改 status，只看 cooldown_until';
COMMENT ON COLUMN proxies.max_keys IS '固定分配组最多把它分给几把 key（一个出口 IP 挂几个账号）；null = 不限。只约束新分配，不驱逐已分配';
COMMENT ON COLUMN proxies.max_concurrency IS '经该代理同时在途的上游请求数上限（跨 key、跨副本，Redis 租约 conc:px:*）；null = 不限';
COMMENT ON COLUMN proxies.exit_ip_changed_at IS '最近一次测试 / 后台探测发现出口 IP 与上次不同的时刻；previous_exit_ip 是变化前的 IP';

CREATE TABLE proxy_groups (
    code varchar(32) NOT NULL,
    name varchar(128) NOT NULL,
    mode varchar(8) DEFAULT 'pinned' NOT NULL,
    owner_id bigint,
    description varchar(255),
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT proxy_groups_mode_chk CHECK ((mode IN ('pinned', 'rotate'))),
    CONSTRAINT proxy_groups_pkey PRIMARY KEY (code)
);
COMMENT ON COLUMN proxy_groups.mode IS 'pinned：每把 key 分到组内一个固定代理（持久化在 channel_keys.egress_proxy_id），代理熔断时等它恢复、不换 IP；rotate：每次请求在健康成员里按 priority 分层 + 层内加权随机选';

CREATE TABLE proxy_group_members (
    group_code varchar(32) NOT NULL,
    proxy_id bigint NOT NULL,
    priority integer DEFAULT 0 NOT NULL,
    weight integer DEFAULT 1 NOT NULL,
    CONSTRAINT proxy_group_members_weight_chk CHECK ((weight > 0)),
    CONSTRAINT proxy_group_members_pkey PRIMARY KEY (group_code, proxy_id)
);
CREATE INDEX idx_proxy_group_members_proxy ON proxy_group_members (proxy_id);

-- ════════ 预扣、计费与账本 ════════

CREATE TABLE balance_holds (
    id uuid NOT NULL,
    user_id bigint NOT NULL,
    api_key_id bigint NOT NULL,
    model_name text NOT NULL,
    request_hash text NOT NULL,
    maximum_micro bigint NOT NULL,
    pricing_snapshot jsonb NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    pool smallint,
    source_window text,
    actual_micro bigint,
    credit_micro bigint,
    settlement jsonb,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    cancel_requested boolean DEFAULT false NOT NULL,
    CONSTRAINT balance_holds_check CHECK (((actual_micro >= 0) AND (actual_micro <= maximum_micro))),
    CONSTRAINT balance_holds_check1 CHECK (((credit_micro >= 0) AND (credit_micro <= maximum_micro))),
    CONSTRAINT balance_holds_check2 CHECK (((state = 'pending'::text) = (pool IS NULL))),
    CONSTRAINT balance_holds_check3 CHECK (((pool = 1) = (source_window IS NOT NULL))),
    CONSTRAINT balance_holds_check4 CHECK (((state = ANY (ARRAY['closing'::text, 'closed'::text])) = (settlement IS NOT NULL))),
    CONSTRAINT balance_holds_check5 CHECK (((state = ANY (ARRAY['closing'::text, 'closed'::text])) = (actual_micro IS NOT NULL))),
    CONSTRAINT balance_holds_check6 CHECK (((state = ANY (ARRAY['closing'::text, 'closed'::text])) = (credit_micro IS NOT NULL))),
    CONSTRAINT balance_holds_maximum_micro_check CHECK (((maximum_micro >= 0) AND (maximum_micro <= '9007199254740991'::bigint))),
    CONSTRAINT balance_holds_pool_check CHECK ((pool = ANY (ARRAY[0, 1]))),
    CONSTRAINT balance_holds_pricing_snapshot_check CHECK ((jsonb_typeof(pricing_snapshot) = 'object'::text)),
    CONSTRAINT balance_holds_pricing_snapshot_check1 CHECK ((octet_length((pricing_snapshot)::text) <= 65536)),
    CONSTRAINT balance_holds_request_hash_check CHECK ((length(request_hash) = 64)),
    CONSTRAINT balance_holds_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'held'::text, 'closing'::text, 'closed'::text]))),
    CONSTRAINT balance_holds_pkey PRIMARY KEY (id)
);
CREATE INDEX balance_holds_active ON balance_holds (user_id, created_at, id) WHERE (state <> 'closed'::text);
CREATE INDEX balance_holds_closing ON balance_holds (updated_at, id) WHERE (state = 'closing'::text);

CREATE TABLE billing_records (
    id bigint GENERATED ALWAYS AS IDENTITY,
    request_id uuid NOT NULL,
    upstream_request_id varchar(128),
    log_type smallint DEFAULT 2 NOT NULL,
    user_id bigint NOT NULL,
    api_key_id bigint,
    team_id bigint,
    group_code varchar(32),
    model_name varchar(128) NOT NULL,
    channel_id bigint,
    channel_key_id bigint,
    status smallint NOT NULL,
    prompt_tokens integer DEFAULT 0 NOT NULL,
    cached_tokens integer DEFAULT 0 NOT NULL,
    completion_tokens integer DEFAULT 0 NOT NULL,
    reasoning_tokens integer DEFAULT 0 NOT NULL,
    media_units jsonb,
    amount_micro bigint DEFAULT 0 NOT NULL,
    original_amount_micro bigint DEFAULT 0 NOT NULL,
    discount_micro bigint DEFAULT 0 NOT NULL,
    upstream_cost_micro bigint,
    pricing_epoch bigint,
    pricing_snapshot jsonb,
    latency_ms integer,
    ttft_ms integer,
    is_stream boolean DEFAULT false NOT NULL,
    retry_count smallint DEFAULT 0 NOT NULL,
    failover_count smallint DEFAULT 0 NOT NULL,
    sticky_layer smallint DEFAULT 0 NOT NULL,
    upstream_status smallint,
    error_code varchar(64),
    client_ip inet,
    client_type varchar(32),
    user_agent varchar(255),
    node varchar(64),
    content_ref jsonb,
    created_at timestamptz DEFAULT now() NOT NULL,
    pool smallint DEFAULT 0 NOT NULL,
    usage_details jsonb,
    source_window text,
    CONSTRAINT billing_records_pkey PRIMARY KEY (id, created_at)
)
PARTITION BY RANGE (created_at);
COMMENT ON COLUMN billing_records.usage_details IS 'Normalized token usage and public request dimensions at settlement; NULL for historical records';

CREATE TABLE billing_records_default PARTITION OF billing_records DEFAULT;

CREATE TABLE billing_record_receipts (
    request_id uuid NOT NULL,
    user_id bigint NOT NULL,
    api_key_id bigint,
    group_code varchar(32),
    model_name varchar(128) NOT NULL,
    channel_id bigint,
    channel_key_id bigint,
    status smallint NOT NULL,
    amount_micro bigint NOT NULL,
    original_amount_micro bigint NOT NULL,
    discount_micro bigint NOT NULL,
    upstream_cost_micro bigint,
    is_stream boolean NOT NULL,
    node varchar(64),
    pool smallint NOT NULL,
    pricing_snapshot jsonb,
    usage_details jsonb,
    created_at timestamptz NOT NULL,
    source_window text,
    CONSTRAINT billing_record_receipts_pool_check CHECK ((pool = ANY (ARRAY[0, 1]))),
    CONSTRAINT billing_record_receipts_pkey PRIMARY KEY (request_id)
);
CREATE INDEX billing_receipts_channel_time ON billing_record_receipts (channel_id, created_at);
CREATE INDEX billing_receipts_user ON billing_record_receipts (user_id, created_at);

CREATE TABLE billing_events (
    event_id bigint GENERATED ALWAYS AS IDENTITY,
    user_id bigint NOT NULL,
    request_id uuid,
    event_type varchar(16) NOT NULL,
    delta_micro bigint NOT NULL,
    balance_after_micro bigint,
    payload jsonb,
    actor varchar(64) NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    pool smallint DEFAULT 0 NOT NULL,
    CONSTRAINT billing_events_pkey PRIMARY KEY (event_id, created_at)
)
PARTITION BY RANGE (created_at);

CREATE TABLE billing_events_default PARTITION OF billing_events DEFAULT;

CREATE TABLE billing_event_carry (
    user_id bigint NOT NULL,
    pool smallint NOT NULL,
    actor varchar(64) NOT NULL,
    event_type varchar(16) NOT NULL,
    delta_micro numeric(38,0) NOT NULL,
    event_count bigint NOT NULL,
    CONSTRAINT billing_event_carry_event_count_check CHECK ((event_count > 0)),
    CONSTRAINT billing_event_carry_pool_check CHECK ((pool = ANY (ARRAY[0, 1]))),
    CONSTRAINT billing_event_carry_pkey PRIMARY KEY (user_id, pool, actor, event_type)
);

CREATE TABLE billing_outbox (
    id bigint GENERATED ALWAYS AS IDENTITY,
    topic varchar(64) NOT NULL,
    payload jsonb NOT NULL,
    status smallint DEFAULT 0 NOT NULL,
    retry_count integer DEFAULT 0 NOT NULL,
    next_retry_at timestamptz,
    created_at timestamptz DEFAULT now() NOT NULL,
    published_at timestamptz,
    event_id uuid DEFAULT gen_random_uuid() NOT NULL,
    ch_batch_id uuid,
    stats_protocol smallint DEFAULT 0 NOT NULL,
    CONSTRAINT billing_outbox_stats_protocol_check CHECK ((stats_protocol = ANY (ARRAY[0, 1]))),
    CONSTRAINT billing_outbox_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_outbox_ch_batch ON billing_outbox (ch_batch_id) WHERE (ch_batch_id IS NOT NULL);
CREATE UNIQUE INDEX idx_outbox_event_id ON billing_outbox (event_id);
CREATE INDEX idx_outbox_pending ON billing_outbox (next_retry_at) WHERE (status <> 1);
CREATE INDEX idx_outbox_published_unassigned ON billing_outbox (id) WHERE ((status = 1) AND (stats_protocol = 1) AND (ch_batch_id IS NULL));

CREATE TABLE billing_sync (
    request_id uuid NOT NULL,
    user_id bigint NOT NULL,
    api_key_id bigint NOT NULL,
    amount_micro bigint NOT NULL,
    pool smallint NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    source_window text,
    CONSTRAINT billing_sync_amount_micro_check CHECK (((amount_micro >= 0) AND (amount_micro <= '9007199254740991'::bigint))),
    CONSTRAINT billing_sync_pool_check CHECK ((pool = ANY (ARRAY[0, 1]))),
    CONSTRAINT billing_sync_pkey PRIMARY KEY (request_id)
);
CREATE INDEX billing_sync_user ON billing_sync (user_id, created_at, request_id);

CREATE TABLE billing_ch_batches (
    id uuid NOT NULL,
    status smallint DEFAULT 0 NOT NULL,
    event_count integer NOT NULL,
    rows jsonb NOT NULL,
    payloads jsonb NOT NULL,
    retry_count integer DEFAULT 0 NOT NULL,
    next_retry_at timestamptz,
    created_at timestamptz DEFAULT now() NOT NULL,
    completed_at timestamptz,
    CONSTRAINT billing_ch_batches_check CHECK (((status = 1) OR ((jsonb_array_length(rows) = event_count) AND (jsonb_array_length(payloads) = event_count)))),
    CONSTRAINT billing_ch_batches_event_count_check CHECK (((event_count >= 0) AND (event_count <= 500))),
    CONSTRAINT billing_ch_batches_payloads_check CHECK ((jsonb_typeof(payloads) = 'array'::text)),
    CONSTRAINT billing_ch_batches_rows_check CHECK ((jsonb_typeof(rows) = 'array'::text)),
    CONSTRAINT billing_ch_batches_status_check CHECK ((status = ANY (ARRAY[0, 1, 2]))),
    CONSTRAINT billing_ch_batches_pkey PRIMARY KEY (id)
);
CREATE INDEX idx_ch_batches_pending ON billing_ch_batches (next_retry_at, created_at) WHERE (status = 0);

CREATE TABLE billing_ch_events (
    event_key text NOT NULL,
    batch_id uuid NOT NULL,
    CONSTRAINT billing_ch_events_pkey PRIMARY KEY (event_key)
);
CREATE INDEX idx_ch_events_batch ON billing_ch_events (batch_id);

CREATE TABLE billing_dlq (
    id bigint GENERATED ALWAYS AS IDENTITY,
    source varchar(32) NOT NULL,
    payload jsonb NOT NULL,
    error text,
    retry_count integer DEFAULT 0 NOT NULL,
    status smallint DEFAULT 0 NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    resolved_at timestamptz,
    resolved_by bigint,
    ch_batch_id uuid,
    event_key text,
    CONSTRAINT billing_dlq_pkey PRIMARY KEY (id)
);
CREATE UNIQUE INDEX idx_dlq_delivery_event ON billing_dlq (event_key) WHERE (event_key IS NOT NULL);

CREATE INDEX idx_br_channel_time ON billing_records (channel_id, created_at DESC);
CREATE INDEX idx_br_request ON billing_records (request_id);
CREATE INDEX idx_br_user_time ON billing_records (user_id, created_at DESC);

CREATE INDEX idx_be_user_time ON billing_events (user_id, created_at DESC);

-- ════════ 图片与视频任务 ════════

CREATE TABLE image_tasks (
    id uuid NOT NULL,
    user_id bigint NOT NULL,
    api_key_id bigint NOT NULL,
    kind text NOT NULL,
    model_name text NOT NULL,
    request_hash text NOT NULL,
    idempotency_hash text,
    payload bytea,
    client_ip text,
    client_type text NOT NULL,
    status text DEFAULT 'queued'::text NOT NULL,
    lease_id uuid,
    reservation_id uuid,
    lease_until timestamptz,
    attempts integer DEFAULT 0 NOT NULL,
    cancel_requested boolean DEFAULT false NOT NULL,
    channel_id bigint,
    channel_key_id bigint,
    result jsonb,
    error jsonb,
    http_status integer,
    billing_pending boolean DEFAULT false NOT NULL,
    storage_budget bigint NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    completed_at timestamptz,
    expires_at timestamptz DEFAULT (now() + '24:00:00'::interval) NOT NULL,
    CONSTRAINT image_tasks_attempts_check CHECK ((attempts >= 0)),
    CONSTRAINT image_tasks_check CHECK (((lease_id IS NULL) = (lease_until IS NULL))),
    CONSTRAINT image_tasks_idempotency_hash_check CHECK ((length(idempotency_hash) = 64)),
    CONSTRAINT image_tasks_kind_check CHECK ((kind = ANY (ARRAY['generation'::text, 'edit'::text]))),
    CONSTRAINT image_tasks_payload_check CHECK (((payload IS NULL) OR (octet_length(payload) <= 50331648))),
    CONSTRAINT image_tasks_request_hash_check CHECK ((length(request_hash) = 64)),
    CONSTRAINT image_tasks_result_check CHECK (((result IS NULL) OR (octet_length((result)::text) <= 1048576))),
    CONSTRAINT image_tasks_status_check CHECK ((status = ANY (ARRAY['queued'::text, 'preparing'::text, 'processing'::text, 'completed'::text, 'failed'::text, 'cancelled'::text]))),
    CONSTRAINT image_tasks_storage_budget_check CHECK ((storage_budget >= 0)),
    CONSTRAINT image_tasks_pkey PRIMARY KEY (id),
    CONSTRAINT image_tasks_user_id_api_key_id_idempotency_hash_key UNIQUE (user_id, api_key_id, idempotency_hash)
);
CREATE INDEX image_tasks_billing ON image_tasks (updated_at) WHERE billing_pending;
CREATE INDEX image_tasks_expiry ON image_tasks (expires_at);
CREATE INDEX image_tasks_owner ON image_tasks (user_id, api_key_id, created_at DESC);
CREATE INDEX image_tasks_queue ON image_tasks (created_at, id) WHERE (status = 'queued'::text);
CREATE INDEX image_tasks_recovery ON image_tasks (lease_until) WHERE (status = ANY (ARRAY['preparing'::text, 'processing'::text]));

CREATE TABLE image_task_attempts (
    reservation_id uuid NOT NULL,
    task_id uuid NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT image_task_attempts_pkey PRIMARY KEY (reservation_id)
);
CREATE INDEX image_task_attempts_task ON image_task_attempts (task_id);

CREATE TABLE image_task_artifacts (
    task_id uuid NOT NULL,
    image_index integer NOT NULL,
    content bytea,
    content_type text NOT NULL,
    object_id uuid,
    CONSTRAINT image_artifact_source CHECK (((content IS NOT NULL) OR (object_id IS NOT NULL))),
    CONSTRAINT image_task_artifacts_content_check CHECK ((octet_length(content) <= 67108864)),
    CONSTRAINT image_task_artifacts_image_index_check CHECK (((image_index >= 0) AND (image_index <= 9))),
    CONSTRAINT image_task_artifacts_pkey PRIMARY KEY (task_id, image_index)
);

CREATE TABLE image_task_objects (
    id uuid NOT NULL,
    task_id uuid NOT NULL,
    image_index integer NOT NULL,
    reference jsonb NOT NULL,
    content_sha256 text NOT NULL,
    content_bytes bigint NOT NULL,
    state text NOT NULL,
    lease_id uuid,
    lease_until timestamptz,
    retry_at timestamptz DEFAULT now() NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    last_error text,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT image_task_objects_check CHECK (((lease_id IS NULL) = (lease_until IS NULL))),
    CONSTRAINT image_task_objects_content_bytes_check CHECK (((content_bytes >= 1) AND (content_bytes <= 67108864))),
    CONSTRAINT image_task_objects_content_sha256_check CHECK ((length(content_sha256) = 64)),
    CONSTRAINT image_task_objects_image_index_check CHECK (((image_index >= 0) AND (image_index <= 9))),
    CONSTRAINT image_task_objects_reference_check CHECK ((octet_length((reference)::text) <= 8192)),
    CONSTRAINT image_task_objects_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'ready'::text, 'deleting'::text]))),
    CONSTRAINT image_task_objects_pkey PRIMARY KEY (id),
    CONSTRAINT image_task_objects_task_id_image_index_key UNIQUE (task_id, image_index)
);
CREATE INDEX image_task_objects_work ON image_task_objects (retry_at, lease_until);

CREATE TABLE image_batches (
    id uuid NOT NULL,
    user_id bigint NOT NULL,
    api_key_id bigint NOT NULL,
    request_hash text NOT NULL,
    idempotency_hash text,
    task_name varchar(256) NOT NULL,
    parent_id uuid,
    model_name varchar(128) NOT NULL,
    group_code varchar(32) NOT NULL,
    provider text NOT NULL,
    channel_id bigint NOT NULL,
    channel_key_id bigint NOT NULL,
    upstream_model varchar(256) NOT NULL,
    pricing_snapshot jsonb NOT NULL,
    unit_quote jsonb NOT NULL,
    maximum_micro bigint NOT NULL,
    actual_micro bigint,
    item_count integer NOT NULL,
    output_count integer NOT NULL,
    success_count integer DEFAULT 0 NOT NULL,
    failure_count integer DEFAULT 0 NOT NULL,
    state text DEFAULT 'funding'::text NOT NULL,
    cancel_requested boolean DEFAULT false NOT NULL,
    delete_requested boolean DEFAULT false NOT NULL,
    cleanup_done boolean DEFAULT false NOT NULL,
    lease_id uuid,
    lease_until timestamptz,
    next_run_at timestamptz DEFAULT now() NOT NULL,
    submit_intent uuid,
    provider_job_name text,
    remote_state text,
    input_ref jsonb DEFAULT '{}'::jsonb NOT NULL,
    output_ref jsonb DEFAULT '{}'::jsonb NOT NULL,
    error_code varchar(128),
    client_ip text,
    client_type varchar(128) NOT NULL,
    storage_budget bigint NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    completed_at timestamptz,
    expires_at timestamptz,
    downloaded_at timestamptz,
    member_user_id bigint,
    results_ready_at timestamptz,
    CONSTRAINT image_batches_check CHECK (((actual_micro >= 0) AND (actual_micro <= maximum_micro))),
    CONSTRAINT image_batches_check1 CHECK (((output_count >= item_count) AND (output_count <= 200))),
    CONSTRAINT image_batches_check10 CHECK (((success_count + failure_count) <= output_count)),
    CONSTRAINT image_batches_check11 CHECK (((octet_length((input_ref)::text) <= 65536) AND (octet_length((output_ref)::text) <= 65536))),
    CONSTRAINT image_batches_check2 CHECK (((success_count >= 0) AND (success_count <= output_count))),
    CONSTRAINT image_batches_check3 CHECK (((failure_count >= 0) AND (failure_count <= output_count))),
    CONSTRAINT image_batches_check4 CHECK (((lease_id IS NULL) = (lease_until IS NULL))),
    CONSTRAINT image_batches_check5 CHECK (((provider_job_name IS NULL) = (remote_state IS NULL))),
    CONSTRAINT image_batches_check6 CHECK (((provider_job_name IS NULL) OR (submit_intent IS NOT NULL))),
    CONSTRAINT image_batches_check7 CHECK (((state = ANY (ARRAY['completed'::text, 'partial'::text, 'failed'::text, 'cancelled'::text])) = (completed_at IS NOT NULL))),
    CONSTRAINT image_batches_check8 CHECK (((completed_at IS NULL) = (actual_micro IS NULL))),
    CONSTRAINT image_batches_check9 CHECK (((completed_at IS NULL) = (expires_at IS NULL))),
    CONSTRAINT image_batches_idempotency_hash_check CHECK ((length(idempotency_hash) = 64)),
    CONSTRAINT image_batches_item_count_check CHECK (((item_count >= 1) AND (item_count <= 200))),
    CONSTRAINT image_batches_maximum_micro_check CHECK (((maximum_micro >= 0) AND (maximum_micro <= '9007199254740991'::bigint))),
    CONSTRAINT image_batches_pricing_snapshot_check CHECK (((jsonb_typeof(pricing_snapshot) = 'object'::text) AND (octet_length((pricing_snapshot)::text) <= 65536))),
    CONSTRAINT image_batches_provider_check CHECK ((provider = ANY (ARRAY['gemini'::text, 'vertex'::text]))),
    CONSTRAINT image_batches_provider_job_name_check CHECK ((length(provider_job_name) <= 1024)),
    CONSTRAINT image_batches_remote_state_check CHECK ((remote_state = ANY (ARRAY['pending'::text, 'running'::text, 'cancelling'::text, 'paused'::text, 'succeeded'::text, 'partially_succeeded'::text, 'failed'::text, 'cancelled'::text, 'expired'::text]))),
    CONSTRAINT image_batches_request_hash_check CHECK ((length(request_hash) = 64)),
    CONSTRAINT image_batches_state_check CHECK ((state = ANY (ARRAY['funding'::text, 'preparing'::text, 'submitting'::text, 'running'::text, 'collecting'::text, 'settling'::text, 'uncertain'::text, 'completed'::text, 'partial'::text, 'failed'::text, 'cancelled'::text]))),
    CONSTRAINT image_batches_storage_budget_check CHECK ((storage_budget >= 0)),
    CONSTRAINT image_batches_unit_quote_check CHECK (((jsonb_typeof(unit_quote) = 'object'::text) AND (octet_length((unit_quote)::text) <= 4096))),
    CONSTRAINT image_batches_pkey PRIMARY KEY (id),
    CONSTRAINT image_batches_user_id_api_key_id_idempotency_hash_key UNIQUE (user_id, api_key_id, idempotency_hash)
);
CREATE INDEX image_batches_cleanup ON image_batches (expires_at, id) WHERE (completed_at IS NOT NULL);
CREATE INDEX image_batches_cleanup_due ON image_batches (next_run_at, expires_at, id) WHERE ((completed_at IS NOT NULL) AND (NOT cleanup_done));
CREATE INDEX image_batches_live_capacity_idx ON image_batches (user_id, api_key_id) INCLUDE (storage_budget, completed_at) WHERE (NOT cleanup_done);
CREATE INDEX image_batches_owner ON image_batches (user_id, api_key_id, created_at DESC, id DESC);
CREATE INDEX image_batches_work ON image_batches (next_run_at, created_at, id) WHERE (completed_at IS NULL);

CREATE TABLE image_batch_items (
    batch_id uuid NOT NULL,
    ordinal integer NOT NULL,
    custom_id varchar(128) NOT NULL,
    prompt_preview varchar(256) NOT NULL,
    output_count integer NOT NULL,
    CONSTRAINT image_batch_items_ordinal_check CHECK (((ordinal >= 0) AND (ordinal <= 199))),
    CONSTRAINT image_batch_items_output_count_check CHECK (((output_count >= 1) AND (output_count <= 4))),
    CONSTRAINT image_batch_items_batch_id_custom_id_key UNIQUE (batch_id, custom_id),
    CONSTRAINT image_batch_items_pkey PRIMARY KEY (batch_id, ordinal)
);

CREATE TABLE image_batch_outputs (
    batch_id uuid NOT NULL,
    slot integer NOT NULL,
    item_ordinal integer NOT NULL,
    image_index integer NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    content bytea,
    content_type text,
    content_hash text,
    usage jsonb DEFAULT '{}'::jsonb NOT NULL,
    error_code varchar(128),
    CONSTRAINT image_batch_outputs_check CHECK (((state = 'succeeded'::text) = (content IS NOT NULL))),
    CONSTRAINT image_batch_outputs_check1 CHECK (((state = 'succeeded'::text) = (content_type IS NOT NULL))),
    CONSTRAINT image_batch_outputs_check2 CHECK (((state = 'succeeded'::text) = (content_hash IS NOT NULL))),
    CONSTRAINT image_batch_outputs_check3 CHECK (((state = 'failed'::text) = (error_code IS NOT NULL))),
    CONSTRAINT image_batch_outputs_content_check CHECK (((octet_length(content) >= 1) AND (octet_length(content) <= 16777216))),
    CONSTRAINT image_batch_outputs_content_hash_check CHECK ((length(content_hash) = 64)),
    CONSTRAINT image_batch_outputs_content_type_check CHECK ((content_type = ANY (ARRAY['image/png'::text, 'image/jpeg'::text, 'image/webp'::text]))),
    CONSTRAINT image_batch_outputs_image_index_check CHECK (((image_index >= 0) AND (image_index <= 3))),
    CONSTRAINT image_batch_outputs_slot_check CHECK (((slot >= 0) AND (slot <= 199))),
    CONSTRAINT image_batch_outputs_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'succeeded'::text, 'failed'::text]))),
    CONSTRAINT image_batch_outputs_usage_check CHECK (((jsonb_typeof(usage) = 'object'::text) AND (octet_length((usage)::text) <= 8192))),
    CONSTRAINT image_batch_outputs_batch_id_item_ordinal_image_index_key UNIQUE (batch_id, item_ordinal, image_index),
    CONSTRAINT image_batch_outputs_pkey PRIMARY KEY (batch_id, slot)
);

CREATE TABLE image_batch_payloads (
    batch_id uuid NOT NULL,
    input bytea NOT NULL,
    binding bytea NOT NULL,
    upload_session bytea,
    CONSTRAINT image_batch_payloads_binding_check CHECK (((octet_length(binding) >= 1) AND (octet_length(binding) <= 262144))),
    CONSTRAINT image_batch_payloads_input_check CHECK (((octet_length(input) >= 1) AND (octet_length(input) <= 134217728))),
    CONSTRAINT image_batch_payloads_upload_session_check CHECK ((octet_length(upload_session) <= 262144)),
    CONSTRAINT image_batch_payloads_pkey PRIMARY KEY (batch_id)
);

CREATE TABLE image_batch_recovery (
    batch_id uuid NOT NULL,
    next_page text,
    candidate_name text,
    pages integer DEFAULT 0 NOT NULL,
    cursor_hashes jsonb DEFAULT '[]'::jsonb NOT NULL,
    complete boolean DEFAULT false NOT NULL,
    conflict boolean DEFAULT false NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT image_batch_recovery_candidate_name_check CHECK (((length(candidate_name) >= 1) AND (length(candidate_name) <= 1024))),
    CONSTRAINT image_batch_recovery_check CHECK (((NOT complete) OR (next_page IS NULL))),
    CONSTRAINT image_batch_recovery_cursor_hashes_check CHECK (((jsonb_typeof(cursor_hashes) = 'array'::text) AND (jsonb_array_length(cursor_hashes) <= 1024) AND (octet_length((cursor_hashes)::text) <= 73728))),
    CONSTRAINT image_batch_recovery_next_page_check CHECK (((octet_length(next_page) >= 1) AND (octet_length(next_page) <= 4096))),
    CONSTRAINT image_batch_recovery_pages_check CHECK (((pages >= 0) AND (pages <= 1024))),
    CONSTRAINT image_batch_recovery_pkey PRIMARY KEY (batch_id)
);

CREATE TABLE image_batch_cleanup (
    batch_id uuid NOT NULL,
    job_removed boolean DEFAULT false NOT NULL,
    operation text,
    last_error varchar(128),
    CONSTRAINT image_batch_cleanup_check CHECK (((NOT job_removed) OR (operation IS NULL))),
    CONSTRAINT image_batch_cleanup_operation_check CHECK (((length(operation) >= 1) AND (length(operation) <= 1024))),
    CONSTRAINT image_batch_cleanup_pkey PRIMARY KEY (batch_id)
);

CREATE TABLE image_batch_downloads (
    id uuid NOT NULL,
    batch_id uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    CONSTRAINT image_batch_downloads_pkey PRIMARY KEY (id)
);
CREATE INDEX image_batch_downloads_batch ON image_batch_downloads (batch_id, expires_at);

CREATE TABLE image_batch_statistics (
    batch_id uuid NOT NULL,
    user_id bigint NOT NULL,
    member_user_id bigint,
    channel_key_id bigint NOT NULL,
    recorded_at timestamptz NOT NULL,
    tokens bigint NOT NULL,
    amount_micro bigint NOT NULL,
    is_error boolean NOT NULL,
    delivered_at timestamptz,
    next_attempt_at timestamptz DEFAULT now() NOT NULL,
    lease_id uuid,
    lease_until timestamptz,
    CONSTRAINT image_batch_statistics_amount_micro_check CHECK (((amount_micro >= 0) AND (amount_micro <= '9007199254740991'::bigint))),
    CONSTRAINT image_batch_statistics_check CHECK (((lease_id IS NULL) = (lease_until IS NULL))),
    CONSTRAINT image_batch_statistics_tokens_check CHECK ((tokens >= 0)),
    CONSTRAINT image_batch_statistics_pkey PRIMARY KEY (batch_id)
);
CREATE INDEX image_batch_statistics_pending ON image_batch_statistics (next_attempt_at, batch_id) WHERE (delivered_at IS NULL);

CREATE TABLE video_tasks (
    user_id bigint NOT NULL,
    task_id text NOT NULL,
    request_id uuid NOT NULL,
    channel_key_id bigint NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    next_poll_at timestamptz DEFAULT now() NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT video_tasks_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'refund_pending'::text, 'completed'::text, 'refunded'::text]))),
    CONSTRAINT video_tasks_pkey PRIMARY KEY (user_id, task_id),
    CONSTRAINT video_tasks_request_id_key UNIQUE (request_id)
);
CREATE INDEX idx_video_tasks_poll ON video_tasks (next_poll_at) WHERE (state = ANY (ARRAY['pending'::text, 'refund_pending'::text]));

-- ════════ 视图与函数 ════════

CREATE VIEW billing_actor_totals AS
 SELECT user_id,
    actor,
    sum(delta_micro) AS delta_micro
   FROM ( SELECT billing_events.user_id,
            billing_events.actor,
            (billing_events.delta_micro)::numeric AS delta_micro
           FROM billing_events
        UNION ALL
         SELECT billing_event_carry.user_id,
            billing_event_carry.actor,
            billing_event_carry.delta_micro
           FROM billing_event_carry) facts
  GROUP BY user_id, actor;

CREATE VIEW billing_balance_totals AS
 SELECT user_id,
    pool,
    (sum(delta_micro))::bigint AS delta_micro
   FROM ( SELECT billing_events.user_id,
            billing_events.pool,
            (billing_events.delta_micro)::numeric AS delta_micro
           FROM billing_events
        UNION ALL
         SELECT billing_event_carry.user_id,
            billing_event_carry.pool,
            billing_event_carry.delta_micro
           FROM billing_event_carry) facts
  GROUP BY user_id, pool;

CREATE VIEW billing_financial_records AS
 SELECT billing_records.request_id,
    billing_records.user_id,
    billing_records.api_key_id,
    billing_records.status,
    billing_records.amount_micro,
    billing_records.pool,
    billing_records.created_at,
    billing_records.source_window
   FROM billing_records
UNION ALL
 SELECT billing_record_receipts.request_id,
    billing_record_receipts.user_id,
    billing_record_receipts.api_key_id,
    billing_record_receipts.status,
    billing_record_receipts.amount_micro,
    billing_record_receipts.pool,
    billing_record_receipts.created_at,
    billing_record_receipts.source_window
   FROM billing_record_receipts;

CREATE VIEW channel_egress AS
 SELECT c.id AS channel_id,
    (c.egress_mode IS NULL) AS inherited,
        CASE
            WHEN (c.egress_mode IS NOT NULL) THEN c.egress_mode
            WHEN ((d.value ->> 'mode'::text) = ANY (ARRAY['proxy'::text, 'group'::text])) THEN ((d.value ->> 'mode'::text))::character varying
            ELSE 'direct'::character varying
        END AS mode,
        CASE
            WHEN (c.egress_mode IS NOT NULL) THEN c.egress_proxy_id
            WHEN (((d.value ->> 'mode'::text) = 'proxy'::text) AND ((d.value ->> 'proxy_id'::text) ~ '^[0-9]{1,18}$'::text)) THEN ((d.value ->> 'proxy_id'::text))::bigint
            ELSE NULL::bigint
        END AS proxy_id,
        CASE
            WHEN (c.egress_mode IS NOT NULL) THEN c.egress_group_code
            WHEN ((d.value ->> 'mode'::text) = 'group'::text) THEN ((d.value ->> 'group_code'::text))::character varying
            ELSE NULL::character varying
        END AS group_code
   FROM (channels c
     LEFT JOIN settings d ON (((d.key)::text = 'egress_default'::text)));

CREATE FUNCTION channel_receipt_tokens(details jsonb) RETURNS bigint
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $_$
    SELECT
        CASE WHEN details #>> '{tokens,prompt_tokens}' ~ '^[0-9]{1,10}$'
             THEN (details #>> '{tokens,prompt_tokens}')::bigint ELSE 0 END
      + CASE WHEN details #>> '{tokens,completion_tokens}' ~ '^[0-9]{1,10}$'
             THEN (details #>> '{tokens,completion_tokens}')::bigint ELSE 0 END
$_$;

CREATE FUNCTION egress_pick(p_channel bigint, p_key bigint, healthy_only boolean) RETURNS TABLE(mode text, proxy_id bigint, url_ciphertext bytea, max_concurrency integer)
    LANGUAGE sql
    AS $$
    SELECT e.mode, px.id, px.url_ciphertext, px.max_concurrency
    FROM channel_egress e
    LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
    LEFT JOIN channel_keys ck ON ck.id = p_key AND ck.channel_id = e.channel_id
    LEFT JOIN LATERAL (
        SELECT p.id, p.url_ciphertext, p.max_concurrency
        FROM proxies p
        LEFT JOIN proxy_group_members m ON m.group_code = g.code AND m.proxy_id = p.id
        WHERE p.status = 1
          AND (NOT healthy_only OR p.cooldown_until IS NULL OR p.cooldown_until <= now())
          AND ((e.mode = 'proxy' AND p.id = e.proxy_id)
            OR (g.mode = 'pinned' AND p.id = ck.egress_proxy_id AND m.proxy_id IS NOT NULL)
            OR (g.mode = 'rotate' AND m.proxy_id IS NOT NULL))
        ORDER BY (p.cooldown_until IS NOT NULL AND p.cooldown_until > now()),
                 m.priority DESC NULLS LAST,
                 -ln(1 - random()) / GREATEST(COALESCE(m.weight, 1), 1)
        LIMIT 1
    ) px ON true
    WHERE e.channel_id = p_channel
$$;

CREATE FUNCTION record_channel_tokens() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.channel_id IS NOT NULL AND NEW.log_type IN (2,5) THEN
        INSERT INTO channel_token_totals(channel_id,tokens)
        VALUES (NEW.channel_id,greatest(NEW.prompt_tokens,0)::bigint + greatest(NEW.completion_tokens,0)::bigint)
        ON CONFLICT(channel_id) DO UPDATE SET tokens=channel_token_totals.tokens+EXCLUDED.tokens;
    END IF;
    RETURN NEW;
END
$$;

CREATE FUNCTION subscription_plan_snapshot(p plans) RETURNS jsonb
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE WHEN p.kind=1 THEN jsonb_build_object(
        'id',p.id,'plan_code',p.plan_code,'display_name',p.display_name,
        'quota_micro',p.grant_micro,'group_code',p.group_code,'price_micro',p.price_micro,
        'period',p.period,'duration_days',p.duration_days,'sort_order',p.sort_order,
        'description',p.description) END
$$;

-- ════════ 触发器 ════════

CREATE TRIGGER billing_channel_tokens AFTER INSERT ON billing_records FOR EACH ROW EXECUTE FUNCTION record_channel_tokens();

-- ════════ 外键 ════════

ALTER TABLE api_keys ADD CONSTRAINT api_keys_group_override_fkey FOREIGN KEY (group_override) REFERENCES price_groups(group_code);
ALTER TABLE api_keys ADD CONSTRAINT api_keys_member_user_id_fkey FOREIGN KEY (member_user_id) REFERENCES users(id);
ALTER TABLE api_keys ADD CONSTRAINT api_keys_pool_override_fkey FOREIGN KEY (pool_override) REFERENCES channel_pools(pool_code);
ALTER TABLE api_keys ADD CONSTRAINT api_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE balance_holds ADD CONSTRAINT balance_holds_api_key_id_fkey FOREIGN KEY (api_key_id) REFERENCES api_keys(id);
ALTER TABLE balance_holds ADD CONSTRAINT balance_holds_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE billing_ch_events ADD CONSTRAINT billing_ch_events_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES billing_ch_batches(id);
ALTER TABLE billing_dlq ADD CONSTRAINT billing_dlq_ch_batch_id_fkey FOREIGN KEY (ch_batch_id) REFERENCES billing_ch_batches(id);
ALTER TABLE billing_outbox ADD CONSTRAINT billing_outbox_ch_batch_id_fkey FOREIGN KEY (ch_batch_id) REFERENCES billing_ch_batches(id);
ALTER TABLE billing_sync ADD CONSTRAINT billing_sync_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE channel_keys ADD CONSTRAINT channel_keys_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE;
ALTER TABLE channel_keys ADD CONSTRAINT channel_keys_egress_proxy_id_fkey FOREIGN KEY (egress_proxy_id) REFERENCES proxies(id) ON DELETE SET NULL;
ALTER TABLE channel_pools ADD CONSTRAINT channel_pools_fallback_pool_code_fkey FOREIGN KEY (fallback_pool_code) REFERENCES channel_pools(pool_code);
ALTER TABLE channel_usage_windows ADD CONSTRAINT channel_usage_windows_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE;
ALTER TABLE channels ADD CONSTRAINT channels_egress_group_code_fkey FOREIGN KEY (egress_group_code) REFERENCES proxy_groups(code);
ALTER TABLE channels ADD CONSTRAINT channels_egress_proxy_id_fkey FOREIGN KEY (egress_proxy_id) REFERENCES proxies(id);
ALTER TABLE channels ADD CONSTRAINT channels_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES users(id);
ALTER TABLE fund_transfers ADD CONSTRAINT fund_transfers_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE image_batch_cleanup ADD CONSTRAINT image_batch_cleanup_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES image_batches(id) ON DELETE CASCADE;
ALTER TABLE image_batch_downloads ADD CONSTRAINT image_batch_downloads_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES image_batches(id) ON DELETE CASCADE;
ALTER TABLE image_batch_items ADD CONSTRAINT image_batch_items_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES image_batches(id) ON DELETE CASCADE;
ALTER TABLE image_batch_outputs ADD CONSTRAINT image_batch_outputs_batch_id_item_ordinal_fkey FOREIGN KEY (batch_id, item_ordinal) REFERENCES image_batch_items(batch_id, ordinal) ON DELETE CASCADE;
ALTER TABLE image_batch_payloads ADD CONSTRAINT image_batch_payloads_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES image_batches(id) ON DELETE CASCADE;
ALTER TABLE image_batch_recovery ADD CONSTRAINT image_batch_recovery_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES image_batches(id) ON DELETE CASCADE;
ALTER TABLE image_batch_statistics ADD CONSTRAINT image_batch_statistics_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES image_batches(id);
ALTER TABLE image_batches ADD CONSTRAINT image_batches_api_key_id_fkey FOREIGN KEY (api_key_id) REFERENCES api_keys(id);
ALTER TABLE image_batches ADD CONSTRAINT image_batches_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES channels(id);
ALTER TABLE image_batches ADD CONSTRAINT image_batches_channel_key_id_fkey FOREIGN KEY (channel_key_id) REFERENCES channel_keys(id);
ALTER TABLE image_batches ADD CONSTRAINT image_batches_parent_id_fkey FOREIGN KEY (parent_id) REFERENCES image_batches(id);
ALTER TABLE image_batches ADD CONSTRAINT image_batches_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE image_task_artifacts ADD CONSTRAINT image_task_artifacts_object_id_fkey FOREIGN KEY (object_id) REFERENCES image_task_objects(id);
ALTER TABLE image_task_artifacts ADD CONSTRAINT image_task_artifacts_task_id_fkey FOREIGN KEY (task_id) REFERENCES image_tasks(id) ON DELETE CASCADE;
ALTER TABLE image_task_attempts ADD CONSTRAINT image_task_attempts_task_id_fkey FOREIGN KEY (task_id) REFERENCES image_tasks(id) ON DELETE CASCADE;
ALTER TABLE image_task_objects ADD CONSTRAINT image_task_objects_task_id_fkey FOREIGN KEY (task_id) REFERENCES image_tasks(id);
ALTER TABLE image_tasks ADD CONSTRAINT image_tasks_api_key_id_fkey FOREIGN KEY (api_key_id) REFERENCES api_keys(id);
ALTER TABLE image_tasks ADD CONSTRAINT image_tasks_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE model_aliases ADD CONSTRAINT model_aliases_target_model_fkey FOREIGN KEY (target_model) REFERENCES models(model_name);
ALTER TABLE model_pricing ADD CONSTRAINT model_pricing_model_id_fkey FOREIGN KEY (model_id) REFERENCES models(id) ON DELETE CASCADE;
ALTER TABLE model_pricing ADD CONSTRAINT model_pricing_updated_by_fkey FOREIGN KEY (updated_by) REFERENCES users(id);
ALTER TABLE oauth_identities ADD CONSTRAINT oauth_identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE payment_receipts ADD CONSTRAINT payment_receipts_order_id_fkey FOREIGN KEY (order_id) REFERENCES recharge_orders(id);
ALTER TABLE pool_channels ADD CONSTRAINT pool_channels_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE;
ALTER TABLE pool_channels ADD CONSTRAINT pool_channels_pool_code_fkey FOREIGN KEY (pool_code) REFERENCES channel_pools(pool_code) ON DELETE CASCADE;
ALTER TABLE price_groups ADD CONSTRAINT price_groups_pool_code_fkey FOREIGN KEY (pool_code) REFERENCES channel_pools(pool_code);
ALTER TABLE pricing_epochs ADD CONSTRAINT pricing_epochs_published_by_fkey FOREIGN KEY (published_by) REFERENCES users(id);
ALTER TABLE proxies ADD CONSTRAINT proxies_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES users(id);
ALTER TABLE proxy_group_members ADD CONSTRAINT proxy_group_members_group_code_fkey FOREIGN KEY (group_code) REFERENCES proxy_groups(code) ON DELETE CASCADE;
ALTER TABLE proxy_group_members ADD CONSTRAINT proxy_group_members_proxy_id_fkey FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE;
ALTER TABLE proxy_groups ADD CONSTRAINT proxy_groups_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES users(id);
ALTER TABLE recharge_orders ADD CONSTRAINT recharge_orders_plan_id_fkey FOREIGN KEY (plan_id) REFERENCES plans(id);
ALTER TABLE recharge_orders ADD CONSTRAINT recharge_orders_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE redemption_codes ADD CONSTRAINT redemption_codes_bind_user_id_fkey FOREIGN KEY (bind_user_id) REFERENCES users(id);
ALTER TABLE redemption_codes ADD CONSTRAINT redemption_codes_created_by_fkey FOREIGN KEY (created_by) REFERENCES users(id);
ALTER TABLE redemption_codes ADD CONSTRAINT redemption_codes_plan_id_fkey FOREIGN KEY (plan_id) REFERENCES plans(id);
ALTER TABLE redemption_codes ADD CONSTRAINT redemption_codes_redeemed_by_fkey FOREIGN KEY (redeemed_by) REFERENCES users(id);
ALTER TABLE subscription_grants ADD CONSTRAINT subscription_grants_subscription_id_fkey FOREIGN KEY (subscription_id) REFERENCES user_subscriptions(id);
ALTER TABLE subscription_grants ADD CONSTRAINT subscription_grants_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE subscription_sync ADD CONSTRAINT subscription_sync_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE team_members ADD CONSTRAINT team_members_member_user_id_fkey FOREIGN KEY (member_user_id) REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE team_members ADD CONSTRAINT team_members_team_user_id_fkey FOREIGN KEY (team_user_id) REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE user_groups ADD CONSTRAINT user_groups_group_code_fkey FOREIGN KEY (group_code) REFERENCES price_groups(group_code);
ALTER TABLE user_groups ADD CONSTRAINT user_groups_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE user_pricing ADD CONSTRAINT user_pricing_model_id_fkey FOREIGN KEY (model_id) REFERENCES models(id) ON DELETE CASCADE;
ALTER TABLE user_pricing ADD CONSTRAINT user_pricing_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE user_subscriptions ADD CONSTRAINT user_subscriptions_plan_id_fkey FOREIGN KEY (plan_id) REFERENCES plans(id);
ALTER TABLE user_subscriptions ADD CONSTRAINT user_subscriptions_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE users ADD CONSTRAINT users_admin_role_id_fkey FOREIGN KEY (admin_role_id) REFERENCES admin_roles(id);
ALTER TABLE users ADD CONSTRAINT users_inviter_id_fkey FOREIGN KEY (inviter_id) REFERENCES users(id);
ALTER TABLE video_tasks ADD CONSTRAINT video_tasks_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id);

-- ════════ 种子数据 ════════

-- 内置默认池：新渠道缺省加入，未指定池的分组走这里
INSERT INTO channel_pools (pool_code, description, routing_strategy)
VALUES ('default', '内置默认池：新渠道缺省加入，未指定池的分组走这里', 'priority_weighted');

-- 默认价格分组（倍率 1，绑定默认池）
INSERT INTO price_groups (group_code, group_ratio, description, is_default, sort_order, pool_code, self_select)
VALUES ('default', 1.0000, '默认分组', true, 0, 'default', false);
