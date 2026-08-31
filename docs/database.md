# Okapi 存储层设计（PostgreSQL / Redis / ClickHouse / NATS）

> 状态：定稿 v1（2026-08-29）· 配套 [IMPLEMENTATION.md](../IMPLEMENTATION.md)
> 本文件是存储层**唯一权威**：schema、键空间、脚本契约以此为准（DESIGN.md §4 为定价域示意）。

## 0. 全局约定

- **金额一律 micro-USD `BIGINT`**（$1 = 1,000,000 micro）；quota 视图 = USD × 500,000 仅展示层换算；任何表禁止浮点金额列。
- 倍率用 NUMERIC 定点（编译进 PriceBook 后为 micro-USD/token 定点数）。
- 时间 TIMESTAMPTZ；软删 `deleted_at`；主键 BIGINT IDENTITY。
- 迁移：sqlx migrate，只前滚；大表加列须可空或带默认，禁止长锁回填（分批脚本）。
- 大表（billing_records / billing_events / audit_logs）按月 RANGE 分区；worker 自动预建下月分区并按保留策略滚动删除（#1790-1）。
- 老仓库 `billing_events_v2` 在 Okapi 新 schema 统一命名为 `billing_events`。

## 1. PostgreSQL（唯一真理源）

### 1.1 表清单总览

| 域 | 表 | 里程碑 |
| --- | --- | --- |
| 身份权限 | admin_roles, users, oauth_bindings, price_groups, user_groups, group_channel_bindings, api_keys | M1–M2 |
| 渠道模型 | channels, channel_keys, models, model_aliases | M1–M2 |
| 定价 | model_pricing, user_pricing, pricing_rules, pricing_epochs | M0–M2 |
| 计费账本 | billing_records, billing_events, billing_outbox, billing_dlq | M1–M2 |
| 营收运营 | recharge_orders, redemption_codes, redemption_records | M2–M4 |
| 平台 | audit_logs, settings | M2 |
| M4 预留 | teams, team_members, plans, user_subscriptions, notification_channels, notification_rules | M4 |

### 1.2 身份与权限

```sql
CREATE TABLE admin_roles (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    role_code     VARCHAR(64) NOT NULL UNIQUE,        -- channel_admin / finance_ro / support ...
    display_name  VARCHAR(128) NOT NULL,
    permissions   JSONB NOT NULL DEFAULT '[]',        -- ["channel.write.own","billing.read",...]
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email              VARCHAR(255) UNIQUE,           -- 可空：OAuth-only 账号
    username           VARCHAR(64) NOT NULL UNIQUE,
    password_hash      VARCHAR(255),                  -- argon2id（`$argon2id$`）；可空：OAuth-only；`$2*` 前缀 = 老 ok-api 迁移的 bcrypt，校验双轨兼容、改密后写回 argon2id
    role               SMALLINT NOT NULL DEFAULT 1,   -- 1=user 10=admin 100=super_admin（值对齐 new-api）
    admin_role_id      BIGINT REFERENCES admin_roles(id),  -- 自定义子角色，仅 role=10 时生效
    status             SMALLINT NOT NULL DEFAULT 1,   -- 1=active 2=disabled
    price_multiplier   NUMERIC(8,4) NOT NULL DEFAULT 1,    -- 个人级微调（保留 ok-api 灵活性）
    balance_micro      BIGINT NOT NULL DEFAULT 0,     -- 快照列；真理源 = billing_events 重放
    balance_expires_at TIMESTAMPTZ,                   -- 余额有效期（NULL=永不过期；到期 worker 清零记 expire 事件并重置 NULL，M4 #1790-6）
    aff_code           VARCHAR(16),                   -- 邀请码（唯一部分索引；门户首查惰性生成，M4 aff）
    inviter_id         BIGINT,                        -- 邀请人（注册时绑定终身不变；返利仅充值触发 settings.aff_percent_bp 基点，事件 actor=system:aff）
    balance_expires_at TIMESTAMPTZ,                   -- M4 余额有效期（#1790-6），充值刷新
    language           VARCHAR(8) NOT NULL DEFAULT 'auto',
    totp_secret_ciphertext BYTEA,                 -- 2FA 密钥（AES-GCM 加密，M3）
    aff_code           VARCHAR(16) UNIQUE,        -- 邀请码（M4 返利）
    inviter_id         BIGINT REFERENCES users(id),   -- 邀请人（M4）
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at         TIMESTAMPTZ
);
-- 用户分组：多对多（无 users.price_group 单列）
-- 定价 = priority 最高组的 group_ratio；渠道可见性 = 所有组并集

CREATE TABLE oauth_bindings (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id          BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider         VARCHAR(32) NOT NULL,            -- github / linuxdo / telegram / oidc
    provider_user_id VARCHAR(128) NOT NULL,
    profile          JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_user_id)
);

CREATE TABLE team_members (                           -- Team 层（M4）：team 即 user（users.kind='team'）
    team_user_id              BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    member_user_id            BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role                      VARCHAR(16) NOT NULL DEFAULT 'member',
    monthly_spend_limit_micro BIGINT,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (team_user_id, member_user_id)
);
-- users += kind ('user'|'team')；api_keys += member_user_id（团 key 归属成员）

CREATE TABLE oauth_identities (                       -- OAuth/OIDC 绑定（§6.4）；(provider, subject) 唯一
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    provider    VARCHAR(32)  NOT NULL,
    subject     VARCHAR(255) NOT NULL,
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    display     VARCHAR(255),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);

CREATE TABLE redemption_codes (                       -- 兑换码（M4）：一次性核销，credit 事件 actor=system:redeem
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code         VARCHAR(64) NOT NULL UNIQUE,
    amount_micro BIGINT NOT NULL CHECK (amount_micro > 0),
    status       SMALLINT NOT NULL DEFAULT 1,          -- 1=未用 2=已用 3=停用
    batch_id     UUID NOT NULL,
    created_by   BIGINT REFERENCES users(id),
    redeemed_by  BIGINT REFERENCES users(id),
    redeemed_at  TIMESTAMPTZ,
    expires_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE price_groups (
    group_code  VARCHAR(32) PRIMARY KEY,              -- default / vip / svip / enterprise ...
    group_ratio NUMERIC(6,4) NOT NULL DEFAULT 1,
    description VARCHAR(255),
    is_default  BOOLEAN NOT NULL DEFAULT false,
    sort_order  INT NOT NULL DEFAULT 0
);

CREATE TABLE user_groups (
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_code VARCHAR(32) NOT NULL REFERENCES price_groups(group_code),
    priority   INT NOT NULL DEFAULT 0,                -- 定价取最高优先级组
    PRIMARY KEY (user_id, group_code)
);

CREATE TABLE group_channel_bindings (                 -- 渠道可见性矩阵（#6977）
    group_code VARCHAR(32) NOT NULL REFERENCES price_groups(group_code),
    channel_id BIGINT NOT NULL,                       -- FK 在 channels 建表后补
    PRIMARY KEY (group_code, channel_id)
);
-- settings.strict_group_isolation = true：未绑定即不可见；false：绑定为空 = 全可见
-- channels.settings 已注册键：thinking_to_content / bill_by_response_model（按上游响应模型计费，Sub2API 0.1.175 对齐）/ strip_request_fields（不透传的请求顶层字段，new-api rc.23 #6847；model/messages/stream 受保护）/ pass_paths（custom_pass 白名单）

CREATE TABLE api_keys (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id           BIGINT NOT NULL REFERENCES users(id),
    team_id           BIGINT,                         -- M4，可空
    name              VARCHAR(128) NOT NULL DEFAULT '',
    key_hash          CHAR(64) NOT NULL UNIQUE,       -- SHA-256(hex)，明文不落库
    key_prefix        VARCHAR(16) NOT NULL,           -- sk-okapi-xxxx… 展示用
    status            SMALLINT NOT NULL DEFAULT 1,    -- 1=active 2=disabled 3=expired
    quota_mode        SMALLINT NOT NULL DEFAULT 0,    -- 0=共享钱包 1=独立限额
    quota_micro       BIGINT,                         -- 独立限额剩余
    used_micro        BIGINT NOT NULL DEFAULT 0,
    model_allowlist   JSONB,                          -- null = 不限
    group_override    VARCHAR(32) REFERENCES price_groups(group_code),  -- 令牌分组（对齐 new-api）
    rpm_limit         INT, tpm_limit INT, rpd_limit INT,                -- 覆盖用户级限速
    daily_token_limit BIGINT,                         -- 日 token 上限（#6458/#5252）
    max_concurrency   INT,
    ip_allowlist      JSONB,
    expires_at        TIMESTAMPTZ,
    last_used_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at        TIMESTAMPTZ
);
CREATE INDEX idx_api_keys_user ON api_keys(user_id) WHERE deleted_at IS NULL;
```

### 1.3 渠道与模型

```sql
CREATE TABLE channels (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name                 VARCHAR(128) NOT NULL,
    provider             VARCHAR(32) NOT NULL,        -- openai/anthropic/gemini/openai_compat/custom_pass
    api_base             VARCHAR(255),
    status               SMALLINT NOT NULL DEFAULT 1, -- 1=启用 2=手动停用 3=自动停用
    priority             INT NOT NULL DEFAULT 0,      -- 高优先级层耗尽才降层
    weight               INT NOT NULL DEFAULT 1,
    models               JSONB NOT NULL DEFAULT '[]', -- 服务的对外模型名
    model_mapping        JSONB NOT NULL DEFAULT '{}', -- 对外名 → 上游名
    capabilities         JSONB NOT NULL DEFAULT '{}', -- {"tools":true,"vision":true,...} 能力感知路由
    trust_upstream_usage BOOLEAN NOT NULL DEFAULT false,   -- #1790-19 跳过本地 token 复核
    retry_policy         JSONB,                       -- 覆盖全局重试矩阵，null=默认
    settings             JSONB NOT NULL DEFAULT '{}', -- 超时/代理/自定义头/透传路径白名单
    owner_id             BIGINT REFERENCES users(id), -- 渠道属主（#6267，own/all 权限范围）
    upstream_unit_cost   JSONB,                       -- 渠道成本参考价；relative_cost_milli 整数千分比 = 调度层内权重除数（缺省 1000）
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at           TIMESTAMPTZ
);
ALTER TABLE group_channel_bindings
    ADD FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE;

CREATE TABLE channel_keys (                           -- key 级状态机（Sub2API 吸收项 3）
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    channel_id            BIGINT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    credential_ciphertext BYTEA NOT NULL,             -- AES-256-GCM，主密钥来自环境变量
    credential_kind       SMALLINT NOT NULL DEFAULT 0,-- 0=static_key 1=oauth_refresh 2=cloud_sts
    status                SMALLINT NOT NULL DEFAULT 1,-- 1 active / 2 cooling / 3 rate_limited
                                                      -- 4 quota_exhausted / 5 banned / 6 invalid
    cooldown_until        TIMESTAMPTZ,
    failed_count          INT NOT NULL DEFAULT 0,
    last_error            VARCHAR(255),
    weight                INT NOT NULL DEFAULT 1,
    max_concurrency       INT,                        -- 在途计数在 Redis conc:ck:*
    quota_snapshot        JSONB,                      -- 被动采集上游 rate-limit 响应头
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_channel_keys_channel ON channel_keys(channel_id, status);

CREATE TABLE models (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    model_name     VARCHAR(128) NOT NULL UNIQUE,      -- canonical 对外名
    display_name   VARCHAR(128),
    vendor         VARCHAR(64),                       -- 图标/厂商墙（@lobehub/icons）
    capabilities   JSONB NOT NULL DEFAULT '{}',
    context_window INT, max_output INT,
    status         SMALLINT NOT NULL DEFAULT 1,
    sort_order     INT NOT NULL DEFAULT 0,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE model_aliases (                          -- 全局别名/通配（#3001）
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pattern      VARCHAR(128) NOT NULL UNIQUE,        -- 精确名或通配 "gpt-4o-*"
    target_model VARCHAR(128) NOT NULL REFERENCES models(model_name),
    priority     INT NOT NULL DEFAULT 0,              -- 精确 > 通配；同类按 priority 降序
    enabled      BOOLEAN NOT NULL DEFAULT true
);
```

### 1.4 定价域（公式见 DESIGN §3）

```sql
-- 缓存双轨：cache_ratio = 读取（打折），cache_write_ratio = 写入（加价，见 0013 迁移）。
-- 两者方向相反，合并为单轴会导致 Anthropic 缓存写入漏计费约 20%（DESIGN §3.2）。
-- 模态三轴（0014）：音频/图片与文本不同价，gpt-4o-audio 音频输入是文本 16×，
-- 不分轴则该场景漏收约 80%。缺省 1.0 = 按文本计，对纯文本模型零影响。
CREATE TABLE model_pricing (                          -- 真理源：倍率制
    model_id             BIGINT PRIMARY KEY REFERENCES models(id) ON DELETE CASCADE,
    pricing_mode         VARCHAR(16) NOT NULL DEFAULT 'ratio',  -- ratio|per_call|tiered|media|time
    model_ratio          NUMERIC(12,6),               -- 1.0 = $2/1M input
    completion_ratio     NUMERIC(12,6) NOT NULL DEFAULT 1,
    cache_ratio          NUMERIC(6,4)  NOT NULL DEFAULT 1,  -- 缓存读取（Anthropic 官方 0.1）
    cache_write_ratio    NUMERIC(6,4)  NOT NULL DEFAULT 1,  -- 缓存写入（官方 1.25@5m / 2.0@1h，0013）
    audio_ratio          NUMERIC(12,6) NOT NULL DEFAULT 1,  -- 音频输入（gpt-4o-audio 官方 16，0014）
    audio_completion_ratio NUMERIC(12,6) NOT NULL DEFAULT 1,-- 音频输出（叠乘在 audio 之上，官方 2）
    image_ratio          NUMERIC(12,6) NOT NULL DEFAULT 1,  -- 图片输入（相对文本）
    per_call_price_micro BIGINT,                      -- per_call 模式
    tier_expr            TEXT,                        -- tiered 模式表达式
    tier_ratios       JSONB,                          -- service_tier 档位倍率（{"flex":"0.5"}；NULL=全档 1.0，0012）
    media_prices         JSONB,                       -- image/audio/video 单价（micro）
    effective_from       TIMESTAMPTZ,                 -- 定价生效预告
    updated_by           BIGINT REFERENCES users(id),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_pricing (                           -- 用户×模型专属（最高优先级）
    id                        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id                   BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    model_id                  BIGINT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    override_kind             VARCHAR(8) NOT NULL,    -- ratio | absolute
    custom_model_ratio        NUMERIC(12,6),
    custom_completion_ratio   NUMERIC(12,6),
    custom_cache_ratio        NUMERIC(6,4),
    custom_cache_write_ratio  NUMERIC(6,4),           -- NULL = 用模型级值（0013）
    custom_input_per_1m_micro  BIGINT,                -- absolute 模式（落库时同步换算 ratio 冗余）
    custom_output_per_1m_micro BIGINT,
    reason                    VARCHAR(255),
    expires_at                TIMESTAMPTZ,
    UNIQUE (user_id, model_id)
);

CREATE TABLE pricing_rules (                          -- 修饰器栈（保留 ok-api 灵活性）
    rule_code  VARCHAR(64) PRIMARY KEY,
    rule_type  VARCHAR(16) NOT NULL,                  -- volume|time_based|discount|surge
    scope      JSONB NOT NULL DEFAULT '{}',           -- {"groups":[],"models":[],"users":[]} 选择器
    params     JSONB NOT NULL,                        -- 必含 multiplier（十进制字符串，命中即乘）；
                                                      -- volume 追加 min_monthly_tokens（读 tok:{uid}:<yyyymm>）；
                                                      -- time_based 追加 start_minute/end_minute（[start,end) 分钟窗，
                                                      -- 支持跨零点回绕；start==end=空窗永不命中）；
                                                      -- discount 无条件命中；surge 读 settings.surge_inflight_threshold
    priority   INT NOT NULL DEFAULT 0,                -- 同类内排序；类间固定序 volume→time→discount→surge
    enabled    BOOLEAN NOT NULL DEFAULT true,
    valid_from TIMESTAMPTZ, valid_to TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE pricing_epochs (                         -- PriceBook 版本（发布历史/回滚/diff）
    epoch        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    snapshot     JSONB NOT NULL,                      -- 编译后 PriceBook 全量
    diff_summary JSONB,
    published_by BIGINT REFERENCES users(id),
    published_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### 1.5 计费账本（事件溯源）

```sql
CREATE TABLE billing_records (                        -- 请求级明细（分区表）
    id               BIGINT GENERATED ALWAYS AS IDENTITY,
    request_id       UUID NOT NULL,
    upstream_request_id VARCHAR(128),                 -- 上游请求 ID（工单排障；按此检索走 CH）
    log_type         SMALLINT NOT NULL DEFAULT 2,     -- 1充值 2消费 3管理 4系统 5错误 6退款 7登录（对齐 new-api 0-7 枚举）
    user_id          BIGINT NOT NULL,
    api_key_id       BIGINT,
    team_id          BIGINT,
    group_code       VARCHAR(32),
    model_name       VARCHAR(128) NOT NULL,
    channel_id       BIGINT, channel_key_id BIGINT,
    status           SMALLINT NOT NULL,               -- 状态机：10 reserved / 20 committed / 30 refunded / 40 failed
    prompt_tokens    INT NOT NULL DEFAULT 0,
    cached_tokens    INT NOT NULL DEFAULT 0,
    completion_tokens INT NOT NULL DEFAULT 0,
    reasoning_tokens INT NOT NULL DEFAULT 0,
    media_units      JSONB,
    amount_micro          BIGINT NOT NULL DEFAULT 0,  -- 实付
    original_amount_micro BIGINT NOT NULL DEFAULT 0,  -- 标价（无规则/个人折扣）
    discount_micro        BIGINT NOT NULL DEFAULT 0,  -- 原价 − 实付（账单「已节省」/让利报表）
    upstream_cost_micro   BIGINT,                     -- 渠道成本（毛利分析）
    pricing_epoch    BIGINT,
    pricing_snapshot JSONB,                           -- 形状见 DESIGN §3.4
    latency_ms       INT, ttft_ms INT,
    is_stream        BOOLEAN NOT NULL DEFAULT false,
    retry_count      SMALLINT NOT NULL DEFAULT 0,
    failover_count   SMALLINT NOT NULL DEFAULT 0,
    sticky_layer     SMALLINT NOT NULL DEFAULT 0,     -- 0 无 / 1 response_id / 2 session / 3 打分
    upstream_status  SMALLINT,
    error_code       VARCHAR(64),
    client_ip        INET,
    client_type      VARCHAR(32),                     -- UA 解析（#5277）
    user_agent       VARCHAR(255),
    node             VARCHAR(64),                     -- 处理节点（gateway 实例名）
    content_ref      JSONB,                           -- 内容审计三态开启时的引用
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE INDEX idx_br_user_time    ON billing_records (user_id, created_at DESC);
CREATE INDEX idx_br_request      ON billing_records (request_id);
CREATE INDEX idx_br_channel_time ON billing_records (channel_id, created_at DESC);

CREATE TABLE billing_events (                         -- 余额账本，append-only（分区表）
    event_id           BIGINT GENERATED ALWAYS AS IDENTITY,
    user_id            BIGINT NOT NULL,
    request_id         UUID,                          -- 消费/退款事件关联
    event_type         VARCHAR(16) NOT NULL,          -- reserve|commit|refund|recharge|redeem|adjust|expire
    delta_micro        BIGINT NOT NULL,               -- 余额变动（负=扣）
    balance_after_micro BIGINT,                       -- 事件后余额（对账锚点）
    payload            JSONB,                         -- refund.reason / adjust.tags（开放枚举，如 compensation|goodwill|correction|manual_credit|aff_rebate）
    actor              VARCHAR(64) NOT NULL,          -- user:{id} / admin:{id} / mcp:{key_id} / system[:{component}]（如 system:gateway / system:worker）
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, created_at)
) PARTITION BY RANGE (created_at);
CREATE INDEX idx_be_user_time ON billing_events (user_id, created_at DESC);

CREATE TABLE billing_outbox (                         -- 与业务同事务写入，worker SKIP LOCKED 消费
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    topic        VARCHAR(64) NOT NULL,                -- billing.completed / billing.refunded ...
    payload      JSONB NOT NULL,
    status       SMALLINT NOT NULL DEFAULT 0,         -- 0 pending 1 published 2 failed
    retry_count  INT NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);
CREATE INDEX idx_outbox_pending ON billing_outbox (next_retry_at) WHERE status <> 1;

CREATE TABLE billing_dlq (                            -- 终态死信（console/MCP 可 requeue）
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source      VARCHAR(32) NOT NULL,                 -- outbox / jetstream / chsink
    payload     JSONB NOT NULL,
    error       TEXT,
    retry_count INT NOT NULL DEFAULT 0,
    status      SMALLINT NOT NULL DEFAULT 0,          -- 0 pending 1 requeued 2 resolved 3 discarded
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by BIGINT
);
```

### 1.6 营收运营

```sql
CREATE TABLE recharge_orders (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_no         VARCHAR(64) NOT NULL UNIQUE,
    user_id          BIGINT NOT NULL REFERENCES users(id),
    amount_micro     BIGINT NOT NULL,                 -- 入账额度
    currency         VARCHAR(8) NOT NULL DEFAULT 'USD',
    pay_amount       NUMERIC(12,2),                   -- 支付金额（原币种，仅展示）
    gateway          VARCHAR(32) NOT NULL,            -- stripe / epay / manual ...
    gateway_trade_no VARCHAR(128),
    status           SMALLINT NOT NULL DEFAULT 0,     -- 0 created 1 paid 2 failed 3 refunded
    paid_at          TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_recharge_user ON recharge_orders (user_id, created_at DESC);

-- 兑换码增强（#1790-5 / #2845，M4 已按下述形态实现，见 0006/0007/0011 迁移）：
-- 单码一次性核销（多次使用 max_uses 列 backlog——按需再引入 redemption_records 计数表）；
-- 核销留痕在 billing_events（actor=system:redeem, payload.code_id/plan_code），无独立 records 表。
CREATE TABLE redemption_codes (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code_hash     VARCHAR(64) NOT NULL UNIQUE,        -- 明文不落库（生成时一次性返回）
    amount_micro  BIGINT NOT NULL CHECK (amount_micro > 0), -- 面值（绑套餐时被 plans.grant_micro 覆盖）
    status        SMALLINT NOT NULL DEFAULT 1,        -- 1=未用 2=已用 3=停用
    batch_id      UUID NOT NULL,                      -- 同批溯源（per-IP 计数锚点）
    plan_id       BIGINT REFERENCES plans(id),        -- 兑套餐（0011）
    bind_user_id  BIGINT REFERENCES users(id),        -- 限定核销用户（他人核销与不存在同响应）
    max_per_ip    INT CHECK (max_per_ip > 0),         -- 同批次单 IP 核销上限（Redis redeem:ip:* 计数）
    created_by    BIGINT REFERENCES users(id),
    redeemed_by   BIGINT REFERENCES users(id),
    redeemed_at   TIMESTAMPTZ,
    expires_at    TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE plans (                                  -- 套餐模板（#1790-5，0011）
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plan_code          VARCHAR(64) NOT NULL UNIQUE,
    display_name       VARCHAR(128) NOT NULL,
    grant_micro        BIGINT NOT NULL CHECK (grant_micro > 0),
    group_code         VARCHAR(64),                   -- 兑换后追加分组（须存在于 price_groups）
    balance_valid_days INT CHECK (balance_valid_days > 0), -- 兑换后设置余额有效期
    status             SMALLINT NOT NULL DEFAULT 1,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 【蓝图残留清理】以下 records 表为设计期方案，未实现（留痕走 billing_events）：
CREATE TABLE redemption_records (                     -- backlog：max_uses 多次核销时引入
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code_id      BIGINT NOT NULL REFERENCES redemption_codes(id),
    user_id      BIGINT NOT NULL,
    ip           INET,
    amount_micro BIGINT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_redemption_code_ip ON redemption_records (code_id, ip);
```

### 1.7 平台

```sql
CREATE TABLE audit_logs (                             -- 管理操作审计（独立于业务日志，分区表）
    id         BIGINT GENERATED ALWAYS AS IDENTITY,
    actor      VARCHAR(64) NOT NULL,                  -- admin:{id} / mcp:{key_id} / system
    action     VARCHAR(64) NOT NULL,                  -- channel.update / pricing.publish / user.assist ...
    target     VARCHAR(128),
    detail     JSONB,
    ip         INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE TABLE settings (                               -- 全局 KV（strict_group_isolation / 调度权重 / 内容审计三态 ...）
    key        VARCHAR(128) PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_by BIGINT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

已注册键（M4 收口清单；读写走 `POST /admin/settings` + `GET /admin/settings/{key}`）：
`strict_group_isolation`（组可见性两态）、`ssrf_policy`（渠道 api_base 校验策略）、
`mcp_write_enabled`（MCP 写工具总闸）、`single_user_release_ack`（单用户模式生产确认）、
`turnstile_secret`、`oauth_providers`、`payment_epay` / `payment_stripe`、
`model_rate_limits`（用户×模型 RPM）、`realtime_max_conns_per_key`（WS 连接租约上限，缺省 4）、
`aff_percent_bp`（邀请返利基点，缺省 0=关）、`retention_months`（PG 分区保留，缺省 0=永久）、
`notify_channels`（通知多路配置数组）、`balance_low_threshold_micro`（余额低事件阈值，缺省 0=关）、
`critical_rate_limits`（关键接口每 IP 限流覆写，对象键=login/register/totp/redeem，0=关）、
`surge_inflight_threshold`（surge 规则的负载判定阈值：单 gateway 进程在途计费请求数 ≥ 该值即
`surge_active`，缺省 0=永不触发；仅当价簿含启用的 surge 规则时才读取该设置）、
`response_header_whitelist`（backlog 未启用）。

### 1.8 M4 预留（概要，实施时出迁移）

- `teams(id, name, owner_id, balance_micro, settings)`；`team_members(team_id, user_id, role, limits)`——四件套同构。
- `plans(id, name, quota_micro, duration_days, group_code, price_micro)`；`user_subscriptions(user_id, plan_id, remaining_micro, expires_at)`——套餐×分组绑定（#3388）。
- `notification_channels(kind: email|webhook|dingtalk|feishu|telegram, config)`；`notification_rules(event, channel_id, rate_limit)`——事件订阅矩阵（#1790-8）。
- `tasks(id, request_id, user_id, api_key_id, kind: mj|suno|image_async|video, channel_id, upstream_task_id, status, payload, result_ref, billed_micro, created_at, updated_at)`——任务型异步中转（submit → worker 轮询/callback → 完成结算，复用 per_call/media 计费与 billing_events；IMPLEMENTATION §4.4）。
- 邀请返利：不加新表——充值返利记 `billing_events(event_type=adjust, payload.tags=["aff_rebate"], payload.source_order)`，邀请关系在 users.aff_code / inviter_id。

### 1.9 查询边界

PG 只服务**点查与账本**（鉴权回源、CRUD、事件重放对账）；任何看板聚合一律走 CH / Redis——这是 new-api logs 单表两头堵与 Sub2API PG 回填工程复杂度的反面教训（DESIGN 调研结论）。

## 2. Redis（热账本 + 限流 + 实时 KPI）

### 2.1 键空间总表

| 键 | 类型 | TTL | 说明 |
| --- | --- | --- | --- |
| `bal:{<uid>}` | HASH | 永久 | 余额热账本：`avail` + 在途预扣字段 `r:<request_id>` |
| `rl:{<uid>}:k:<key_id>:rpm:<分钟桶>` / `:tpm:<分钟桶>` | STRING 计数 | 120s | key 级限速（限额四件套配置在 api_keys 行，键按 key 维度，多把 key 互不挤兑）。固定分钟窗计数，GCRA 滑窗为升级项；用户级汇总限速随 Team 层（M4）加第二层键 |
| `rl:{<uid>}:k:<key_id>:rpd:<yyyymmdd>` | STRING | 48h | key 级每日请求数（RPD） |
| `rl:{<uid>}:tokd:<key_id>:<yyyymmdd>` | STRING | 48h | key 日 token 上限计数 |
| `rl:{<uid>}:m:<model>:rpm` | GCRA state | 自然过期 | 用户×模型级限流（可选启用，机制复用） |
| `ws:lease:k:<key_id>` | ZSET | 成员 60s 租约/20s 续期；键 6h 兜底 | Realtime WS per-key 连接租约：member=连接 id（request_id），score=到期毫秒；准入 Lua 先 ZREMRANGEBYSCORE 清过期再 ZCARD 比上限（settings.realtime_max_conns_per_key 缺省 4），崩溃连接不续期自然滚出（§14.4） |
| `video:task:{<uid>}:<task_id>` | STRING | 48h | videos 异步任务 → channel_key_id 映射（轮询/下载回源锚点；键含 user_id 天然租户隔离，他人任务 404） |
| `notify:mute:<idx>:<event>` | STRING | =min_interval_secs | 通知频率闸（SET NX；worker 事件 drift/channel_cooldown/balance_low → settings.notify_channels webhook 分发，#1790-8） |
| `redeem:ip:<batch_id>:<ip>` | STRING | 7d | 兑换码同批次单 IP 核销计数（max_per_ip 闸；IP 取 CDN 头，直连无头不限；翻转失败回退） |
| `crl:<scope>:<ip>` | STRING | 60s | 关键接口每 IP 固定窗限流（login/register/totp/redeem；settings.critical_rate_limits 覆写缺省，对齐 new-api rc.24） |
| `conc:{<uid>}:k:<key_id>` | STRING | 1h 泄漏保护 | key 级在途并发（api_keys.max_concurrency） |
| `conc:ck:<channel_key_id>` | STRING | 无（对账清理） | 渠道 key 在途并发信号量 |
| `stick:resp:{<uid>}:v1:<response_id>` | STRING | 30min | 粘性 L1 → channel_key_id |
| `stick:sess:{<uid>}:v1:<session_hash>` | STRING | 1h 滑动 | 粘性 L2 → channel_key_id |
| `auth:key:<sha256>` | STRING(JSON) | 60s | 鉴权缓存（key 元数据+限额+可见组）。值内嵌写入时版本号 |
| `auth:ver` | STRING | 永久 | 鉴权缓存全局版本：console 角色/分组变更 INCR 即 O(1) 跨进程失效；key 级精确失效走单键 DEL |
| `sess:web:<sid>` | STRING | 7d 滑动 | web 会话（/auth/* 自助面专用；门户/数据面仍 API key 单轨，§6.4） |
| `oauth:state:<token>` | STRING | 10min | OAuth authorization-code 流 CSRF state（一次性，校验即删） |
| `spend:tm:{team}:{member}:<yyyymm>` | STRING | 40d | 团成员月度消费计数（结算后累加，预扣前比较；软实时限额） |
| `tok:{<uid>}:<yyyymm>` | STRING | 40d | 用户本月累计 token（`pricing_rules` volume 规则的唯一输入）。结算后累加实际 usage 总量、报价前读取，语义与团成员计数同构（软实时：跨月自然滚动、Redis 故障按 0 处理即不打折，宁少算不错算）。**仅当生效 PriceBook 含启用的 volume 规则时才产生读写**（`PriceBook::has_volume_rules`），无此类规则时热路径零额外 Redis 往返 |
| `pb:epoch` | STRING | 永久 | 【M3 接入】当前 PriceBook epoch。当前实现：gateway 每 30s 直接轮询 PG `MAX(epoch)`（单机/中小规模更简，见 §2.3） |
| `pb:data:<epoch>` | STRING(bin) | 保留 2 版 | 【M3 接入】编译后 PriceBook 快照（多副本大表分发 + PG 减负时启用） |
| `ch:cool:<channel_key_id>` | STRING | =冷却时长 | 状态机冷却镜像 |
| `ch:stat:<channel_id>` | HASH | 5min | 错误率/TTFT EMA（打分输入） |
| `lock:cred:<channel_key_id>` | STRING NX | 30s | 凭证刷新分布式锁 |
| `kpi:sec:<unix_s>` / `kpi:rpm` / `kpi:tpm` | STRING/ZSET | 短 | 平台秒级 QPS 与 60s 滑动窗 |

`{<uid>}` 为 Redis Cluster hash-tag：同一用户的 余额/限速/并发 键同槽，保证 Lua 原子性与线性扩容（档位二关键，IMPLEMENTATION §12.1）。

### 2.2 余额热账本与 Lua 契约

`bal:{uid}` HASH 结构：`avail` = 可用余额（micro）；每笔在途预扣一个字段 `r:<request_id>` = `"<reserved_micro>|<deadline_unix_ms>|<api_key_id>"`（deadline = 预扣时刻 + 10min；api_key_id 供释放对应并发槽与终态补偿定位）。过期预扣由 commit/refund 正常清理，泄漏者由 reconciler 按 deadline 懒清理（M2）。

精度约束：Lua number 为 double，余额比较的精度上限为 2^53 micro ≈ $90 亿；超出该量级的单账户余额视为配置错误（reserve.lua 内注释同此）。M1 实现细节：Lua 脚本经 EVAL 全量下发（EVALSHA/Script 缓存 M2）；KPI 计数暂缓（M2 与看板一起接入）。

```text
reserve ────────────────────────────────────────────────
KEYS = bal:{uid}, rl:{uid}:rpm, rl:{uid}:tpm, rl:{uid}:rpd:<d>, conc:{uid}   （全部同槽）
ARGV = request_id, est_micro, deadline_ms, rpm_cap, tpm_cap, rpd_cap, conc_cap, est_tokens
返回 = {1, balance_after}                          成功（已原子完成：限速+并发+预扣）
       {0, "INSUFFICIENT", balance}                余额不足
       {0, "RATE_LIMITED", which}                  限速/并发超限（不产生任何写入）
语义 = 检查全部通过后：avail -= est；HSET r:<request_id>；INCR 各计数器；INCR conc

commit ────────────────────────────────────────────────
KEYS = bal:{uid}, conc:{uid}
ARGV = request_id, actual_micro
返回 = {1, delta_micro, balance_after}             delta = reserved − actual（正=退，负=补扣）
       {0, "NO_RESERVATION"}                       调用方转对账路径（不直接改余额）
语义 = 读 r:<request_id> → avail += reserved − actual → HDEL → DECR conc；幂等：重复调用返回 NO_RESERVATION

refund ────────────────────────────────────────────────
KEYS = bal:{uid}, conc:{uid}
ARGV = request_id
返回 = {1, released_micro, balance_after}          幂等：无预扣字段时返回 {1, 0, balance}
语义 = 全额释放（上游失败/空回复不计费路径）
```

- KPI 与 `ch:stat` 更新不在 Lua 内（跨槽），走同连接 pipeline fire-and-forget——**账本原子、统计尽力**，统计口径最终以 CH 对账为准。
- `conc:ck:*` acquire/release 为独立单键操作（与用户槽无关）。

### 2.3 PriceBook 与鉴权缓存失效

- 发布（当前实现）：console 发布前**全量编译校验（fail-closed）**，snapshot 存配置全量；gateway 每 30s 轮询 PG `MAX(epoch)`，变化则整表重载 + ArcSwap。M3 升级：NATS 广播 + `pb:*` Redis 快照分发（多副本减 PG 读）。
- 鉴权失效：key 级变更 DEL `auth:key:<hash>`；用户级变更（角色/分组）INCR `auth:ver` 全量失效（跨进程立即生效）；60s TTL 兜底。

## 3. ClickHouse（明细 + 聚合 MV，可整体关闭）

### 3.1 明细事实表（五组列）

```sql
CREATE TABLE request_log_raw (
    -- 身份维
    ts              DateTime64(3),
    request_id      UUID,
    upstream_request_id String,               -- 上游请求 ID（对上游工单排障，对齐 new-api）
    log_type        UInt8,                    -- 对齐 PG 枚举（1充值 2消费 3管理 4系统 5错误 6退款 7登录）
    user_id         UInt64,
    api_key_id      UInt64,
    team_id         UInt64 DEFAULT 0,
    group_code      LowCardinality(String),
    model           LowCardinality(String),
    channel_id      UInt32,
    channel_key_id  UInt32,
    provider        LowCardinality(String),
    client_type     LowCardinality(String),   -- UA 解析（#5277）
    client_ip       String,                   -- 记录与否走 settings.record_ip_log
    node            LowCardinality(String),   -- 处理节点（gateway 实例名，对齐 quota_data.node_name）
    -- 用量
    prompt_tokens     UInt32, cached_tokens UInt32,
    completion_tokens UInt32, reasoning_tokens UInt32,
    media_units       String,                 -- JSON
    -- 金额（售价/原价/优惠/成本 四列 = 毛利与让利分析，new-api 均缺失）
    amount_micro          Int64,
    original_amount_micro Int64,
    discount_micro        Int64,
    upstream_cost_micro   Int64,
    pricing_epoch         UInt64,
    ratio_snapshot        String,             -- 关键倍率 JSON
    -- 性能
    latency_ms UInt32, ttft_ms UInt32, stream UInt8,
    -- 调度（Sub2API 启发）
    retry_count UInt8, failover_count UInt8, sticky_layer UInt8,
    upstream_status UInt16, error_code LowCardinality(String), is_error UInt8
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(ts)
ORDER BY (user_id, ts)
TTL toDateTime(ts) + INTERVAL 180 DAY;        -- 保留期后台可配（#1790-1）
```

### 3.2 MV 矩阵（AggregatingMergeTree ×6）

| MV | 主键 | 服务场景 |
| --- | --- | --- |
| mv_user_day | (user_id, day) | 用户概览、消耗趋势、本月节省、排行榜 |
| mv_apikey_day | (api_key_id, day) | 按 key 统计（#4971） |
| mv_model_hour | (model, hour) | Top 模型、模型速度 |
| mv_group_day | (group_code, day) | 分组经营 |
| mv_channel_5min | (channel_id, ts5) | 渠道健康红绿灯（错误率/TTFT 分位/切换率/粘性命中率全部免费派生——Sub2API 需专门 worker 回填的东西是我们的 MV 副产品） |
| mv_user_model_day | (user_id, model, day) | 用户下钻 |

通用状态列：`countState()、sumState(tokens/amount/original/discount/upstream_cost)、sumState(is_error)`；性能类加 `quantilesState(0.5,0.95,0.99)(ttft_ms / latency_ms)`、`sumState(completion_tokens)+sumState(latency_ms)`（token 加权速度，#5029）。完整示例：

```sql
CREATE MATERIALIZED VIEW mv_channel_5min
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMM(ts5) ORDER BY (channel_id, ts5)
AS SELECT
    channel_id, toStartOfFiveMinutes(ts) AS ts5,
    countState() AS requests,
    sumState(is_error) AS errors,
    sumState(amount_micro) AS amount, sumState(upstream_cost_micro) AS upstream_cost,
    quantilesState(0.5, 0.95, 0.99)(ttft_ms) AS ttft_q,
    sumState(completion_tokens) AS completion_tokens, sumState(latency_ms) AS latency_sum,
    sumState(failover_count) AS failovers,
    countIfState(sticky_layer = 1) AS sticky_resp_hits,
    countIfState(sticky_layer = 2) AS sticky_sess_hits
FROM request_log_raw GROUP BY channel_id, ts5;
```

**规模账：几十亿明细不影响用户花费统计**

- 用户侧任何花费视图都**不扫明细**：mv_user_day 行数 ∝ 活跃用户 × 天（10 万用户 × 365 天 ≈ 3,650 万行/年，与请求量完全解耦）；单用户 30 天花费查询只读约 30 行聚合态，毫秒级：

```sql
SELECT day, sumMerge(amount) AS spend_micro, sumMerge(discount) AS saved_micro
FROM mv_user_day
WHERE user_id = {uid} AND day >= today() - 30
GROUP BY day ORDER BY day;
```

- 明细页（用量日志）查 request_log_raw 走主键前缀 `(user_id, ts)` + 日分区裁剪，只读该用户自己的数据块，成本与全表几十亿行无关。
- 聚合在**写入时增量完成**（MV 随 chsink 批写触发），无夜间批任务，花费 1–3s 内可见；「今日实时」秒级读 Redis KPI。
- **保留期分层**：raw 默认 180 天可配；MV 聚合表保留 ≥2 年（聚合体量小，成本可忽略）——明细过期后，用户的历史账单趋势与月度汇总仍可查。
- 十亿+/日 走 CH 分片 + Distributed 表（IMPLEMENTATION §12.1 档位三），MV 定义不变。

### 3.3 写入与查询护栏

- 写入：chsink 批写（batch 5000 / flush 1s），`insert_deduplication_token` = JetStream 序号区间 `js-<first_seq>-<last_seq>` → 重投批次幂等；ack-after-write；投递超限（max_deliver=5）→ billing_dlq 终态 + ack。【已实现，`worker/nats_relay.rs`，配置 `OKAPI_NATS_URL` 即启用】单机直连形态（无 NATS）：token = outbox id 区间 `outbox-<min>-<max>`；已知边界——重试批次成员变化时 token 变化可致少量明细重复（账本不受影响，CH↔PG 对账检出），NATS 形态无此边界。表侧已开 `non_replicated_deduplication_window=1000` 且插入带 `deduplicate_blocks_in_dependent_materialized_views=1`（MV 传导去重）。
- 查询（console/MCP 统一继承）：`max_execution_time=15s`、`max_memory_usage=2GiB`、结果缓存 60s–10min + singleflight。
- 退款冲销：chsink 同时消费 `billing.refunded`，写负额修正行（同 request_id，log_type=6 退款，对齐 new-api LogTypeRefund），聚合口径自动一致。

### 3.4 可关闭性

`clickhouse.enabled=false` 时：chsink 停用、统计接口 fail-closed 返回 501 error_code（与老仓库行为一致），计费/账本完全不受影响。

### 3.5 与 new-api 统计字段对照（迁移完备性基线）

> 基准：new-api main 分支 `model/log.go`（logs 表 + Stat 统计条）与 `model/usedata.go`（quota_data 看板表），2026-08 逐字段核对。

**logs 表：**

| new-api 字段 | Okapi 落点 | 说明 |
| --- | --- | --- |
| id / created_at / user_id | id / ts / user_id | ✓ |
| type（0–7 枚举） | log_type，值 1:1 对齐（含 6 退款、7 登录） | 消费/错误/退款进 CH；充值/管理/登录低频记录在 PG（billing_events / audit_logs，login = audit action `user.login`） |
| content（自然语言句子） | **有意不同**：error_code + pricing_snapshot + audit 的 op(action, params) | new-api 自身也已转向 action+params 结构化、渲染期 i18n（其 buildOpField 注释），与本设计 i18n 定案同向 |
| username / token_name / channel_name | user_id / api_key_id / channel_id + PG 维表 join | new-api 的 channel_name 也是查询时回填（gorm `->`），同思路；id 稳定，改名不脏历史 |
| model_name / group | model / group_code | ✓ |
| quota | amount_micro，quota 视图 ×500,000 换算 | 另有 original / discount / upstream_cost 三列增强（new-api 无） |
| prompt_tokens / completion_tokens | 同名列 + cached_tokens / reasoning_tokens 拆列 | 超集 |
| use_time（秒） | latency_ms | 更细粒度 |
| is_stream | stream | ✓ |
| ip（用户级 RecordIpLog 开关） | client_ip（PG + CH），开关走 settings.record_ip_log | ✓ |
| request_id / upstream_request_id | request_id / upstream_request_id | ✓（按上游 ID 检索走 CH search） |
| other.frt（首字耗时） | ttft_ms 独立列 | 提升为一等列，可聚合分位数 |
| other.cache_tokens / cache_ratio | cached_tokens 列 / ratio_snapshot | ✓ |
| other.model_ratio / completion_ratio / group_ratio / model_price | ratio_snapshot（CH 关键值）+ pricing_snapshot（PG 全量） | ✓ |
| other.admin_info / audit_info / op | audit_logs（actor / action / detail）+ billing_events.actor | new-api 靠查询时删 JSON 键做权限剥离；我们由 RBAC + 表分离天然承担 |

**quota_data 看板表（user × model × 小时 × group × token × channel × node 八维，内存合并后落库）：**

| new-api 看板查询 | Okapi 落点 |
| --- | --- |
| 单用户 模型×小时 曲线 | 直查 raw（主键 `(user_id, ts)` 裁剪，行数=该用户请求数，毫秒级）；日粒度走 mv_user_model_day |
| 全站 用户×时间 | mv_user_day |
| 全站 模型×时间 | mv_model_hour |
| group / token / channel 维度 | mv_group_day / mv_apikey_day / mv_channel_5min |
| node_name 处理节点 | node 列（gateway 实例名） |
| count / quota / token_used 三度量 | countState / sumState(amount) / sumState(tokens)，超集 |

**日志页统计条（Stat = 消耗 quota + 最近 60s RPM/TPM，可按用户/令牌/模型/渠道/分组过滤）**：无过滤走 Redis KPI 滑动窗（秒级）；带维度过滤走 CH raw 60s 窗口查询（仅扫最新分区，毫秒级）。

另注：new-api 已支持将 logs 表放入 ClickHouse（LOG_DB 双方言），佐证本设计 CH 承载日志分析的路线；其看板表 quota_data 需站长开启 DataExportEnabled 且靠内存定时合并，我们的 MV 随写随聚合、无此开关与丢数窗口。

### 3.6 老 ok-api（Go/UUID schema）迁移映射契约

工具：`okapi migrate okapi-old --dir <dump> [--enc-passphrase X]`（实现 `bins/okapi/src/migrate.rs`，
演练用例 `tests/migrate_okapi_old.rs`）。源侧五表 JSONL 导出（PG `\copy (SELECT row_to_json(t)) TO ...`，
DECIMAL 列建议 `::text` 保精度）。

| 老表.列 | Okapi 落点 | 换算 / 语义 |
| --- | --- | --- |
| users.id (UUID) | —（仅内存映射 uuid→BIGINT） | 老 UUID 不入库；关联靠迁移期 map |
| users.email | users.email | **幂等锚**（老库唯一键）；username 缺失时取 email 本地部分 |
| users.password_hash (bcrypt) | users.password_hash | 原样迁移，`$2*` 双轨校验；二跑 `COALESCE` 不覆盖已改密码 |
| users.role (varchar) | users.role (SMALLINT) | super_admin→100 / admin→10 / 其余→1 |
| users.status | users.status | active→1，其余→2 |
| users.balance DECIMAL(20,8) | billing_events(adjust) + Redis | ×1e6 定点截断（第 7 位起舍去，禁浮点）；≤0 不入账仅告警。actor=`system:migrate:okapi_old` 兼作幂等锚 |
| users.quota_* / tags / parent_id | 不迁 | quota 周期语义与 Okapi 钱包模型不同；tags/子账户由 price_groups + teams 表达 |
| api_keys.key_hash (bcrypt) | **不可用** | bcrypt 不可逆且热路径成本高；Okapi 只认 SHA-256 |
| api_keys.key_encrypted | api_keys.key_hash | AES-256-GCM 解密（key = PBKDF2-HMAC-SHA256(pass, SHA256("okapi-key-derivation:"+pass)[..16], 100k, 32B)，与老 Go `pkg/crypto` 逐字节对齐）→ 明文重算 SHA-256。**解不出一律不落库**（错哈希=永久鉴权失败），计入 `keys_undecryptable` 提示重建 |
| api_keys.allowed_models / rate_limit_rpm / expires_at | model_allowlist / rpm_limit / expires_at | 直映；allowed_models 接受 JSON 数组或逗号串两种导出形态 |
| providers × provider_api_keys | channels × channel_keys | **每 key 一 channel**（`old/{code}/{key_name}`），保留 key 级 base_url（空则回落 provider.api_endpoint）/ supported_models / weight / priority |
| provider_api_keys.adapter_type | channels.provider | claude→anthropic / google→gemini / openai→openai / 其余→openai_compat 并告警 |
| models.input_price DECIMAL(12,8) USD/1K | model_pricing.model_ratio | ÷ 基准 $0.002/1K；completion_ratio = out/in，cache_ratio = cached_in/in（比值推导，6 位定点）。老库无缓存写入价字段 → cache_write_ratio 留 1.0，迁移后按 provider 官方定价手工配置 |
| models.request_price | model_pricing.per_call_price_micro | `pricing_type=request` → per_call 模式 |
| models.hourly/monthly_price | 不迁 | 无对应计价语义，告警 |
| pricing_rules + 4 张 binding 表 | 不迁（语义等价替代） | Okapi 用 price_groups + user_pricing + model_pricing.tier_ratios 表达；见 IMPLEMENTATION §11.4 吸收判据 |
| plugins / proxy_ip* / audit_logs / request_logs / usage_stats_daily | 不迁 | 运维与历史统计域：日志留源库，CH 从新开始 |

## 4. NATS JetStream

### 4.1 Stream 拓扑

| Stream | Subjects | 存储 | 保留 | max_age | 副本 |
| --- | --- | --- | --- | --- | --- |
| BILLING | `billing.>`（completed / refunded） | file | limits | 48h | 3（单机 1） |
| NOTIFY | `notify.>`（balance.low / channel.down / …） | file | limits | 7d | 1 |

`pricing.epoch` 走 **core NATS 普通 pub/sub**（非持久）：广播丢失由 30s epoch 轮询兜底，不需要 JetStream 成本。

### 4.2 消费者

| durable | stream | ack_wait | max_deliver | 说明 |
| --- | --- | --- | --- | --- |
| chsink | BILLING | 30s | 5 | 批写 CH；超限 → PG billing_dlq（source=chsink） |
| audit | BILLING | 30s | 5 | 对账抽样比对 |
| notifier | NOTIFY | 60s | 3 | 通知分发（M4 全量） |

单机无 NATS 形态：worker 内嵌线程直接消费 billing_outbox（SKIP LOCKED），链路语义不变。

## 5. 一致性与对账

- **三方对账**：Redis `bal:{uid}.avail` ↔ PG billing_events 重放余额 ↔ CH 金额汇总；reconciler 每 5min 抽样 + 每日全量，差异 > 0 即告警并生成修正 adjust 事件（人工确认）。
- **幂等锚点**：request_id 全链路唯一；commit/refund Lua 幂等；CH 批次 dedup token；outbox 重投靠 `billing_records.status` 状态机去重。
- **在途预扣泄漏**：reconciler 扫 `bal:{uid}` 中超过 deadline 的 `r:*` 字段 → 按 billing_records 终态决定 commit 或 refund 补偿。
- 余额快照列 `users.balance_micro` 由 worker 周期从事件流重放校准（展示与导出用，不参与计费判定）。
