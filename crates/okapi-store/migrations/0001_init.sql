-- Okapi 初始 schema（唯一权威：docs/database.md；本文件为其落地）
--
-- 发布前把 0001–0016 压成单文件：项目尚无生产部署，16 个增量迁移里有一半是
-- "加一列"，还有一处（0015）把 0004 建的表整张替换掉——保留链条只会让新读者
-- 沿着已被推翻的中间态读一遍。压平后表结构与 docs/database.md 逐节对应。
-- 形状由 bins/okapi/tests/schema_shape.rs 守：临时库跑一遍本文件，核对
-- 关键列在、废弃表不在、明文列不在、池策略 CHECK 生效——压平最容易漏列或把
-- 已推翻的中间态搬回来，这两类错都能被它挡住。
--
-- 此后恢复只前滚的增量迁移。分区表先建 DEFAULT 分区，月度分区滚动由 worker 接管。

-- ============ 身份与权限 ============

CREATE TABLE admin_roles (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    role_code     VARCHAR(64) NOT NULL UNIQUE,
    display_name  VARCHAR(128) NOT NULL,
    permissions   JSONB NOT NULL DEFAULT '[]',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email              VARCHAR(255) UNIQUE,
    username           VARCHAR(64) NOT NULL UNIQUE,
    password_hash      VARCHAR(255),                 -- argon2id；bcrypt 前缀兼容老站迁移用户
    role               SMALLINT NOT NULL DEFAULT 1,  -- 1=user 10=admin 100=super_admin（对齐 new-api）
    admin_role_id      BIGINT REFERENCES admin_roles(id),
    status             SMALLINT NOT NULL DEFAULT 1,  -- 1=active 2=disabled
    kind               VARCHAR(8) NOT NULL DEFAULT 'user',  -- user | team（team 无登录凭证，钱包机制复用）
    price_multiplier   NUMERIC(8,4) NOT NULL DEFAULT 1,
    balance_micro      BIGINT NOT NULL DEFAULT 0,    -- 快照列；真理源 = billing_events 重放
    balance_expires_at TIMESTAMPTZ,                  -- NULL = 永不过期；到期由 worker 清零并重置为 NULL
    language           VARCHAR(8) NOT NULL DEFAULT 'auto',
    totp_secret_ciphertext BYTEA,
    aff_code           VARCHAR(16) UNIQUE,           -- 惰性生成（门户首查时）
    inviter_id         BIGINT REFERENCES users(id),  -- 注册时绑定，终身不变
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at         TIMESTAMPTZ
);
CREATE UNIQUE INDEX idx_users_aff_code ON users (aff_code) WHERE aff_code IS NOT NULL;
CREATE INDEX idx_users_inviter ON users (inviter_id) WHERE inviter_id IS NOT NULL;

-- 渠道池：一组渠道 + 在这组里怎么选。与 price_groups 正交——
-- 分组只管"付多少钱"，池只管"打哪些上游、怎么选"（docs/database.md §3.7 论证）。
CREATE TABLE channel_pools (
    pool_code        VARCHAR(32) PRIMARY KEY,
    description      VARCHAR(255),
    routing_strategy VARCHAR(24) NOT NULL DEFAULT 'priority_weighted',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT channel_pools_strategy_chk
        CHECK (routing_strategy IN ('priority_weighted', 'least_latency'))
);
COMMENT ON COLUMN channel_pools.routing_strategy IS
    'priority_weighted：priority 分层 + 层内成本修正加权随机（历史行为）；'
    'least_latency：层内按 Redis lat:ck:* 的 EWMA 升序，无数据者按中位数处理';

CREATE TABLE price_groups (
    group_code  VARCHAR(32) PRIMARY KEY,
    group_ratio NUMERIC(6,4) NOT NULL DEFAULT 1,
    description VARCHAR(255),
    is_default  BOOLEAN NOT NULL DEFAULT false,
    sort_order  INT NOT NULL DEFAULT 0,
    pool_code   VARCHAR(32) REFERENCES channel_pools(pool_code)
);
COMMENT ON COLUMN price_groups.pool_code IS '该分组走哪个渠道池；null = 只看未入池的渠道';

CREATE TABLE user_groups (
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_code VARCHAR(32) NOT NULL REFERENCES price_groups(group_code),
    priority   INT NOT NULL DEFAULT 0,               -- 定价取最高优先级组
    PRIMARY KEY (user_id, group_code)
);

CREATE TABLE team_members (
    team_user_id              BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    member_user_id            BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role                      VARCHAR(16) NOT NULL DEFAULT 'member',  -- owner | admin | member
    monthly_spend_limit_micro BIGINT,                                 -- null = 不限
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (team_user_id, member_user_id)
);
CREATE INDEX idx_team_members_member ON team_members(member_user_id);

CREATE TABLE api_keys (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id           BIGINT NOT NULL REFERENCES users(id),
    team_id           BIGINT,
    member_user_id    BIGINT REFERENCES users(id),   -- 团 key 的归属成员（分账与限额锚点；null = 非团 key）
    name              VARCHAR(128) NOT NULL DEFAULT '',
    key_hash          CHAR(64) NOT NULL UNIQUE,      -- SHA-256(hex)，明文不落库
    key_prefix        VARCHAR(16) NOT NULL,          -- sk-okapi-xxxx… 展示用
    status            SMALLINT NOT NULL DEFAULT 1,   -- 1=active 2=disabled 3=expired
    quota_mode        SMALLINT NOT NULL DEFAULT 0,   -- 0=共享钱包 1=独立限额
    quota_micro       BIGINT,
    used_micro        BIGINT NOT NULL DEFAULT 0,
    model_allowlist   JSONB,                         -- null = 不限
    group_override    VARCHAR(32) REFERENCES price_groups(group_code),
    pool_override     VARCHAR(32) REFERENCES channel_pools(pool_code),
    rpm_limit         INT,
    tpm_limit         INT,
    rpd_limit         INT,
    daily_token_limit BIGINT,
    max_concurrency   INT,
    ip_allowlist      JSONB,
    expires_at        TIMESTAMPTZ,
    last_used_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at        TIMESTAMPTZ
);
CREATE INDEX idx_api_keys_user ON api_keys(user_id) WHERE deleted_at IS NULL;
COMMENT ON COLUMN api_keys.pool_override IS '令牌钉住某渠道池，优先于分组的池；null = 跟随分组';

CREATE TABLE oauth_identities (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    provider    VARCHAR(32)  NOT NULL,               -- github / discord / linuxdo / oidc:<code>
    subject     VARCHAR(255) NOT NULL,               -- IdP 侧稳定主体 id
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    display     VARCHAR(255),                        -- IdP 侧展示名（审计用）
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);
CREATE INDEX idx_oauth_identities_user ON oauth_identities(user_id);

-- ============ 渠道与模型 ============

CREATE TABLE channels (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name                 VARCHAR(128) NOT NULL,
    provider             VARCHAR(32) NOT NULL,       -- openai / anthropic / gemini / openai_compat / custom_pass
    api_base             VARCHAR(255),
    status               SMALLINT NOT NULL DEFAULT 1,-- 1=启用 2=手动停用 3=自动停用
    priority             INT NOT NULL DEFAULT 0,     -- 高优先级层耗尽才降层
    weight               INT NOT NULL DEFAULT 1,
    models               JSONB NOT NULL DEFAULT '[]',-- 服务的对外模型名
    model_mapping        JSONB NOT NULL DEFAULT '{}',-- 对外名 → 上游名
    capabilities         JSONB NOT NULL DEFAULT '{}',-- 能力感知路由（显式 false 才排除）
    trust_upstream_usage BOOLEAN NOT NULL DEFAULT false,
    retry_policy         JSONB,
    settings             JSONB NOT NULL DEFAULT '{}',-- thinking_to_content / bill_by_response_model / strip_request_fields / pass_paths
    owner_id             BIGINT REFERENCES users(id),
    upstream_unit_cost   JSONB,                      -- relative_cost_milli 千分比 = 调度层内权重除数（缺省 1000）
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at           TIMESTAMPTZ
);

CREATE TABLE pool_channels (
    pool_code  VARCHAR(32) NOT NULL REFERENCES channel_pools(pool_code) ON DELETE CASCADE,
    channel_id BIGINT      NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    PRIMARY KEY (pool_code, channel_id)
);
CREATE INDEX idx_pool_channels_channel ON pool_channels(channel_id);

CREATE TABLE channel_keys (
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    channel_id            BIGINT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    credential_ciphertext BYTEA NOT NULL,            -- AES-256-GCM，主密钥来自环境变量
    credential_kind       SMALLINT NOT NULL DEFAULT 0,-- 0=static_key 1=oauth_refresh 2=cloud_sts
    status                SMALLINT NOT NULL DEFAULT 1,-- 1 active / 2 cooling / 3 rate_limited / 4 quota_exhausted / 5 banned / 6 invalid
    cooldown_until        TIMESTAMPTZ,
    failed_count          INT NOT NULL DEFAULT 0,
    last_error            VARCHAR(255),
    weight                INT NOT NULL DEFAULT 1,
    max_concurrency       INT,                       -- 在途计数在 Redis conc:ck:*
    -- key 级配额：同渠道下不同 key 的权限与限额常常不同（同组织两把 OpenAI key
    -- 可能一把有 gpt-4 权限一把没有；套餐不同则 RPM 不同）。只有渠道级限额时，
    -- 这类差异只能靠拆渠道表达，base_url 与 settings 被迫重复。
    model_subset          JSONB,
    rpm_limit             INT,
    daily_spend_cap_micro BIGINT,
    quota_snapshot        JSONB,                     -- 被动采集上游 rate-limit 响应头
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_channel_keys_channel ON channel_keys(channel_id, status);
COMMENT ON COLUMN channel_keys.model_subset IS
    'null = 继承 channels.models；非空 = 该 key 只服务这些模型';
COMMENT ON COLUMN channel_keys.rpm_limit IS
    'null = 不限。固定分钟窗计数在 Redis rpm:ck:*；超限把该 key 摘出候选而非拒绝请求';
COMMENT ON COLUMN channel_keys.daily_spend_cap_micro IS
    'null = 不限。当日累计消费在 Redis spend:ck:*，结算后累加、选路前比较（软实时）';

CREATE TABLE models (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    model_name     VARCHAR(128) NOT NULL UNIQUE,     -- canonical 对外名
    display_name   VARCHAR(128),
    vendor         VARCHAR(64),                      -- 按模型名前缀自动归类，可覆写
    capabilities   JSONB NOT NULL DEFAULT '{}',
    context_window INT,
    max_output     INT,
    status         SMALLINT NOT NULL DEFAULT 1,
    sort_order     INT NOT NULL DEFAULT 0,
    fallback_models JSONB NOT NULL DEFAULT '[]',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
COMMENT ON COLUMN models.fallback_models IS
    '零可用候选时的降级模型名数组，单跳不递归；不覆盖上游 4xx 与用户参数错误。计费按实际服务模型（DESIGN §3.4.1）';

CREATE TABLE model_aliases (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pattern      VARCHAR(128) NOT NULL UNIQUE,       -- 精确名或通配 "gpt-4o-*"
    target_model VARCHAR(128) NOT NULL REFERENCES models(model_name),
    priority     INT NOT NULL DEFAULT 0,             -- 精确 > 通配；同类按 priority 降序
    enabled      BOOLEAN NOT NULL DEFAULT true
);

-- ============ 定价域（公式见 DESIGN §3） ============

CREATE TABLE model_pricing (
    model_id             BIGINT PRIMARY KEY REFERENCES models(id) ON DELETE CASCADE,
    pricing_mode         VARCHAR(16) NOT NULL DEFAULT 'ratio',
    model_ratio          NUMERIC(12,6),
    completion_ratio     NUMERIC(12,6) NOT NULL DEFAULT 1,
    cache_ratio          NUMERIC(6,4)  NOT NULL DEFAULT 1,
    cache_write_ratio    NUMERIC(6,4)  NOT NULL DEFAULT 1,
    audio_ratio            NUMERIC(12,6) NOT NULL DEFAULT 1,
    audio_completion_ratio NUMERIC(12,6) NOT NULL DEFAULT 1,
    image_ratio            NUMERIC(12,6) NOT NULL DEFAULT 1,
    per_call_price_micro BIGINT,
    tier_expr            TEXT,
    tier_ratios          JSONB,
    media_prices         JSONB,
    effective_from       TIMESTAMPTZ,
    updated_by           BIGINT REFERENCES users(id),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- 各模态轴缺省 1.0 = 按文本计（保持纯文本模型零影响）。轴缺失会实打实算错钱：
-- 以 gpt-4o-audio-preview 官方价（text in $2.5/1M、audio in $40/1M、audio out $80/1M）
-- 为例，音频输入是文本的 16×，统一按文本计漏收约八成
-- （回归断言见 crates/okapi-pricing/tests/parity.rs::openai_audio_official_pricing_parity）。
COMMENT ON COLUMN model_pricing.cache_write_ratio IS
    '缓存写入倍率（Anthropic cache_creation；1.0=按常规输入计，官方 1.25×@5m / 2.0×@1h）';
COMMENT ON COLUMN model_pricing.audio_ratio IS
    '音频输入倍率（相对文本；gpt-4o-audio 官方 16.0，缺省 1.0=按文本计）';
COMMENT ON COLUMN model_pricing.audio_completion_ratio IS
    '音频输出倍率，叠乘在 audio_ratio 之上（官方 2.0 → 输出 = 文本×16×2）';
COMMENT ON COLUMN model_pricing.image_ratio IS
    '图片输入倍率（相对文本，缺省 1.0）';
COMMENT ON COLUMN model_pricing.tier_ratios IS
    'service_tier 档位倍率，如 {"flex":"0.5","priority":"2.0"}；NULL = 全档 1.0。'
    '结算档取请求声明档与上游响应档中倍率较低者（只降不升）';

CREATE TABLE user_pricing (
    id                         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id                    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    model_id                   BIGINT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    override_kind              VARCHAR(8) NOT NULL,  -- ratio | absolute
    custom_model_ratio         NUMERIC(12,6),
    custom_completion_ratio    NUMERIC(12,6),
    custom_cache_ratio         NUMERIC(6,4),
    custom_cache_write_ratio   NUMERIC(6,4),
    custom_input_per_1m_micro  BIGINT,
    custom_output_per_1m_micro BIGINT,
    reason                     VARCHAR(255),
    expires_at                 TIMESTAMPTZ,
    UNIQUE (user_id, model_id)
);
COMMENT ON COLUMN user_pricing.custom_cache_write_ratio IS
    '用户专属缓存写入倍率覆盖；NULL = 用模型级值';

CREATE TABLE pricing_rules (
    rule_code  VARCHAR(64) PRIMARY KEY,
    rule_type  VARCHAR(16) NOT NULL,                 -- volume | time_based | discount | surge
    scope      JSONB NOT NULL DEFAULT '{}',          -- {groups?, models?, users?}；空 = 全局
    params     JSONB NOT NULL,
    priority   INT NOT NULL DEFAULT 0,
    enabled    BOOLEAN NOT NULL DEFAULT true,
    valid_from TIMESTAMPTZ,
    valid_to   TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE pricing_epochs (
    epoch        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    snapshot     JSONB NOT NULL,
    diff_summary JSONB,
    published_by BIGINT REFERENCES users(id),
    published_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ============ 计费账本（分区表） ============

CREATE TABLE billing_records (
    id               BIGINT GENERATED ALWAYS AS IDENTITY,
    request_id       UUID NOT NULL,
    upstream_request_id VARCHAR(128),
    log_type         SMALLINT NOT NULL DEFAULT 2,    -- 1充值 2消费 3管理 4系统 5错误 6退款 7登录（对齐 new-api）
    user_id          BIGINT NOT NULL,
    api_key_id       BIGINT,
    team_id          BIGINT,
    group_code       VARCHAR(32),
    model_name       VARCHAR(128) NOT NULL,
    channel_id       BIGINT,
    channel_key_id   BIGINT,
    status           SMALLINT NOT NULL,              -- 10 reserved / 20 committed / 30 refunded / 40 failed
    prompt_tokens    INT NOT NULL DEFAULT 0,
    cached_tokens    INT NOT NULL DEFAULT 0,
    completion_tokens INT NOT NULL DEFAULT 0,
    reasoning_tokens INT NOT NULL DEFAULT 0,
    media_units      JSONB,
    amount_micro          BIGINT NOT NULL DEFAULT 0,
    original_amount_micro BIGINT NOT NULL DEFAULT 0,
    discount_micro        BIGINT NOT NULL DEFAULT 0,
    upstream_cost_micro   BIGINT,
    pricing_epoch    BIGINT,
    pricing_snapshot JSONB,
    latency_ms       INT,
    ttft_ms          INT,
    is_stream        BOOLEAN NOT NULL DEFAULT false,
    retry_count      SMALLINT NOT NULL DEFAULT 0,
    failover_count   SMALLINT NOT NULL DEFAULT 0,
    sticky_layer     SMALLINT NOT NULL DEFAULT 0,
    upstream_status  SMALLINT,
    error_code       VARCHAR(64),
    client_ip        INET,
    client_type      VARCHAR(32),
    user_agent       VARCHAR(255),
    node             VARCHAR(64),
    content_ref      JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE TABLE billing_records_default PARTITION OF billing_records DEFAULT;
CREATE INDEX idx_br_user_time    ON billing_records (user_id, created_at DESC);
CREATE INDEX idx_br_request      ON billing_records (request_id);
CREATE INDEX idx_br_channel_time ON billing_records (channel_id, created_at DESC);

CREATE TABLE billing_events (
    event_id            BIGINT GENERATED ALWAYS AS IDENTITY,
    user_id             BIGINT NOT NULL,
    request_id          UUID,
    event_type          VARCHAR(16) NOT NULL,        -- reserve|commit|refund|recharge|redeem|adjust|expire
    delta_micro         BIGINT NOT NULL,
    balance_after_micro BIGINT,
    payload             JSONB,
    actor               VARCHAR(64) NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, created_at)
) PARTITION BY RANGE (created_at);
CREATE TABLE billing_events_default PARTITION OF billing_events DEFAULT;
CREATE INDEX idx_be_user_time ON billing_events (user_id, created_at DESC);

CREATE TABLE billing_outbox (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    topic         VARCHAR(64) NOT NULL,
    payload       JSONB NOT NULL,
    status        SMALLINT NOT NULL DEFAULT 0,       -- 0 pending 1 published 2 failed
    retry_count   INT NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at  TIMESTAMPTZ
);
CREATE INDEX idx_outbox_pending ON billing_outbox (next_retry_at) WHERE status <> 1;

CREATE TABLE billing_dlq (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source      VARCHAR(32) NOT NULL,
    payload     JSONB NOT NULL,
    error       TEXT,
    retry_count INT NOT NULL DEFAULT 0,
    status      SMALLINT NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by BIGINT
);

-- ============ 营收运营 ============

CREATE TABLE plans (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plan_code          VARCHAR(64) NOT NULL UNIQUE,
    display_name       VARCHAR(128) NOT NULL,
    grant_micro        BIGINT NOT NULL CHECK (grant_micro > 0),
    group_code         VARCHAR(64),                        -- 兑换后追加分组（NULL=不改组）
    balance_valid_days INT CHECK (balance_valid_days > 0), -- 兑换后设置余额有效期（NULL=不设置）
    status             SMALLINT NOT NULL DEFAULT 1,        -- 1=启用 2=停用
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE redemption_codes (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code_hash    VARCHAR(64) NOT NULL UNIQUE,             -- SHA-256 hex，明文不落库
    amount_micro BIGINT NOT NULL CHECK (amount_micro > 0),
    status       SMALLINT NOT NULL DEFAULT 1,             -- 1=未用 2=已用 3=停用
    batch_id     UUID NOT NULL,                           -- 同批生成溯源
    plan_id      BIGINT REFERENCES plans(id),             -- 绑套餐则金额取套餐 grant
    bind_user_id BIGINT REFERENCES users(id),             -- 限定核销人（NULL=任何人）
    max_per_ip   INT CHECK (max_per_ip > 0),              -- 同批次单 IP 核销上限（Redis 计数）
    created_by   BIGINT REFERENCES users(id),
    redeemed_by  BIGINT REFERENCES users(id),
    redeemed_at  TIMESTAMPTZ,
    expires_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_redemption_batch ON redemption_codes(batch_id);

CREATE TABLE recharge_orders (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_no         VARCHAR(64) NOT NULL UNIQUE,
    user_id          BIGINT NOT NULL REFERENCES users(id),
    amount_micro     BIGINT NOT NULL CHECK (amount_micro > 0),
    currency         VARCHAR(8) NOT NULL DEFAULT 'USD',
    pay_amount       NUMERIC(12,2),                   -- 支付金额（原币种，仅展示）
    gateway          VARCHAR(32) NOT NULL,            -- stripe / epay / manual ...
    gateway_trade_no VARCHAR(128),
    status           SMALLINT NOT NULL DEFAULT 0,     -- 0 created 1 paid 2 failed 3 refunded
    paid_at          TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_recharge_user ON recharge_orders (user_id, created_at DESC);

-- ============ 平台 ============

CREATE TABLE settings (
    key        VARCHAR(128) PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_by BIGINT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE audit_logs (
    id         BIGINT GENERATED ALWAYS AS IDENTITY,
    actor      VARCHAR(64) NOT NULL,                 -- admin:{id} / mcp:{key_id} / system
    action     VARCHAR(64) NOT NULL,                 -- channel.create / pricing.publish / user.credit ...
    target     VARCHAR(128),
    detail     JSONB,
    ip         INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE TABLE audit_logs_default PARTITION OF audit_logs DEFAULT;
CREATE INDEX idx_audit_actor_time ON audit_logs (actor, created_at DESC);

-- ============ 种子数据 ============

INSERT INTO price_groups (group_code, group_ratio, description, is_default)
VALUES ('default', 1, '默认分组', true);
