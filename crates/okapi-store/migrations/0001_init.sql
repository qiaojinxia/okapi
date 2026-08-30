-- Okapi M1 初始 schema（唯一权威：docs/database.md；本文件为其 M1 子集的落地）
-- 迁移只前滚；分区表先建 DEFAULT 分区，月度分区滚动由 M2 worker 接管。

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
    password_hash      VARCHAR(255),
    role               SMALLINT NOT NULL DEFAULT 1,   -- 1=user 10=admin 100=super_admin（对齐 new-api）
    admin_role_id      BIGINT REFERENCES admin_roles(id),
    status             SMALLINT NOT NULL DEFAULT 1,   -- 1=active 2=disabled
    price_multiplier   NUMERIC(8,4) NOT NULL DEFAULT 1,
    balance_micro      BIGINT NOT NULL DEFAULT 0,     -- 快照列；真理源 = billing_events 重放
    balance_expires_at TIMESTAMPTZ,
    language           VARCHAR(8) NOT NULL DEFAULT 'auto',
    totp_secret_ciphertext BYTEA,
    aff_code           VARCHAR(16) UNIQUE,
    inviter_id         BIGINT REFERENCES users(id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at         TIMESTAMPTZ
);

CREATE TABLE price_groups (
    group_code  VARCHAR(32) PRIMARY KEY,
    group_ratio NUMERIC(6,4) NOT NULL DEFAULT 1,
    description VARCHAR(255),
    is_default  BOOLEAN NOT NULL DEFAULT false,
    sort_order  INT NOT NULL DEFAULT 0
);

CREATE TABLE user_groups (
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_code VARCHAR(32) NOT NULL REFERENCES price_groups(group_code),
    priority   INT NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, group_code)
);

CREATE TABLE api_keys (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id           BIGINT NOT NULL REFERENCES users(id),
    team_id           BIGINT,
    name              VARCHAR(128) NOT NULL DEFAULT '',
    key_hash          CHAR(64) NOT NULL UNIQUE,
    key_prefix        VARCHAR(16) NOT NULL,
    status            SMALLINT NOT NULL DEFAULT 1,   -- 1=active 2=disabled 3=expired
    quota_mode        SMALLINT NOT NULL DEFAULT 0,   -- 0=共享钱包 1=独立限额
    quota_micro       BIGINT,
    used_micro        BIGINT NOT NULL DEFAULT 0,
    model_allowlist   JSONB,
    group_override    VARCHAR(32) REFERENCES price_groups(group_code),
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

-- ============ 渠道与模型 ============

CREATE TABLE channels (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name                 VARCHAR(128) NOT NULL,
    provider             VARCHAR(32) NOT NULL,       -- openai / anthropic / gemini / openai_compat / custom_pass
    api_base             VARCHAR(255),
    status               SMALLINT NOT NULL DEFAULT 1,-- 1=启用 2=手动停用 3=自动停用
    priority             INT NOT NULL DEFAULT 0,
    weight               INT NOT NULL DEFAULT 1,
    models               JSONB NOT NULL DEFAULT '[]',
    model_mapping        JSONB NOT NULL DEFAULT '{}',
    capabilities         JSONB NOT NULL DEFAULT '{}',
    trust_upstream_usage BOOLEAN NOT NULL DEFAULT false,
    retry_policy         JSONB,
    settings             JSONB NOT NULL DEFAULT '{}',
    owner_id             BIGINT REFERENCES users(id),
    upstream_unit_cost   JSONB,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at           TIMESTAMPTZ
);

CREATE TABLE channel_keys (
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    channel_id            BIGINT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    credential_ciphertext BYTEA NOT NULL,            -- M1 过渡：明文字节存放，AES-GCM 在 M2 接入（见 store::channels 注释）
    credential_kind       SMALLINT NOT NULL DEFAULT 0,
    status                SMALLINT NOT NULL DEFAULT 1,-- 1 active / 2 cooling / 3 rate_limited / 4 quota_exhausted / 5 banned / 6 invalid
    cooldown_until        TIMESTAMPTZ,
    failed_count          INT NOT NULL DEFAULT 0,
    last_error            VARCHAR(255),
    weight                INT NOT NULL DEFAULT 1,
    max_concurrency       INT,
    quota_snapshot        JSONB,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_channel_keys_channel ON channel_keys(channel_id, status);

CREATE TABLE models (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    model_name     VARCHAR(128) NOT NULL UNIQUE,
    display_name   VARCHAR(128),
    vendor         VARCHAR(64),
    capabilities   JSONB NOT NULL DEFAULT '{}',
    context_window INT,
    max_output     INT,
    status         SMALLINT NOT NULL DEFAULT 1,
    sort_order     INT NOT NULL DEFAULT 0,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ============ 定价域 ============

CREATE TABLE model_pricing (
    model_id             BIGINT PRIMARY KEY REFERENCES models(id) ON DELETE CASCADE,
    pricing_mode         VARCHAR(16) NOT NULL DEFAULT 'ratio',
    model_ratio          NUMERIC(12,6),
    completion_ratio     NUMERIC(12,6) NOT NULL DEFAULT 1,
    cache_ratio          NUMERIC(6,4)  NOT NULL DEFAULT 1,
    per_call_price_micro BIGINT,
    tier_expr            TEXT,
    media_prices         JSONB,
    effective_from       TIMESTAMPTZ,
    updated_by           BIGINT REFERENCES users(id),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_pricing (
    id                         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id                    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    model_id                   BIGINT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    override_kind              VARCHAR(8) NOT NULL,  -- ratio | absolute
    custom_model_ratio         NUMERIC(12,6),
    custom_completion_ratio    NUMERIC(12,6),
    custom_cache_ratio         NUMERIC(6,4),
    custom_input_per_1m_micro  BIGINT,
    custom_output_per_1m_micro BIGINT,
    reason                     VARCHAR(255),
    expires_at                 TIMESTAMPTZ,
    UNIQUE (user_id, model_id)
);

CREATE TABLE pricing_rules (
    rule_code  VARCHAR(64) PRIMARY KEY,
    rule_type  VARCHAR(16) NOT NULL,                 -- volume | time_based | discount | surge
    scope      JSONB NOT NULL DEFAULT '{}',
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

-- ============ 平台 ============

CREATE TABLE settings (
    key        VARCHAR(128) PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_by BIGINT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ============ 种子数据 ============

INSERT INTO price_groups (group_code, group_ratio, description, is_default)
VALUES ('default', 1, '默认分组', true);
