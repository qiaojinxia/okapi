# Okapi 存储层设计（PostgreSQL / Redis / ClickHouse / NATS）

> 状态：定稿 v1（2026-08-29）· 配套 [IMPLEMENTATION.md](../IMPLEMENTATION.md)
> 本文件是存储层**唯一权威**：schema、键空间、脚本契约以此为准（DESIGN.md §4 为定价域示意）。

## 0. 全局约定

- **交易金额与余额使用 micro-USD `BIGINT`**（$1 = 1,000,000 micro）；历史 actor 结转使用 `NUMERIC(38,0)` 保存可能超过 bigint 的相抵累计额，返回余额时仍须校验 bigint 与 Redis 安全整数范围。quota 视图 = USD × 500,000 仅展示层换算；禁止浮点金额列。
- 倍率用 NUMERIC 定点（编译进 PriceBook 后为 micro-USD/token 定点数）。
- 时间 TIMESTAMPTZ；软删 `deleted_at`；主键 BIGINT IDENTITY。
- 迁移：sqlx migrate，只前滚；大表加列须可空或带默认，禁止长锁回填（分批脚本）。2026-10-06 起从单个基线
  `migrations/0001_baseline.sql` 开始（原 0001–0038 连同会话绑定登录 key 压平，见 IMPLEMENTATION §11.10；开发阶段不做
  历史兼容，压平前建的库直接 `scripts/dev-reset.sh` 重建）；本文提到的 00xx 为历史编号。
- 大表（billing_records / billing_events / audit_logs）按月 RANGE 分区；worker 自动预建下月分区并按保留策略滚动删除（#1790-1）；删除账本分区前必须同事务保留资金结转与财务凭证，见 [历史账本结转](billing-retention.md)。
- 老仓库 `billing_events_v2` 在 Okapi 新 schema 统一命名为 `billing_events`。

## 1. PostgreSQL（唯一真理源）

### 1.1 表清单总览

| 域 | 表 | 里程碑 |
| --- | --- | --- |
| 身份权限 | admin_roles, users, oauth_bindings, price_groups, user_groups, group_channel_bindings, api_keys | M1–M2 |
| 渠道模型 | channels, channel_keys, models, model_aliases, proxies, proxy_groups, proxy_group_members | M1–M2 |
| 定价 | model_pricing, user_pricing, pricing_rules, pricing_epochs | M0–M2 |
| 计费账本 | billing_records, billing_events, billing_event_carry, billing_record_receipts, billing_outbox, billing_dlq | M1–M2 |
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
    price_multiplier   NUMERIC(12,6) NOT NULL DEFAULT 1,   -- 个人级微调（保留 ok-api 灵活性）；0003 起 1e-6 精度
    balance_micro      BIGINT NOT NULL DEFAULT 0,     -- 快照列；真理源 = billing_events 重放
    balance_expires_at TIMESTAMPTZ,                   -- 余额有效期（NULL=永不过期；到期 worker 清零记 expire 事件并重置 NULL，M4 #1790-6）
    aff_code           VARCHAR(16),                   -- 邀请码（唯一部分索引；门户首查惰性生成，M4 aff）
    inviter_id         BIGINT,                        -- 邀请人（注册时绑定终身不变；返利仅充值触发 settings.aff_percent_bp 基点，事件 actor=system:aff）
    balance_expires_at TIMESTAMPTZ,                   -- M4 余额有效期（#1790-6），充值刷新
    language           VARCHAR(8) NOT NULL DEFAULT 'auto',
    totp_secret_ciphertext BYTEA,                 -- 2FA 密钥（AES-GCM 加密，M3）
    totp_last_counter BIGINT,                    -- 最近成功的时间片，条件 UPDATE 防重放（0031）
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
    group_ratio NUMERIC(12,6) NOT NULL DEFAULT 1,        -- 0003 起 1e-6 精度（4 位小数会把小倍率静默舍成 0）
    description VARCHAR(255),
    is_default  BOOLEAN NOT NULL DEFAULT false,
    sort_order  INT NOT NULL DEFAULT 0,
    pool_code   VARCHAR(32) NOT NULL DEFAULT 'default', -- 该分组的用户打哪个池；分组必有池（FK 在 channel_pools 后补）
    self_select BOOLEAN NOT NULL DEFAULT false,       -- 用户可在门户为自己的 key 选此分组（价随组走）
    rpm_limit   INT CHECK (rpm_limit IS NULL OR rpm_limit > 0),  -- 分组内每用户每分钟请求上限；NULL = 不限（IMPLEMENTATION §11.32）
    rph_limit   INT CHECK (rph_limit IS NULL OR rph_limit > 0)   -- 分组内每用户每小时请求上限；NULL = 不限。随鉴权缓存下发，Redis rl:{uid}:g:*
);

CREATE TABLE user_groups (
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_code VARCHAR(32) NOT NULL REFERENCES price_groups(group_code),
    priority   INT NOT NULL DEFAULT 0,                -- 定价取最高优先级组
    PRIMARY KEY (user_id, group_code)
);

-- 渠道池：一组渠道 + 在这组里怎么选（见 §3.7 论证）。
-- 与 price_groups 正交：分组只管"付多少钱"，池只管"打哪些上游、怎么选"。
-- 内置池 `default`（迁移种子，不可删）：新渠道缺省加入，未指定池的分组走这里。
CREATE TABLE channel_pools (
    pool_code          VARCHAR(32) PRIMARY KEY,        -- default / stable / fast / cheap ...
    description        VARCHAR(255),
    routing_strategy   VARCHAR(24) NOT NULL DEFAULT 'priority_weighted',
    -- priority_weighted：priority 分层 + 层内成本修正加权随机（历史行为，默认）
    -- least_latency  ：层内按 Redis 时延 EWMA 升序（需 lat:ck:* 有数据，缺数据退化为 priority_weighted）
    fallback_pool_code VARCHAR(32) REFERENCES channel_pools(pool_code), -- 本池无候选时退到的池（单跳）
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT channel_pools_strategy_chk
        CHECK (routing_strategy IN ('priority_weighted', 'least_latency')),
    CONSTRAINT channel_pools_fallback_not_self
        CHECK (fallback_pool_code IS NULL OR fallback_pool_code <> pool_code)
);


`trust_upstream_usage = false` 时，结算前按 tiktoken 分词与本次请求实测的
token/字符密度重算 prompt 与 completion，取与上游报告值中的**较大者**：上游可以
报得比本地算的多，但不能更少——这个开关的用途就是防转售型上游少报。缓存 /
音频 / 图片轴只有上游知道，本地无从复核，原样保留。本地计数对 OpenAI 方言是
权威的，对 Anthropic / Gemini 只是同量级近似（它们的分词器不在本进程内），
取 max 也正是为了不让近似偏低反过来伤到诚实上报的渠道。


`retry_policy` 两项，均按渠道生效、写入后由 gateway 夹取到安全区间：

- `same_key_retries`（缺省 1，夹 0..=3）：瞬态失败（连接 / 超时 / 5xx）时同一把 key
  重试几次。空回复不适用——那种情况直接换渠道。
- `first_output_timeout_secs`（缺省 30，夹 5..=300）：首字窗口（连接 + 首个产出事件）。
  按渠道配是有实义的：直连官方与经两跳转售的上游，首 token 该等多久差一个数量级，
  一个全局常数要么把慢渠道误判成超时，要么让快渠道的故障拖满 30 秒才换。

夹取而不是照单全收，是因为这是渠道级配置：写错一个 0 或多一个零，代价分别是
"永不重试"和"一个坏渠道把请求吊死几分钟"。

CREATE TABLE pool_channels (                          -- 池 ↔ 渠道（多对多）
    pool_code         VARCHAR(32) NOT NULL REFERENCES channel_pools(pool_code) ON DELETE CASCADE,
    channel_id        BIGINT NOT NULL,                -- FK 在 channels 建表后补
    priority_override INT,                            -- 本池内优先级覆盖；NULL = 用 channels.priority
    weight_override   INT,                            -- 本池内权重覆盖（作用于该渠道全部 key）；NULL = 用各 key weight
    PRIMARY KEY (pool_code, channel_id)
);

-- 池的解析：api_keys.pool_override > 生效定价组的 price_groups.pool_code > 'default'；
-- 池链 = [主池, 主池.fallback_pool_code]（单跳不递归）。
-- 可见性只有一条规则（IMPLEMENTATION §11.14）：**渠道只服务它所在的池**——
--   候选 = 池链内任一池的成员；排序 (池序, 有效优先级 DESC, key id)，同渠道两池共存只按靠前的池算；
--   不在任何池的渠道 = 孤儿，对谁都不可达（列表页与站点规模条标红）。
-- 此前的"无池只看未入池 / strict_group_isolation 三态"已由 0002 迁移退役：那套规则让 vip 组
-- 看不到任何公共渠道、UI 文案又与之相反。0002 把未入池渠道并入 default 池、无池分组指向 default 池，
-- 老部署行为不变。历史 group_channel_bindings 已在 0015 迁移为 pool_<group_code> 并删表。
-- channels.settings 已注册键：thinking_to_content / bill_by_response_model（按上游响应模型计费，Sub2API 0.1.175 对齐）/ strip_request_fields（不透传的请求顶层字段，new-api rc.23 #6847；model/messages/stream 受保护）/ inject_request_fields（对象，dispatch 在 strip 之后浅合并到请求顶层；model/messages/stream/provider 受保护不可注入；缺省空=零开销；写入最多 32 键 / 4KB）/ responses_native（/v1/responses 同方言直转到上游 /responses；缺省 openai=true、openai_compat=false，其它协议忽略恒降级；上游 404/405 自动回退降级）/ pass_paths（custom_pass 白名单）
-- / api_version（仅 provider=azure：数据面 api-version，`YYYY-MM-DD[-preview]`，管理面写入时校验形状；缺省 2024-10-21；每个出向请求都带 `?api-version=`）
-- / image_stream_usage（直接 Images SSE 多图计数：cumulative 缺省，最后一份累计值且各计费轴不可回退；per_image 逐完成事件检查求和。写入只接受这两个字符串，派发时冻结，账单快照同步口径和完整性。JSON 回退始终按响应总用量一次结算。）
-- / aws_region（仅 provider=bedrock：SigV4 签名区域覆写；缺省从 api_base 主机名 `bedrock-runtime.{region}.amazonaws.com` 解析，VPC 端点等解析不出时必填）
-- / proxy_url（已废弃，写入一律 400：出口改由 channels.egress_* 绑定代理 / 代理组，Realtime WS 同样经此握手，见下文 Egress proxies）
-- / extra_headers（对象 string→string，附加到每条上游请求；写入拒 Authorization / api-key / x-api-key / x-goog-api-key / Host / Content-Type / 逐跳头 / x-okapi-request-id，热路径再跳过一次；鉴权头后写覆盖）。
-- provider=azure 的约定：api_base = 资源端点 `https://{res}.openai.azure.com`（必填，无缺省；贴了 `/openai` 或 `/openai/v1` 后缀网关自行剥掉）；
-- 出向 URL = `{endpoint}/openai/deployments/{deployment}/{chat/completions|embeddings|images/generations|images/edits|audio/*}?api-version=`，鉴权 `api-key` 头；
-- **部署名 = model_mapping 的值**（未映射则用模型名本身，Azure 缺省部署名与模型名相同时零配置）。responses_native 对 azure 忽略（恒降级）；videos / realtime 不路由 azure 渠道。
-- provider=bedrock（IMPLEMENTATION §11.35）：api_base = `https://bedrock-runtime.{region}.amazonaws.com`（必填；region 从主机名解析，解析不出用 settings.aws_region）；
-- 凭证 `ACCESS_KEY_ID:SECRET[:SESSION_TOKEN]` → SigV4，否则视为 Bedrock API key → Bearer；出向 = InvokeModel `/model/{modelId}/invoke[-with-response-stream]`，
-- 请求体 Anthropic Messages（去 model/stream、加 anthropic_version=bedrock-2023-05-31）；**模型 ID = model_mapping 的值**（`us.anthropic.claude-…-v1:0`）。只服务 Anthropic 方言模型。
-- provider=vertex（IMPLEMENTATION §11.35）：api_base = `https://{loc}-aiplatform.googleapis.com/v1/projects/{project}/locations/{loc}`（必填，须含 /projects/ 与 /locations/）；
-- 凭证 = 服务账号 JSON 原文（JWT RS256 换 access token，进程内缓存）或现成 access token；`claude*` → publishers/anthropic `:rawPredict|:streamRawPredict`
-- （anthropic_version=vertex-2023-10-16），其余 → publishers/google `:generateContent|:streamGenerateContent?alt=sse`。两家只路由 chat 族入口。
-- provider=anthropic_max / codex（IMPLEMENTATION §11.38，实验性）：站长自己的订阅经 OAuth 登录；channel_keys.credential_ciphertext 里是
-- JSON `{"kind":"oauth","access_token","refresh_token","expires_at"(unix 秒),"account_id"?}`（仍经 AES-GCM 信封，非 JSON 凭证照旧当静态 key）。
-- 请求惰性刷新、worker 预刷新与手动刷新共用 §4.3 租约（Redis `lock:cred:<key_id>`），
-- refresh token 轮转按原密文字节条件回写；invalid_grant 仅作废仍匹配原凭证的 key（status=6）。
-- 首字前 OAuth API 401 对被拒 access_token 锁内强制刷新一次；仍失败冷却 30s，invalid_grant/缺 refresh_token 保持 invalid。
-- anthropic_max：api_base 缺省 `https://api.anthropic.com/v1`，Bearer + `anthropic-beta: oauth-2025-04-20` + system 首句前置；
-- codex：api_base 缺省 `https://chatgpt.com/backend-api/codex`，只走 Responses，头 `chatgpt-account-id` / `originator: codex_cli_rs`，store 恒 false。

CREATE TABLE api_keys (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id           BIGINT NOT NULL REFERENCES users(id),
    team_id           BIGINT,                         -- M4，可空
    name              VARCHAR(128) NOT NULL DEFAULT '',
    key_hash          CHAR(64) NOT NULL UNIQUE,       -- SHA-256(hex)，明文不落库
    key_ciphertext    BYTEA,                          -- 0026：可选 AES-256-GCM 副本；历史 key 为 NULL
    key_prefix        VARCHAR(16) NOT NULL,           -- sk-okapi-xxxx… 展示用
    status            SMALLINT NOT NULL DEFAULT 1,    -- 1=active 2=disabled 3=expired
    quota_mode        SMALLINT NOT NULL DEFAULT 0,    -- 0=共享钱包 1=独立限额
    quota_micro       BIGINT,                         -- 密钥累计消费上限（非剩余额度）；quota_mode=0 不限
    used_micro        BIGINT NOT NULL DEFAULT 0,
    model_allowlist   JSONB,                          -- null = 不限
    group_override    VARCHAR(32) REFERENCES price_groups(group_code),  -- 令牌分组（对齐 new-api）；用户可在 self_select 组 ∪ 已分配组内自选（§11.14 R4）
    pool_override     VARCHAR(32),                    -- 令牌钉住某渠道池（优先于分组的池；FK 在 channel_pools 后补，仅管理面）
    rpm_limit         INT, tpm_limit INT, rpd_limit INT,                -- 覆盖用户级限速
    daily_token_limit BIGINT,                         -- 日 token 上限（#6458/#5252）
    max_concurrency   INT,
    ip_allowlist      JSONB,                         -- 来源 IP 白名单（地址 / CIDR 数组；null = 不限）。只约束 /v1 数据面，门户登录不受限（§11.17）
    expires_at        TIMESTAMPTZ,
    last_used_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at        TIMESTAMPTZ,
    session_hash      TEXT                            -- 登录 key 所属会话 sid 的 sha256 hex（null = 普通 key）。
                                                      -- 只在请求带着该会话 cookie 且会话有效时可用；门户密钥列表不显示
);
CREATE INDEX idx_api_keys_user ON api_keys(user_id) WHERE deleted_at IS NULL;
-- 一个会话同一时刻只有一把有效登录 key；登录时按用户清理死会话的登录 key
CREATE UNIQUE INDEX api_keys_live_session_key ON api_keys (session_hash) WHERE session_hash IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX api_keys_user_session_keys ON api_keys (user_id) WHERE session_hash IS NOT NULL AND deleted_at IS NULL;
```

### 1.3 渠道与模型

```sql
CREATE TABLE channels (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name                 VARCHAR(128) NOT NULL,
    provider             VARCHAR(32) NOT NULL,        -- openai/openai_compat/azure/anthropic/gemini/bedrock/vertex/anthropic_max/codex/custom_pass
    api_base             VARCHAR(255),
    status               SMALLINT NOT NULL DEFAULT 1, -- 1=启用 2=手动停用 3=自动停用
    priority             INT NOT NULL DEFAULT 0,      -- 高优先级层耗尽才降层
    weight               INT NOT NULL DEFAULT 1,
    models               JSONB NOT NULL DEFAULT '[]', -- 服务的对外模型名
    model_mapping        JSONB NOT NULL DEFAULT '{}', -- 对外名 → 上游名
    capabilities         JSONB NOT NULL DEFAULT '{}', -- {"tools":true,"vision":true,...} 能力感知路由
    trust_upstream_usage BOOLEAN NOT NULL DEFAULT false,   -- true=照单全收上游 usage；false=结算前本地复核（见下）
    retry_policy         JSONB,                       -- {same_key_retries, first_output_timeout_secs}；null=默认
    settings             JSONB NOT NULL DEFAULT '{}', -- 超时/代理/自定义头/透传路径白名单
    owner_id             BIGINT REFERENCES users(id), -- 渠道属主（#6267，own/all 权限范围）
    upstream_unit_cost   JSONB,                       -- relative_cost_milli 整数千分比（缺省 1000 = 官方标价）：调度层内权重除数，也是毛利核算的成本系数（§11.18）
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at           TIMESTAMPTZ
);
ALTER TABLE pool_channels
    ADD FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE;
ALTER TABLE price_groups ADD FOREIGN KEY (pool_code) REFERENCES channel_pools(pool_code);
ALTER TABLE api_keys     ADD FOREIGN KEY (pool_override) REFERENCES channel_pools(pool_code);

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
    -- 以下三项为 key 级配额（0016）。同一渠道下不同 key 的权限与限额常常不同：
    -- 同组织的两把 OpenAI key 可能一把有 gpt-4 权限、一把没有；套餐不同则 RPM 不同。
    -- 只有渠道级 models/限额时，这类差异只能靠拆渠道表达，base_url 与设置被迫重复。
    model_subset          JSONB,                      -- null = 继承 channels.models；非空 = 该 key 只服务这些模型
    rpm_limit             INT,                        -- null = 不限；固定窗口计数在 Redis rpm:ck:*
    daily_spend_cap_micro BIGINT,                     -- null = 不限；当日累计消费在 Redis spend:ck:*（结算时累加）
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
    catalog_config JSONB NOT NULL DEFAULT '{}',       -- 0027：kind / description / input_modalities / output_modalities
    status         SMALLINT NOT NULL DEFAULT 1,
    sort_order     INT NOT NULL DEFAULT 0,
    -- 模型级降级链（0016）：本模型**无任何可用候选**时按序改投这些模型。
    -- 只在"渠道都挂了/都被限住"时触发，不覆盖上游 4xx 与用户参数错误——
    -- 那些换模型也不会好，只会把错误藏起来。计费与响应都按实际服务模型走（见 DESIGN §3）。
    fallback_models JSONB NOT NULL DEFAULT '[]',
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
    cache_ratio          NUMERIC(12,6) NOT NULL DEFAULT 1, -- 0027：缓存轴与其他倍率统一 6 位精度
    cache_write_ratio    NUMERIC(12,6) NOT NULL DEFAULT 1, -- 未有时长明细时使用的通用缓存写入倍率
    audio_ratio          NUMERIC(12,6) NOT NULL DEFAULT 1,  -- 音频输入（gpt-4o-audio 官方 16，0014）
    audio_completion_ratio NUMERIC(12,6) NOT NULL DEFAULT 1,-- 音频输出（叠乘在 audio 之上，官方 2）
    image_ratio          NUMERIC(12,6) NOT NULL DEFAULT 1,  -- 图片输入（相对文本）
    modality_ratios       JSONB NOT NULL DEFAULT '{}',    -- 0025：独立缓存/模态价轴，十进制字符串
    per_call_price_micro BIGINT,                      -- per_call 模式
    tier_expr            TEXT,                        -- tiered 模式表达式
    tier_ratios       JSONB,                          -- service_tier 档位倍率（{"flex":"0.5"}；NULL=全档 1.0，0012）
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
    custom_cache_ratio        NUMERIC(12,6),
    custom_cache_write_ratio  NUMERIC(12,6),          -- NULL = 用模型级值（0013）；0003 起 1e-6 精度
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
                                                      -- volume 追加 min_monthly_tokens（读 tok:{uid}:<yyyymm>）
                                                      --   与/或 min_monthly_spend_micro（读 usd:{uid}:<yyyymm>；
                                                      --   两轴至少一项，同配 = AND，§11.5 消费额轴）；
                                                      -- time_based 追加 start_minute/end_minute（[start,end) 分钟窗，
                                                      -- 支持跨零点回绕；start==end=空窗永不命中）
                                                      --   与可选 weekdays（0=周日…6=周六 数组，缺省每天，UTC 钟源）；
                                                      -- discount 无条件命中；surge 读 settings.surge_inflight_threshold；
                                                      -- 可选 stacking_mode（stackable 缺省/exclusive/best_for_user，
                                                      --   桶内裁决语义见 okapi-pricing rules.rs；未知值装载期拒绝）
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
    upstream_cost_micro   BIGINT,                     -- 上游成本 = 官方价（乘分组倍率前）× 实际候选冻结的 relative_cost_milli / 1000；依据存 pricing_snapshot；未成功结算的失败/释放记录 NULL，管理员退款仅翻转原账单状态并保留原成本（§11.18）
    pricing_epoch    BIGINT,                          -- 有报价快照时必须与 pricing_snapshot.epoch 相同，不能在上游返回后读取新版本标注旧金额
    pricing_snapshot JSONB,                           -- 形状见 DESIGN §3.4；共享文本准入 reservation 包含主/已准入降级候选的完整估价依据；estimated_usage 不属于实测用量，失败记录只存 epoch/reservation
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
    pool             SMALLINT NOT NULL DEFAULT 0,     -- 0 钱包 1 订阅池（这笔由谁付，0004）
    content_ref      JSONB,                           -- 内容审计三态开启时的引用
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE INDEX idx_br_user_time    ON billing_records (user_id, created_at DESC);
CREATE INDEX idx_br_request      ON billing_records (request_id);
CREATE INDEX idx_br_channel_time ON billing_records (channel_id, created_at DESC);
-- 语义上每个 request_id 恰一行（终态由 status 翻转，退款不另起行），但分区表加不了
-- 不含分区键的唯一约束，所以由 ledger::record_settlement 在事务开头以
-- `SELECT EXISTS(... WHERE request_id = $1)` 做幂等闸：已落账则整笔跳过（不再写
-- events / 快照 / key 用量 / outbox），返回 Ok 并告警。覆盖的是 settle_write 重试撞上
-- 「COMMIT 已成功、回包丢失」的窗口——否则事件流多一笔 −amount，对账 repair 又以事件流
-- 为权威把 Redis 也改成双扣。同一 request_id 不存在并发结算（Redis commit 闸保证串行）。

CREATE TABLE billing_events (                         -- 余额账本，append-only（分区表）
    event_id           BIGINT GENERATED ALWAYS AS IDENTITY,
    user_id            BIGINT NOT NULL,
    request_id         UUID,                          -- 消费/退款事件关联
    event_type         VARCHAR(16) NOT NULL,          -- reserve|commit|refund|recharge|redeem|adjust|expire|sub_grant|sub_reset|sub_expire
    delta_micro        BIGINT NOT NULL,               -- 池内余额变动（负=扣）
    balance_after_micro BIGINT,                       -- 事件后该池余额（对账锚点）
    payload            JSONB,                         -- refund.reason / adjust.tags（开放枚举，如 compensation|goodwill|correction|manual_credit|aff_rebate）
    actor              VARCHAR(64) NOT NULL,          -- user:{id} / admin:{id} / mcp:{key_id} / system[:{component}]（如 system:gateway / system:worker）
    pool               SMALLINT NOT NULL DEFAULT 0,   -- 0 钱包 1 订阅池（0004；两池各自对账，IMPLEMENTATION §11.28）
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, created_at)
) PARTITION BY RANGE (created_at);
CREATE INDEX idx_be_user_time ON billing_events (user_id, created_at DESC);
-- billing_records 同样带 pool SMALLINT NOT NULL DEFAULT 0（这笔请求由哪个池付）。
-- 不变式（reconciler 两池分别核）：
--   钱包  Redis bal.avail + Σ在途(pool=0) == Σ delta_micro WHERE pool=0 == users.balance_micro
--   订阅  Redis bal.sub   + Σ在途(pool=1) == Σ delta_micro WHERE pool=1（sub_reset/sub_expire 记的是池变动 delta，跨窗口累计成立）
-- users.balance_micro 快照只随 pool=0 事件动；api_keys.used_micro 两池都累加（用量就是用量）。

-- 0028：投递前提交冻结数据，完成时原子清除大载荷，身份回执保留。
CREATE TABLE billing_ch_batches (
    id UUID PRIMARY KEY,
    status SMALLINT NOT NULL DEFAULT 0 CHECK (status IN (0,1,2)), -- 0 pending 1 complete 2 DLQ
    event_count INTEGER NOT NULL CHECK (event_count BETWEEN 0 AND 500),
    rows JSONB NOT NULL CHECK (jsonb_typeof(rows)='array'),
    payloads JSONB NOT NULL CHECK (jsonb_typeof(payloads)='array'),
    retry_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CHECK (status=1 OR (jsonb_array_length(rows)=event_count AND jsonb_array_length(payloads)=event_count))
);
CREATE INDEX idx_ch_batches_pending ON billing_ch_batches(next_retry_at,created_at) WHERE status=0;
CREATE TABLE billing_ch_events (
    event_key TEXT PRIMARY KEY,
    batch_id UUID NOT NULL REFERENCES billing_ch_batches(id)
);
CREATE INDEX idx_ch_events_batch ON billing_ch_events(batch_id);

CREATE TABLE billing_outbox (                         -- 与业务同事务写入，worker SKIP LOCKED 消费
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id     UUID NOT NULL DEFAULT gen_random_uuid(), -- 0028 服务端身份，不用 request_id 猜事件
    ch_batch_id  UUID REFERENCES billing_ch_batches(id),
    stats_protocol SMALLINT NOT NULL DEFAULT 0 CHECK (stats_protocol IN (0,1)), -- 0029：新 relay 确认发布=1
    topic        VARCHAR(64) NOT NULL,                -- billing.completed / billing.refunded ...
    payload      JSONB NOT NULL,
    status       SMALLINT NOT NULL DEFAULT 0,         -- 0 pending 1 published 2 failed
    retry_count  INT NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);
CREATE INDEX idx_outbox_pending ON billing_outbox (next_retry_at) WHERE status <> 1;
CREATE UNIQUE INDEX idx_outbox_event_id ON billing_outbox(event_id);
CREATE INDEX idx_outbox_ch_batch ON billing_outbox(ch_batch_id) WHERE ch_batch_id IS NOT NULL;
CREATE INDEX idx_outbox_published_unassigned ON billing_outbox(id) WHERE status=1 AND stats_protocol=1 AND ch_batch_id IS NULL;

CREATE TABLE billing_dlq (                            -- 终态死信（console/MCP 可 requeue）
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source      VARCHAR(32) NOT NULL,                 -- outbox / jetstream / chsink
    payload     JSONB NOT NULL,
    error       TEXT,
    retry_count INT NOT NULL DEFAULT 0,
    status      SMALLINT NOT NULL DEFAULT 0,          -- 0 pending 2 discarded；重投后删除 DLQ 行
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by BIGINT,
    ch_batch_id UUID REFERENCES billing_ch_batches(id), -- 0028 批次整体处置
    event_key TEXT
);
CREATE UNIQUE INDEX idx_dlq_delivery_event ON billing_dlq(event_key) WHERE event_key IS NOT NULL;
```

携带报价的音频、视频与自定义透传记录，其 PG `pricing_epoch`、outbox/CH 同名字段必须与实际报价快照的 `epoch` 一致。等待上游时发布新价格不改变该请求已生成的报价；下一请求按新版本报价。自定义透传失败释放预扣后，PG/outbox/CH 的实付、原金额、优惠均为 0；上游成本保持 PG NULL、outbox/CH 0 与 unknown 状态。尝试报价快照保留，但不得计为消费或已节省金额。归档回执保留原快照，历史账单不重写。详见 [价格版本一致性核对](pricing-epoch-consistency-audit.md)。

### 1.6 营收运营

```sql
CREATE TABLE recharge_orders (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_no         VARCHAR(64) NOT NULL UNIQUE,
    user_id          BIGINT NOT NULL REFERENCES users(id),
    amount_micro     BIGINT NOT NULL,                 -- 入账额度
    currency         VARCHAR(8) NOT NULL DEFAULT 'USD',
    pay_amount       NUMERIC(12,2),                   -- 原币种报价，回调金额须精确匹配
    gateway          VARCHAR(32) NOT NULL,            -- stripe / epay / manual ...
    gateway_trade_no VARCHAR(128),
    payment_contract_version SMALLINT NOT NULL DEFAULT 0, -- 0023：新订单显式写 1
    merchant_id      VARCHAR(128),                   -- Epay 下单时商户快照
    checkout_session_id VARCHAR(128),                -- Stripe 创建后持久绑定
    status           SMALLINT NOT NULL DEFAULT 0,     -- 0 created 1 paid 2 failed 3 refunded
    plan_id          BIGINT REFERENCES plans(id),     -- 非空 = 订阅购买单（0004）：amount_micro 为售价快照，
                                                      --   支付成功激活/续期订阅而**不入钱包**；NULL = 钱包充值
    paid_at          TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_recharge_user ON recharge_orders (user_id, created_at DESC);
CREATE UNIQUE INDEX recharge_checkout_session_unique
    ON recharge_orders(gateway,checkout_session_id) WHERE checkout_session_id IS NOT NULL;
CREATE INDEX recharge_paid_transaction_lookup
    ON recharge_orders(gateway,gateway_trade_no) WHERE status IN (1,3);

-- 0023：永久交易认领，和订单核销、钱包 credit / 订阅 grant 在同一 PG 事务中提交。
CREATE TABLE payment_receipts (
    gateway VARCHAR(32) NOT NULL,
    merchant_id VARCHAR(128) NOT NULL,                -- Stripe 使用空串命名空间
    trade_no VARCHAR(128) NOT NULL,
    order_id BIGINT NOT NULL UNIQUE REFERENCES recharge_orders(id),
    currency VARCHAR(8) NOT NULL,
    amount_minor BIGINT NOT NULL CHECK(amount_minor > 0),
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(gateway,merchant_id,trade_no)
);

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

CREATE TABLE plans (                                  -- 套餐（#1790-5，0011；订阅形态 0004，IMPLEMENTATION §11.28）
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plan_code          VARCHAR(64) NOT NULL UNIQUE,
    display_name       VARCHAR(128) NOT NULL,
    kind               SMALLINT NOT NULL DEFAULT 0,   -- 0 充值模板（兑换即入钱包）1 订阅（周期配额池）
    grant_micro        BIGINT NOT NULL CHECK (grant_micro > 0), -- kind 0：入账金额；kind 1：**每个周期**的池额度
    group_code         VARCHAR(64),                   -- kind 0：兑换后追加分组；kind 1：订阅有效期内附加分组（到期收回，只收回订阅授予的）
    balance_valid_days INT CHECK (balance_valid_days > 0), -- kind 0 专用：兑换后设置余额有效期
    price_micro        BIGINT NOT NULL DEFAULT 0 CHECK (price_micro >= 0), -- kind 1：自助购买售价（0 = 不可购买，只能兑换码/管理员发放）
    period             SMALLINT,                      -- kind 1 必填：1 日 2 周 3 月（自激活时刻起算，非自然日历）
    duration_days      INT CHECK (duration_days > 0), -- kind 1 必填：有效期；同套餐再购 = 续期（expires_at += duration）
    sort_order         INT NOT NULL DEFAULT 0,        -- 门户套餐页排序
    description        TEXT,                          -- 门户套餐卡副文案（站长自填，不受 i18n 约束）
    status             SMALLINT NOT NULL DEFAULT 1,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind = 0 OR (period IN (1, 2, 3) AND duration_days IS NOT NULL))
);

CREATE TABLE user_subscriptions (                     -- 订阅实例（0004；§1.8 预留兑现）
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id        BIGINT NOT NULL REFERENCES users(id),
    plan_id        BIGINT NOT NULL REFERENCES plans(id),
    plan_code_snapshot VARCHAR(64) NOT NULL,           -- 0020：实例权益不跟随目录修改
    display_name_snapshot VARCHAR(128) NOT NULL,
    period_snapshot SMALLINT NOT NULL CHECK (period_snapshot IN (1,2,3)),
    group_code_snapshot VARCHAR(64),
    status         SMALLINT NOT NULL DEFAULT 1,       -- 1 active 2 expired 3 cancelled
    starts_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at     TIMESTAMPTZ NOT NULL,
    window_start   TIMESTAMPTZ NOT NULL,              -- 当前配额窗 [window_start, window_end)
    window_end     TIMESTAMPTZ NOT NULL,
    quota_micro    BIGINT NOT NULL,                   -- 每窗额度快照（套餐改价不影响存量订阅）
    granted_group  BOOLEAN NOT NULL DEFAULT false,    -- 分组是订阅新加的（到期才收回；用户本来就在组里则不动）
    source         VARCHAR(96) NOT NULL,              -- purchase:<order_no> / redeem:<code_id> / admin:<uid>
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX uq_user_sub_active ON user_subscriptions (user_id) WHERE status = 1; -- 同一时刻最多一个激活订阅
CREATE INDEX idx_user_sub_window ON user_subscriptions (window_end) WHERE status = 1;    -- worker 滚窗/到期扫描
-- 池余额不在 PG：热值在 Redis bal:{uid}.sub（§2.2），权威值 = billing_balance_totals 的 pool=1 总额（含历史结转）。

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
    actor      VARCHAR(64) NOT NULL,                  -- admin:{id} / mcp:{key_id} / user:{id}（登录）/ anon（未知邮箱的登录失败）/ system
    action     VARCHAR(64) NOT NULL,                  -- channel.update / pricing.publish / user.assist / user.login / user.login_failed ...
    target     VARCHAR(128),
    detail     JSONB,
    ip         INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

-- settings.streaming_policy = {idle_timeout_secs: 1..480 (默认 120), heartbeat_secs: 1..60 (默认 15)}；
-- 上游事件空闲与客户端心跳独立，总请求预算 8min，超时部分产出走现有结算。
CREATE TABLE settings (                               -- 全局 KV（site_notice / registration_policy / model_rpm_limits / 内容审计三态 ...）
    key        VARCHAR(128) PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_by BIGINT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

已注册键（M4 收口清单；读写走 `POST /admin/settings` + `GET /admin/settings/{key}`）：
`pricing_base_per_1m_micro`（倍率基准价草稿，正整数 micro-USD / 1M 输入 tokens，默认 2,000,000，最大 1,000,000,000,000；发布时以 `base_price_per_1m_micro` 写入 `pricing_epochs.snapshot`。网关启动/热更只读对应 epoch 的已发布基准，旧 epoch 缺字段取默认值；不改变按次价、阶梯绝对价、用户绝对单价、历史账单或 USD/quota 汇率。新 token 账单快照记录 `base_price_per_1m_usd`，绝对价换算仍使用固定 $2 的内部归一化基准。详见 [倍率基准价](pricing-base.md)）、
`strict_group_isolation`（组可见性两态）、`ssrf_policy`（渠道 api_base 校验策略）、
`mcp_write_enabled`（MCP 写工具总闸）、`single_user_release_ack`（单用户模式生产确认）、
`image_tasks_enabled`（异步图片创建开关，布尔值，缺省 false；关闭仍允许已有任务执行/查询/取消/下载，见 [图片契约](images-contract.md)）、
`turnstile_secret`、`turnstile_verify_url`（siteverify 地址覆写，缺省 Cloudflare 官方端点；内网出口代理或自动化用例的 mock 用）、`oauth_providers`、`payment_epay` / `payment_stripe`、
`model_rpm_limits`（用户×模型 RPM）、`realtime_max_conns_per_key`（Realtime 连接租约上限，缺省 4）、`responses_ws_max_conns_per_key`（Responses WS 独立连接租约上限，缺省 4）、`responses_ws_turn_timeout_secs`（单轮硬时限，缺省/最大 480 秒，可缩短；另留最多 30 秒收尾）、`responses_ws_transport`（native/http/auto，缺省 auto；channels.settings 同名项非 null 时优先）、`responses_ws_context_bytes`（HTTP 桥接连接内快照序列化字节预算，缺省/最大 128 MiB）、
`aff_percent_bp`（邀请返利基点，缺省 0=关）、`retention_months`（PG 分区保留，缺省 0=永久）、
`notify_channels`（通知多路配置数组，可含 webhook `secret`；列表与审计脱敏）、`balance_low_threshold_micro`（余额低事件阈值，缺省 0=关）、
`critical_rate_limits`（关键接口每 IP 限流覆写，对象键=login/register/totp/setup/redeem/email_code/password_forgot/password_reset/invalid_api_key，0=关）、
`site_url`（对外站点地址；找回密码与 OAuth 回调必须配置合法 HTTP(S) 基址，绝不按请求 Host/转发头推导；缺失、非法或数据库读取失败均拒绝起跳）、
`smtp`（邮件出口：host/port/security(starttls|tls|none)/username/password/from_address/from_name/reply_to；
host 空=未配置；含密码故列表接口只回"已配置"，IMPLEMENTATION §11.27）、
`surge_inflight_threshold`（surge 规则的负载判定阈值：Redis 汇总的集群 HTTP 请求与 Responses WS 活动轮次数 ≥ 该值即
`surge_active`，缺省 0=永不触发；仅当价簿含启用的 surge 规则时才读取该设置）、
`margin_breaker`（负毛利自动熔断，IMPLEMENTATION §11.34：`{enabled, window_hours, min_requests,
min_cost_micro, margin_bp, cooldown_secs, lift_secs}`，缺省关；worker 读取，状态在 Redis `mb:blocks`）、
`playground_presets`（站点聊天预设数组 `[{name, model, system, temperature, max_tokens, top_p}]`，
公开只读 `GET /api/playground/presets` 白名单收口，IMPLEMENTATION §11.39）、
`web_session_limit`（每用户同时有效的 web 会话数上限，整数，缺省 0=不限；登录 / OAuth 回调建会话后超限即踢
最早的会话，刚建的这条永不被踢，IMPLEMENTATION §11.37）、
`response_header_whitelist`（backlog 未启用）。

### 1.8 持久图片任务与 M4 预留

`0006_image_tasks.sql` 已加入图片队列，具体接口、容量及恢复边界见 [图片契约](images-contract.md)：

- `image_tasks`：用户/key 共同归属、请求 hash、幂等键 hash、有限大小的请求体、状态、执行租约、当前预扣 ID、结果元数据、错误、容量预算、到期时间和 `billing_pending` 恢复标记。轮询不读取输入或二进制图片；终态清除输入和来源 IP。
- `image_task_attempts`：独立预扣 ID 到任务的映射，保留旧执行尝试，使过期预扣清扫可与结果事务锁定同一任务行。处理前中断的重新领取使用新 ID；已发送的未知结果不重发。
- `image_task_artifacts`：任务下按图片索引保存的私有 `BYTEA` 和 MIME；删除任务级联清除图片与执行尝试。结果与 PG 账单/outbox 共用事务，Redis 结算单独幂等恢复；未完成恢复的记录不得被 TTL 清理。

`0007_image_objects.sql` 增加可选的 S3 转存：`image_task_objects` 在任何远端写入前保存对象键、存储位置摘要、内容 SHA-256/字节数、版本、租约及重试状态，不保存存储凭证。确认上传后，在同一事务内把 artifact 改为对象引用并释放 `BYTEA`；失败期间仍可从 PG 下载。对象记录对任务采用非级联外键，远端删除成功后才清除引用，避免任务 TTL 提前丢失清理依据。幂等键释放与任务清理都跳过仍有对象记录的任务。S3 不改变已有逻辑容量预算，具体部署和故障边界见 [图片契约](images-contract.md)。

以下仍为预留：

- `teams(id, name, owner_id, balance_micro, settings)`；`team_members(team_id, user_id, role, limits)`——四件套同构。
- ~~`plans(...)`；`user_subscriptions(...)`~~——已于 0004 兑现为 §1.6 的 `plans.kind=1` + `user_subscriptions`（remaining 不落 PG，热值在 Redis `bal.sub`，权威值为 pool=1 事件和）。
- `notification_channels(kind: email|webhook|dingtalk|feishu|telegram, config)`；`notification_rules(event, channel_id, rate_limit)`——事件订阅矩阵（#1790-8）。
- 通用 `tasks`：Midjourney / Suno / 供应商原生 Batch / callback 等任务适配。上述图片队列不等同于供应商任务状态轮询或原生批量处理。
- 邀请返利：不加新表——充值返利记 `billing_events(event_type=adjust, payload.tags=["aff_rebate"], payload.source_order)`，邀请关系在 users.aff_code / inviter_id。

### 1.9 查询边界

PG 只服务**点查与账本**（鉴权回源、CRUD、事件重放对账）；任何看板聚合一律走 CH / Redis——这是 new-api logs 单表两头堵与 Sub2API PG 回填工程复杂度的反面教训（DESIGN 调研结论）。

## 2. Redis（热账本 + 限流 + 实时 KPI）

### 2.1 键空间总表

| 键 | 类型 | TTL | 说明 |
| --- | --- | --- | --- |
| `bal:{<uid>}` | HASH | 永久 | 余额热账本：`avail`（钱包）+ `sub` / `sub_until` / `sub_epoch`（订阅池剩余 / 截止秒 / 窗口身份）+ 同步预扣 `r:<request_id>` + 长期冻结 `h:<UUID>` |
| `hold:{<uid>}:<UUID>` | STRING JSON | 永久 | 长期冻结/关闭凭证；不设 TTL，防止延迟重放再次扣款。PG 真理源为 `balance_holds`，尚未实现凭证 GC |
| `rl:{<uid>}:k:<key_id>:rpm:<分钟桶>` / `:tpm:<分钟桶>` | STRING 计数 | 120s | key 级限速（限额四件套配置在 api_keys 行，键按 key 维度，多把 key 互不挤兑）。固定分钟窗计数，GCRA 滑窗为升级项；用户级汇总限速随 Team 层（M4）加第二层键 |
| `rl:{<uid>}:k:<key_id>:rpd:<yyyymmdd>` | STRING | 48h | key 级每日请求数（RPD） |
| `count:{<key_id>}:rpm:<分钟桶>` / `:rpd:<UTC日桶>` | STRING 计数 | 120s / 48h | Responses input_tokens 独立准入；RPM/RPD 取 key 正数配置，RPM 默认 60、RPD 默认不限；不写生成的余额/Token 计数。与 leases 使用相同 Cluster hash tag，Lua 原子校验后递增 |
| `count:{<key_id>}:leases` | ZSET | 成员 90s / 键 120s | 计数在途租约，member 为请求 UUID；并发取 key 正数配置，否则 4。准入先删过期成员；正常返回或取得租约后的取消异步 ZREM。Redis 故障拒绝准入 |
| `rl:{<uid>}:tokd:<key_id>:<yyyymmdd>` | STRING | 48h | key 日 token 上限计数 |
| `rl:{<uid>}:m:<model>:rpm:<分钟桶>` | STRING 计数 | 120s | 用户×模型级 RPM（settings.model_rpm_limits；INCR 固定分钟窗，尽力语义，Redis 故障放行） |
| `rl:{<uid>}:g:<group>:rpm:<分钟桶>` / `:rph:<小时桶>` | STRING 计数 | 120s / 7200s | 分组级限流（`price_groups.rpm_limit / rph_limit`，IMPLEMENTATION §11.32）：分组内**每用户**固定窗计数，全部计费端点在 reserve 前检查；超限 429 `rate_limited` param=group_rpm / group_rph；限额随鉴权缓存下发，未配置零往返 |
| `ws:lease:k:<key_id>` | ZSET | 成员 60s 租约/20s 续期；键 6h 兜底 | Realtime WS per-key 连接租约：member=连接 id（request_id），score=到期毫秒；准入 Lua 先 ZREMRANGEBYSCORE 清过期再 ZCARD 比上限（settings.realtime_max_conns_per_key 缺省 4），崩溃连接不续期自然滚出（§14.4） |
| `ws:responses:k:<key_id>` | ZSET | 成员 60s 租约/20s 续期；键 120s 兜底 | Responses WS 独立连接租约；member=连接 UUID，score=到期毫秒。准入/续期 Redis 故障拒绝连接，关闭后释放；默认每 key 4 个连接，与 Realtime 分开计数 |
| `video:task:{<uid>}:<task_id>` | STRING | 48h | videos 异步任务 → channel_key_id 映射（轮询/下载回源锚点；键含 user_id 天然租户隔离，他人任务 404） |
| `notify:mute:<idx>:<event>` | STRING | 投递租约 120s / 成功后 min_interval_secs | SET NX 认领 owner UUID，单键 Lua 核对 owner 后：成功置 sent 并启动静默窗，失败 DEL；告警审计使用独立事件键 |
| `totp:pending:<uid>:<sha256(sid)>:<ticket>` | STRING | 5min | 服务端密封 TOTP 秘钥；仅用户和发起会话可确认，成功后删除。时间片防重放真理源为 users.totp_last_counter |
| `mb:blocks` | HASH | 永久（字段按 `until` 由 worker 剪除） | 负毛利熔断状态（IMPLEMENTATION §11.34）：字段 `<group>\|<channel_id>` → JSON `{state: blocked\|lifted, since, until, requests, amount_micro, cost_micro, margin_bp}`。worker 每 5 分钟按 settings.margin_breaker 评估 CH mv_analysis_hour 写入；gateway 10s 进程缓存一次 HGETALL，`blocked` 且未到 `until` 的对从候选里摘掉（Redis 故障 = 不熔）；`lifted` 为管理员解除，期间评估器跳过该对；关闭功能时整键删除 |
| `verify:email:<email>` | STRING | 10min | 注册邮箱验证码（6 位数字；重发覆盖旧码；注册对上即 DEL，一次性。IMPLEMENTATION §11.27） |
| `verify:email:cd:<email>` | STRING | 60s | 同一邮箱验证码重发冷却（SET NX） |
| `pwreset:<sha256(token)>` | STRING | 30min | 找回密码 token → user_id（明文 token 只出现在邮件链接里；重设成功即 DEL） |
| `redeem:ip:<batch_id>:<ip>` | STRING | 7d | 兑换码同批次单 IP 核销计数（max_per_ip 闸；IP 取 CDN 头，直连无头不限；翻转失败回退） |
| `crl:<scope>:<ip>` | STRING | 60s | 关键接口每 IP 固定窗限流（login/register/totp/setup/redeem/email_code/password_forgot/password_reset/invalid_api_key；settings.critical_rate_limits 覆写缺省，对齐 new-api rc.24） |
| `conc:{<uid>}:k:<key_id>` | STRING | 1h 泄漏保护 | key 级在途并发（api_keys.max_concurrency） |
| `conc:ck:<channel_key_id>` | STRING | 1h 泄漏保护 | 渠道 key 在途并发信号量；生成和原生 Token 计数共用 |
| `conc:px:{<proxy_id>}:v1` | ZSET | 成员 90s 续租 / 键 2× | 出口代理在途并发租约（`proxies.max_concurrency`，§11.41）：与 key 租约一起占，任一满了当「渠道忙」并退回已占的那份；跨 key、跨网关实例共享 |
| `egress:probe:round` | STRING | ≈ 探测间隔 | 出口代理后台探测的本轮租约（SET NX EX）：多 worker 实例时每轮只由拿到的那个执行 |
| `inflight:gauge` | HASH | 整键 1h；字段超过 10s 不计入、5min 清除 | 集群在途量（HTTP 请求与 Responses WS 活动轮次）：`node → <count>\|<unix_ms>`。HTTP 在启用 surge 规则时跟踪响应体；Responses WS 跟踪单轮准入至结算；活动期间每秒续报，零/非零切换及时上报，其余变化一秒采样。正常结束、报错、断开、取消均释放；每实例串行写入，节点名称须唯一。软实时计价输入，不用于严格并发限流 |
| `rpm:ck:<channel_key_id>:<分钟桶>` | STRING 计数 | 120s | 渠道 key 级 RPM 闸（`channel_keys.rpm_limit`）。固定分钟窗，与 `crl:*` 同机制；超限即把该 key 从候选里摘掉而非拒绝请求——同渠道其它 key 仍可承接 |
| `spend:ck:<channel_key_id>:<yyyymmdd>` | STRING | 48h | 渠道 key 当日累计消费 micro（`channel_keys.daily_spend_cap_micro`）。结算后累加、选路前比较：软实时，宁可略超也不阻塞热路径 |
| `lat:ck:<channel_key_id>` | STRING | 10min | 时延 EWMA（毫秒）。结算侧按 `new = old*0.7 + sample*0.3` 更新；`least_latency` 池按此升序。无数据的 key 视为中位数，避免新 key 因"无历史"被永久冷落或被优先灌流 |
| `stick:resp:{<uid>}:v2:<api_key_id>:<sha256(response_id)>` | STRING/JSON | 30 天固定 | Responses L1：渠道 ID、凭证 ID、上游账号摘要；按用户/API key 隔离，Lua 原子建立且不可改绑，读取不续期；读写故障均拒绝改投。详见 [历史路由](responses-history-routing.md) |
| `stick:sess:{<uid>}:v1:<session_hash>` | STRING | 1h 滑动 | 粘性 L2 → channel_key_id |
| `auth:key:<sha256>` | STRING(JSON) | 60s | 鉴权缓存（key 元数据+限额+可见组）。值内嵌写入时版本号 |
| `auth:ver` | STRING | 永久 | 鉴权缓存全局版本：console 角色/分组变更 INCR 即 O(1) 跨进程失效；key 级精确失效走单键 DEL |
| `sess:web:<sid>` | STRING | 7d 滑动 | web 会话（/auth/* 自助面；门户/数据面仍 API key 单轨，§6.4）。登录 key 绑定它：用登录 key 鉴权时校验并续期，会话没了 key 即失效 |
| `sess:idx:<user_id>` | SET | 7d 滑动 | 该用户全部 web 会话 sid（列举 / 一键吊销 / 会话数上限裁剪；成员过期靠读时 SREM） |
| `sess:meta:<sid>` | HASH | 7d 滑动 | 会话展示元数据：`ip` / `ua` / `created_at`（unix 秒，展示）/ `created_ms`（unix 毫秒，会话上限裁剪的排序键——同秒多次登录要分先后） |
| `oauth:state:<token>` | STRING | 10min | OAuth authorization-code 流 CSRF state（一次性，校验即删） |
| `spend:tm:{team}:{member}:<yyyymm>` | STRING | 40d | 团成员月度消费计数（结算后累加，预扣前比较；软实时限额） |
| `tok:{<uid>}:<yyyymm>` | STRING | 40d | 用户本月累计 token（`pricing_rules` volume 规则的 token 轴输入）。结算后累加实际 usage 总量、报价前读取，语义与团成员计数同构（软实时：跨月自然滚动、Redis 故障按 0 处理即不打折，宁少算不错算）。**仅当生效 PriceBook 含启用的 volume 规则时才产生读写**（`PriceBook::has_volume_rules`），无此类规则时热路径零额外 Redis 往返 |
| `usd:{<uid>}:<yyyymm>` | STRING | 40d | 用户本月累计消费 micro（volume 规则的**消费额轴**输入，服务"贵模型大客户用量少但付费多"）。与 tok 计数完全同构；门控独立为 `PriceBook::has_spend_rules`（含 min_monthly_spend_micro>0 的规则才读写），只用 token 阈值的站点不付此往返 |
| `pb:epoch` | STRING | 永久 | 【M3 接入】当前 PriceBook epoch。当前实现：gateway 每 30s 直接轮询 PG `MAX(epoch)`（单机/中小规模更简，见 §2.3） |
| `pb:data:<epoch>` | STRING(bin) | 保留 2 版 | 【M3 接入】编译后 PriceBook 快照（多副本大表分发 + PG 减负时启用） |
| `ch:cool:<channel_key_id>` | STRING | =冷却时长 | 状态机冷却镜像 |
| `ch:test:<channel_id>` | STRING(JSON) | 30d | 最近一次测活结果（ok/latency_ms/http_status/error_code/at），渠道列表"最近测试"列回填（IMPLEMENTATION §11.12）。提示性信息不进 PG，过期即消失 |
| `ch:balance:<channel_id>` | STRING(JSON) | 30d | 最近一次上游余额查询结果（probe/currency/balance_micro/at，IMPLEMENTATION §11.33），列表 `last_balance` 回填；与 `ch:test` 同一取舍 |
| `ch:stat:<channel_id>` | HASH | 5min | 错误率/TTFT EMA（打分输入） |
| `lock:cred:<channel_key_id>` | STRING NX | 90s | OAuth 刷新租约，值为随机持有者；释放时比较持有者。请求/后台/手动刷新共用；Redis 故障不无锁刷新，PG 按原密文字节条件回写 |
| `oauth:refresh:<channel_key_id>` | STRING JSON | 30 天 | 脱敏刷新观测：尝试/成功时间、连续失败数、下次重试、错误码。瞬态失败退避 30 秒至 15 分钟；不存任何凭证或上游错误原文 |
| `oauth:cred:<state>` | STRING(JSON) | 10min | 渠道 OAuth 登录流程的 PKCE verifier + provider + 发起管理员 ID + 可选 channel/key 目标；`start` 写，`exchange` 一次性读删并校验绑定 |
| `kpi:{kpi}:<req\|tok\|amt\|err>:<unix_s>` | STRING 计数 | 360s | 平台实时 KPI 秒桶（四序列各一键/秒；单 Lua 四路累加，读侧 MGET 整窗且跳过累加中的当前秒）。`{kpi}` hash-tag 同槽使跨秒 MGET 在 Cluster 下成立。弃初稿 ZSET 滑窗——按请求存成员的内存 ∝ 流量，秒桶与流量无关（IMPLEMENTATION §11.12）。写入挂 gateway `settle_write` 收口处，只计 log_type 2/5 |

`{<uid>}` 为 Redis Cluster hash-tag：同一用户的 余额/限速/并发 键同槽，保证 Lua 原子性与线性扩容（档位二关键，IMPLEMENTATION §12.1）。

### 2.2 余额热账本与 Lua 契约

`bal:{uid}` HASH 结构：`avail` = 钱包可用余额（micro）；`sub` = 订阅池当前窗剩余（micro，允许短暂为负：最后一笔可越界，下窗重置）、`sub_until` = 池可用截止 unix 秒（= min(window_end, expires_at)；缺省/0 = 无订阅）；`sub_epoch` 保存订阅实例 ID 与窗口起点微秒。每笔在途预扣一个字段 `r:<request_id>`：钱包或未采集周期的旧请求为 `"<reserved_micro>|<deadline_unix_ms>|<api_key_id>|<pool>"`，带周期订阅请求追加 `|w:<sub_epoch>`。deadline = 预扣时刻 + 10min；api_key_id 用于释放并发槽与终态补偿；pool 0 钱包 / 1 订阅，旧格式缺省 0。过期预扣由 commit/refund 正常清理，泄漏者由 reconciler 按 deadline 懒清理。

**选池规则（IMPLEMENTATION §11.28）**：reserve 时 `sub > 0 且 now < sub_until` → 订阅池（不校验足额，允许单笔越界），否则钱包（`avail >= est` fail-closed）。同一请求只动一个池；commit/refund 保留预扣的 pool 和周期，旧周期凭据关闭时不向当前周期补钱或扣款。订阅只改"谁付"，不改价——pricing_snapshot 与 DESIGN §3 公式对两池一致。

精度约束：Lua number 为 double，普通预扣要求金额、Token、正限额、余额绝对值及计数递增后的结果不超过 2^53−1（金额约 $90 亿）。负数预扣、非规范整数、异常 Redis 类型和越界计数均在资金写入前拒绝；cap≤0 表示不限额，但仍须验证会递增的计数器。向 Redis 传递大金额与 Token 增量时保留十进制字符串。M1 实现细节：Lua 脚本经 EVAL 全量下发（EVALSHA/Script 缓存 M2）；KPI 计数已随 §11.12 实时看板落地（`kpi:*` 秒桶，写入在结算旁路 fire-and-forget）。

```text
reserve ────────────────────────────────────────────────
KEYS = bal:{uid}, rl:{uid}:k:<kid>:rpm:<minute>, rl:{uid}:k:<kid>:tpm:<minute>, rl:{uid}:k:<kid>:rpd:<YYYYMMDD>, conc:{uid}:k:<kid>   （全部同槽）
ARGV = request_id, est_micro, deadline_ms, rpm_cap, tpm_cap, rpd_cap, conc_cap, est_tokens, api_key_id, now_unix_s
返回 = {1, balance_after, pool, source_window}     成功；source_window 为订阅周期，钱包/旧无周期为空字符串
       {0, "INSUFFICIENT", balance}                钱包余额不足（订阅池不可用或已耗尽时才到这一步）
       {0, "RATE_LIMITED", which}                  限速/并发超限（不产生任何写入）
       {0, "RESERVATION_EXISTS"}                  同 uid/request_id 的预扣记录已存在（零写入，Rust 返回 LedgerError::ReservationExists）
       {0, "INVALID_RESERVATION"}                 预扣参数非法或越界，Rust 返回 LedgerError::InvalidReservation
       {0, "ADMISSION_STATE_INVALID"}             计数/余额数据异常，Rust 返回 LedgerError::AdmissionStateInvalid
语义 = 首先 HEXISTS 检查 r:<request_id>；存在即拒绝，不延长 deadline、不重新占槽或计限速，旧格式/异常记录同样保留；
       其余检查全部通过后：选池（sub>0 且 now<sub_until → sub，否则 avail 且须 ≥ est）→ 池 -= est；
       HSET r:<request_id> = est|deadline|kid|pool（已知订阅周期追加 |w:epoch）；INCR 各计数器；INCR conc

commit ────────────────────────────────────────────────
KEYS = bal:{uid}, conc:{uid}:k:<kid>
ARGV = request_id, actual_micro, api_key_id, expected_pool, expected_epoch（后两项可选；持久结算使用）
返回 = {1, delta_micro, balance_after, pool, 1}    末项表示确实关闭；同周期 delta = reserved − actual；已换周期 delta = 0
       {0, "NO_RESERVATION"}                       调用方转对账路径（不直接改余额）
       {0, "INVALID_SETTLEMENT"}                  actual 非规范、负数或越界
       {0, "RESERVATION_CONFLICT"}                key、expected_pool 或 expected_epoch 与凭据不同
       {0, "SETTLEMENT_STATE_INVALID"}            凭证、余额、并发或计算结果非法
语义 = 校验凭证、key、金额、周期及全部将修改的状态 → 池 += delta → HDEL → conc>0 才 DECR；
       旧无周期凭据可接受请求保留的 expected_epoch，但它必须等于当前热账本周期；不猜测已过期归属；
       幂等：重复调用返回 NO_RESERVATION；并发键已过期时不创建负计数

refund ────────────────────────────────────────────────
KEYS = bal:{uid}, conc:{uid}:k:<kid>
ARGV = request_id, api_key_id
返回 = {1, released_micro, balance_after, pool, closed}；无预扣字段返回 {1, 0, avail, 0, 0}
       RESERVATION_CONFLICT / SETTLEMENT_STATE_INVALID 同 commit
语义 = 与 commit 共用写入前校验；同周期全退，已换周期只关闭凭据及并发槽（released=0, closed=1）

repair ────────────────────────────────────────────────
KEYS = bal:{uid}
ARGV = target_micro, pool_field("avail"|"sub")
返回 = {prev, next, inflight}                      next = target − Σ同池有效在途；已知不同周期的普通预扣不计入当前 sub

sub_set ───────────────────────────────────────────────
KEYS = bal:{uid}
ARGV = quota_micro, sub_until_unix_s, sub_epoch（旧调用默认空字符串）
返回 = {prev_sub, new_sub}                         new_sub = quota + Σ仍无周期身份的旧订阅 r:*；有周期普通预扣不带入新窗；
                                                   旧热账本有可靠 epoch 时补到旧凭据；长期 h:* 不加入此低层接口；
                                                   生产订阅变更使用 PG 事件与持久恢复，未知旧凭据先关闭再切窗
语义 = quota/deadline 非负；全部 r:* 格式、累加及新余额先校验，失败不改订阅额度、截止时间或窗口标识；
       金额写入和返回使用精确十进制字符串，不使用可能转成科学计数法的 tostring(number)
```

- KPI 与 `ch:stat` 更新不在 Lua 内（跨槽），走同连接 pipeline fire-and-forget——**账本原子、统计尽力**，统计口径最终以 CH 对账为准。
- `RESERVATION_EXISTS` 保护的是尚未终结的同步预扣，包括过期但尚待对账回收的记录；重复调用不会获得再次调用上游的许可。调用方需保留同一请求的执行状态，不能捕获该错误后改用新 UUID 重试生成。终态释放后同步脚本不保存 tombstone，request_id 仍必须全链路唯一；长期批任务使用下述独立冻结记录，仍须保存远端执行状态。
- Redis Lua 运行时错误不会撤销先前写入。普通预扣先验证四个会修改的计数器，再检查限额与选池；类型、规范整数或范围异常不会留下扣款、预扣凭证、计数增量或新 TTL。异常数据不自动清零，账本错误仍返回既有 HTTP 500 `internal_error`，不会被当作正常 429。这里的保证针对已校验的数据异常，不替代 Redis 断连/执行确认丢失时的原有对账机制，也不将脚本外的模型/分组限流纳入资金原子事务。
- 普通结算/退款在改钱、删除 r:* 之前验证所选余额、并发键及结果的 2^53−1 安全整数界限。凭证接受完整的两段旧格式（key=0、pool=0）、三段旧格式（pool=0）、四段格式，以及第五段为 `w:epoch` 的订阅格式。epoch 必须非空、至多 128 字节，仅含 ASCII 字母、数字及 `-:._`；空字段、未知 pool、非法整数及多余字段均拒绝，不猜测成钱包。key 作为规范十进制字符串比较，支持完整非负 PG bigint 身份，不受 Lua 浮点截断影响。
- 无凭证退款的 pool=0 是既有幂等返回约定，不代表原请求由钱包支付。Chat、Embeddings/Rerank 和 Realtime 保留 reserve 返回的池，用于退款错误或零释放结果时的失败账单归属；重复退款不得额外入账。普通成功请求现先在 PG 保存账单与待同步记录，再关闭 Redis 预扣，详见下文；该保证从 PG 提交成功后生效。`repair` 另对在途累加与最终减法检查 Lua 安全整数界限，超限拒绝写入。
- `conc:ck:*` acquire/release 为独立单键操作（与用户槽无关）。

普通持久结算由 `0016_billing_sync.sql` 新增 `billing_sync`：request_id 主键、用户/key、实际 micro 金额、预扣池与创建时间。成功账单、用量、事件、用户/key 累计、outbox 与待同步行同事务提交；PG 失败不关闭 Redis，Redis 失败保留待同步记录。worker 在过期清理之前恢复，且共用用户锁，防止订阅滚窗/余额修复与待同步实际费用交错。Redis 凭证已不存在时，根据 PG 事件重建两池，保留其他活跃预扣和长期冻结，不猜测再次扣款。管理员退款持有同一用户锁并先恢复待同步结算，恢复失败则拒绝继续。普通成功事件的 `balance_after_micro` 为 NULL，不能在 Redis 同步之前伪造同步后余额。见 [普通持久结算契约](synchronous-settlements.md)。

钱包入账与管理员退款由 `0017_fund_transfers.sql` 新增 `fund_transfers`：操作 UUID、用户、金额、来源池、创建/应用/清理时间。钱包订单 paid / 兑换码使用状态、账本事件、用户快照与待入账意图同一 PG 事务；退款状态、事件/用量回冲和待入账意图同事务。Redis `fund_transfer.lua` 在 `bal:{uid}` 内原子写入金额和 `c:<operation_id>` 凭据（`avail|<delta>` 或 `sub|<delta>`），先验规范整数和 ±(2^53−1) 界限。重复同凭据只确认；冲突/异常金额拒绝且不写。PG 记录 applied_at 后才删除 Redis 凭据，PG cleaned_at 让中断清理可重试。0018 为操作增加 sequence；Redis fund_seq 与余额同一次 HSET 写入且不随凭据清理，旧序号的迟到 EVAL 不重复追加。序号按规范十进制字符串比较，支持完整 PG bigint。余额重建与全部已接受操作的最高序号一次写入，包含已清理操作；fund_seq 不得单独删除或过期。worker 在过期预扣前恢复未完成资金。待应用的 HTTP 受理结果包含 `pending:true`、操作 ID 与空 balance_after_micro；不把数据库快照冒充当前可用余额。正常返回保留原余额数值。见 [钱包入账与退款持久恢复](durable-fund-transfers.md) 的范围与剩余边界。

管理充值、MCP 调整、兑换、支付/返利、注册赠送、单用户引导与迁移现通过 `ledger::operations` 入账，与普通结算恢复、订阅变更和对账修复共用用户锁。PG 事件和用户快照事务先校验再触碰 Redis；用户更新 0 行视为不存在，回滚事件并返回 `not_found`，不创建无归属余额。按日志退款的锁覆盖 PG 幂等翻转和原池 Redis 回补；余额到期在锁内重读有效期并锁住用户行。用户锁本身不是跨存储原子提交；钱包订单/核销、管理员退款及订阅发放已使用本节的 PG 持久意图补偿。邀请奖励/注册业务状态到入账意图的衔接仍需专项验证；钱包到期已使用 PG 事件与 fund_transfers 同事务接受后再更新热余额，不能据这些锁或队列宣称全部业务原子化。

长期冻结由 `0008_balance_holds.sql`、`0009_balance_hold_cancellation.sql` 的 `balance_holds` 表记录：UUID、用户/key、模型/请求摘要、最大金额/价格快照、pending/held/closing/closed、来源池/订阅窗口、实际金额/退款额/结算凭证、取消标记。未关闭记录每用户最多 128 条；价格与金额来源在确认后不可变。完整算法、取消与恢复边界见 [长期冻结契约](durable-balance-holds.md)。

原生图片批任务由 `0010_image_batches.sql` 增加四表：`image_batches` 保存归属、固定渠道账号与价格、提交意图/远端身份、租约及状态；`image_batch_payloads` 独立保存私有输入/连接快照/上传会话；`image_batch_items` 保存调用方条目 ID；`image_batch_outputs` 预分配每张图片的唯一输出槽位。单任务最多 200 条目/200 输出、每条目 1–4 输出，输入 128 MiB、每图 16 MiB；原子容量准入为全部输出预留预算。元数据列表用服务端游标分页，大小 1–100；图片仅在同用户/key、结算关闭、终态且未过期时可读取。创建事务同时写入 pending balance_hold，独立 key 预算包含全部未关闭资金意图。公开路由默认每页 20 条，创建由 image_batches_enabled 控制且默认关闭。七天期限和逻辑删除立即限制读取，并触发后台回收，见 [批任务存储契约](native-image-batch-jobs.md)。

`0011_image_batch_recovery.sql` 增加私有分页检查点表 `image_batch_recovery`，以 batch_id 为唯一归属保存候选、下一页、页数、游标摘要、完成/冲突标志。每次变更和恢复接管都核对当前执行租约和 uncertain 状态；完整扫描、唯一候选与详情身份核对才能记录远端任务。列表未找到或详情 404 不关闭资金冻结。重扫不删除已有候选或冲突，避免忘掉重复任务证据。

`0012_image_batch_cleanup.sql` 增加 `image_batch_cleanup`，保存终态任务的远端删除检查点与独立重试错误。清理租约必须满足已删除/到期及匹配的 closed hold；远端 job 和文件清理完成后，事务删除私有 payload、条目/图片、恢复与清理检查点，将 cleanup_done 置真并把存储预算降为每任务 512 KiB。产物任务数与运行字节预算均排除已清理记录（0031 的 live capacity 部分索引）；保留元数据不占运行准入预算。账单、资金凭证、请求/幂等身份不删除，原幂等键不能被用来再次生成。

| Lua | 同槽 keys | 参数与结果 | 资金语义 |
| --- | --- | --- | --- |
| `hold_reserve` | `bal:{uid}`、`hold:{uid}:UUID` | UUID、最大金额、key、摘要、时间、可选 PG 窗口；返回凭证或固定错误 JSON | 必须已有 PG pending；原子扣可用余额并保存两份 held 凭证；相同凭证重放不扣款 |
| `hold_seal` | 同上 | UUID、最大金额、key、摘要；返回已有 held 或零退款 closed 凭证 | PG 先标记 pending 取消；未扣款则写永久拦截凭证，已扣款则交给结算退回，不直接退款 |
| `hold_close` | 同上 | UUID、已提交 PG 的关闭凭证；返回同一凭证 | 退还固定 credit，删除活跃 h:*，保留关闭凭证；重复不动钱，来源窗口不符拒绝 |
| `hold_repair` | 余额与该用户各 hold key | 两池事件总额、PG 清单、窗口、待资金操作、最高序号、预期余额 hash 指纹；返回 wallet/sub 的 before/after/inflight（整数字符串） | 指纹不一致返回 recovery_required 且不写；全部校验后重建两池与持久凭证，保留同步 r:*，拒绝未知/矛盾冻结 |

worker 对账计算两池的 `可用 + 有效 r:* + h:*`：订阅 r:* 仅计当前周期及无身份的旧兼容凭据，已知其他周期的不计入当前余额。清扫 deadline 只处理 r:*，以 `closed` 而非释放金额是否为零判断是否实际关闭。worker 修复在用户锁内读取事件总额并使用 `hold_repair`；旧的单池 `repair` 也会扣除本池活跃 h:*，但不能恢复已丢失的持久凭证，不应作为长期冻结的完整修复入口。hold 结算共用原有 PG 账单/四金额/outbox，reserve 审计 delta 为零；跨订阅窗口的未用冻结额使用已有 `sub_expire` 事件，不新增事件类型或改变消费统计公式。


`0019_billing_retention.sql` 新增历史事件结转、历史财务凭证，以及 `billing_balance_totals` / `billing_actor_totals` / `billing_financial_records` 三个视图。财务真理源是当前记录与结转的合计；对账、余额修复、缺失凭据恢复、导入防重、历史退款及累计邀请奖励均读取合并历史。分区结转与 DROP 原子提交，财务读事务共享锁与清理排他锁互斥，财务凭证 ID 冲突或依赖对象导致该分区清理回滚。只清理真实归属和实际月边界均匹配的分区，不使用 CASCADE。保留期内明细接口与 CH TTL 保持独立。详细约束及无法恢复此前已删数据的边界见 [历史账本结转](billing-retention.md)。

`0020_subscription_snapshots.sql` 固定购买订单、兑换码和实例的套餐条款，实例取消/滚窗不再读取当前目录的周期或分组。`0021_subscription_grants.sql` 保存唯一 `(user_id,source)` 的持久发放回执；paid/核销状态与发放意图同事务，兑现时订阅、分组、财务事件、回执与 `subscription_sync` 标记同事务。受理后余额不可用返回 pending，不伪造可用额度；worker 按用户顺序每次至多处理 32 条。管理员发放支持 Idempotency-Key，门户/管理接口的待发放列表固定每页 20 条、游标分页。迁移对旧条款仅能使用升级时的配置，旧已付/已核销遗留权益不自动重放，见 [订阅持久发放契约](durable-subscriptions.md)。

生产订阅激活/滚窗/结束先在用户锁内完成未结算同步，再按 `新配额 + 活跃长期冻结额` 计算 PG 第二池目标，以与完整历史总额的差值记事件。普通预扣不带入新周期；尚无周期身份的旧订阅预扣必须先关闭，否则返回待恢复错误。Redis 从当前权威两池总额扣除当前周期普通预扣和长期冻结重建可用额；PG 已确认的长期冻结即使 hash 丢失也保留。`hold_repair` 的 ARGV 第 8 项是读取清单前取得的完整余额 hash 指纹，Lua 写入前校验，避免迟到修复覆盖新的扣费/预扣/入账。新的订阅财务事件不提前猜测 `balance_after_micro`，使用 NULL；没有新增事件类型或变更消费四金额公式。

`0024_billing_source_window.sql` 为详细账单、历史财务凭据和 `billing_sync` 增加 nullable `source_window`。普通请求从准入至迟到结算保留原周期；旧周期成功账单的 `commit=-actual` 与 `sub_expire=+actual` 在同一用户锁、同一 PG 事务内记录，后者修订旧额度的过期额，两者不改变新周期资金。管理员退款在原周期已过期、取消或替换时以 `refund=+amount` 与 `sub_expire=-amount` 原子冲销，`credited_micro=0`；仅原周期仍有效时恢复可用额度。归档保留身份，NULL 历史数据不伪造周期。墙上时间已到而正式维护尚未滚窗时，记录的周期身份保持稳定，准入另查截止时间。详细规则与兼容限制见 [普通请求的订阅周期归属](subscription-window-accounting.md)。

`0022_subscription_retry.sql` 为订阅实例添加 `maintenance_retry_after`，为 `subscription_grants`、`subscription_sync` 添加 `retry_after`，均为 nullable timestamp，升级时不改变已有任务的可选状态。worker 在用户锁内重读到期/窗口，单用户故障延期 60 秒并继续本批其他用户。延期核对旧实例边界，已完成的续期或滚窗不会被旧扫描误取消/反复注资；结束订阅会解除待发放来源的退避。原币报价以 i128 中间值精确换算并在写入前检查 NUMERIC(12,2) 的上限，见 [订阅持久发放契约](durable-subscriptions.md)。

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
    cache_write_tokens Nullable(UInt32) DEFAULT NULL, -- 未采集历史为 NULL；已采集且未写缓存为 0
    completion_tokens UInt32, reasoning_tokens UInt32,
    media_units       String,                 -- JSON
    server_tool_usage String DEFAULT '',      -- 带 provider 的工具计数 JSON；空串=历史未采集
    -- 金额（售价/原价/优惠/成本 四列 = 毛利与让利分析，new-api 均缺失）
    amount_micro          Int64,
    original_amount_micro Int64,
    discount_micro        Int64,
    upstream_cost_micro   Int64,
    pricing_epoch         UInt64,
    ratio_snapshot        String,             -- 完整价格快照 JSON；含实际缓存/模态价轴与交叉计数
    -- 性能
    latency_ms UInt32, ttft_ms UInt32, stream UInt8,
    ttft_reported Nullable(UInt8) DEFAULT NULL, -- 新行 1=已测量（含 0ms）/0=未测量；NULL=旧行
    latency_reported Nullable(UInt8) DEFAULT NULL, -- 总耗时采用同样的采集语义
    diagnostics String DEFAULT '',           -- 有界错误摘要、模型观察及渠道尝试 JSON；空串为历史未采集
    billing_status Nullable(UInt8) DEFAULT NULL, -- 20结算/30退款/40失败；与请求 is_error 独立
    -- 调度（Sub2API 启发）
    retry_count UInt8, failover_count UInt8, sticky_layer UInt8,
    upstream_status UInt16, error_code LowCardinality(String), is_error UInt8,
    pool UInt8 DEFAULT 0                      -- 0 钱包 1 订阅池（ch_schema.sql 增量 ALTER）
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(ts)
ORDER BY (user_id, ts)
TTL toDateTime(ts) + INTERVAL 180 DAY;        -- 保留期后台可配（#1790-1）
```

`billing_records.usage_details.tokens.server_tool_usage` 与 outbox 同名字段保存同一原生工具
观察。当前 provider 为 `anthropic`，包含可空/缺省的 `web_search_requests` 和
`web_fetch_requests`；缺省未知、0 明确已观察，均不是 Token。CH 使用有界结构 JSON
字符串，旧事件/旧行为空串，不回填零。迁移 0033 给 `model_pricing` 增加可空 JSONB
`server_tool_prices`；随 `pricing_epochs.snapshot.models` 发布，不读取未发布价格。
`pricing_snapshot.server_tool_fees` 保存逐工具 request 数量、用量契约、额外/已包含
策略、整数 micro 单价及 list_price/original/amount/discount 分量。未知数量或未配置
价格对应 null，明确免费是 additional 的 0 单价，已包含费用不重复计费。2026-10-07 起，请求声明了
原生工具而模型没有 `server_tool_prices` 时准入即拒（400 `server_tool_unpriced`），与模型未定价同口径，
兜底链里没给工具定价的备选模型被跳过；未配置价格的 null 只剩历史数据。结算阶段按响应模型计价的规则不变。
四金额合计沿现有 PG/outbox/CH 字段传递，退款按已结算合计退款。有未定价非零/未知
工具数量或渠道成本估算溢出时，PG upstream_cost 为 NULL、
事件/CH upstream_cost_known 为 false；不截成最大整数冒充真实成本。长期工具聚合仍待
实现，Token 指标不包含工具次数。退费记录不制造新的工具
调用，明细 API 的 provider 标签表示原用量契约，而不是入口方言。

`tokens.server_tool_usage.code_execution_requests` 是可空的 Anthropic 原生执行
请求次数；仅采集官方 usage 字段，不能由函数名、结果块数或容器 id 推导。
同一对象经过 PG、outbox 与 CH `server_tool_usage` 字符串传递，不新增 Token。
代码执行的容器时长/费用尚未配置时，明确声明或非零原生观察产生
`pricing_snapshot.server_tool_cost_coverage={version:1,source:"anthropic_native_tool_usage",
complete:false,reason:"container_duration_price_contract_unavailable",provider:"anthropic",
tool:"code_execution",billing_unit:"container_duration",requested:bool,observed_requests:u32|null}`。
此标记和现有未定价 search/fetch 分量均使整体成本未知；零数值载荷配合
outbox/CH `upstream_cost_known=false`，PG `upstream_cost_micro=NULL`，不得显示为已知免费。
退款不复制原工具实测，原快照标记仍保留；重放不查询当前价格补全容器费用。
所有成功结算的统一准备步骤均检查非零代码执行观察；缺标记时补以上信息，
不完整工具契约使调用方预填的 upstream_cost 也保持 NULL，不能依赖早返回跳过。

### 3.2 MV 矩阵（AggregatingMergeTree）

历史 speech 校准新增 `legacy_speech_units_v1`（`ReplacingMergeTree(copies)`，无 TTL）：按原 ts/request_id/用户/密钥/分组/模型/渠道/高级维度、字符数量和报价版本保存已确认的旧字符证据。`copies` 为同一证据身份在原明细中实际出现的数量，以最大已确认数量作替换版本；查询必须 FINAL 后再求和，重跑不会叠加。保留旧快照和识别 basis，仅用于排除原主 Token 中的字符，不更新财务金额或旧主 MV。`legacy_speech_calibration_v1`（`ReplacingMergeTree(version)`，单槽、无 TTL）保存 `(cursor_ts,cursor_id)`、完成状态与单调版本。worker 每批最多 500 个请求身份，先确认并写证据，再写进度；失败不前进，重复执行幂等。迟到的旧 outbox 通过同一识别器在投递 CH 前归一化，不依赖已经完成的历史游标。归一化行使用 `input_unit=characters`、`input_characters=N`、`prompt_tokens=0`，raw 的 `historical_prompt_units=N` 和 `input_unit_basis=legacy_speech_contract_v1` 保留原载体及依据；原报价快照、版本与四金额保持不变。显式新单位、模态报告/上游 Token 或其他冲突不归一化。实施/验证状态见 [历史 speech 校准](historical-speech-unit-audit.md)。

缓存/模态交叉计费（0025）继续沿用既有总量列：`cached_tokens` 包括图片/音频缓存，
输入/输出总量不重复叠加子集。交叉明细及实际价轴落在 PG `pricing_snapshot` 和
CH `ratio_snapshot` 同一 JSON 中；直接 Images 另有 `image_cache_usage` 图文读写拆分。
后续独立 `mv_token_details_5min` 已增加九轴观察聚合；快照明细本身仍不能等同于
完整的模态缓存分析面板。完整范围与观察子集/覆盖率见下述长期细分契约。模型配置 `modality_ratios` 存十进制字符串，
账单快照的有效价格存精确 JSON 数字，二者均不经过浮点。

模型目录配置（0027）：`catalog_config` 必须为 JSON 对象，类型、说明和输入/输出模态分别存储；
视觉、工具调用、并行工具、结构化输出、推理等能力仍在 `models.capabilities` 保存布尔值。
缺键表示未声明，`false` 表示明确不支持，不由模型名推断。目录声明与实际渠道协议/接口可用性分离，
不会注入上游参数或自动启用路由。管理端模型、价格、服务档位和降级链在同一事务保存；旧调用方省略
`metadata` / `pricing_mode` / `tier_ratios` / `modality_ratios` 时保留已有配置，空对象显式清除独立价格或档位。

缓存时长价格使用 `modality_ratios.cache_write_5m` / `cache_write_1h`（十进制字符串）：
二者相对于普通文本输入单价，替换通用写入倍率，不再次叠乘。上游的 5m/1h 明细必须完整且和总写入量相等；
没有明细时按通用写入倍率计费，明确的零倍率合法。有效时长价格随账单快照保存，并在账单参考分项中分开展示。
若缓存模态和时长是重叠边际且缺少联合分配，独立时长价格与通用价格不同时拒绝不明确计算，不能猜测交叉数量。
本配置是缓存创建价格，不会设置缓存 TTL；自定义时长和存储时间费仍需各协议的计量支持。

**两档 TTL 长期观察契约（2026-10-01，实施验证中）：** 新增独立无 TTL 的
`mv_cache_ttl_5min`，维度与 `mv_token_details_5min` 相同。记录全部调用的
`countState`，以及 5 分钟、1 小时各自的数值和与样本数。只有写入已采集、
两档均非空且两档之和等于总写入量时才算有效样本；明确的零为有效值，
缺失/矛盾/单档值不补零。退款是财务事件，不增加调用或 TTL 样本。
该聚合接入既有可信请求分类和完整维度覆盖探针，与九轴细分聚合独立选源，
不能因新增 TTL 聚合不完整而丢弃已保留的九轴历史。

管理/个人统计及两类日志的 `token_detail_observations` 增加
`cache_write_5m_tokens` / `cache_write_1h_tokens`：完整范围才返回 `tokens`，
另提供 `observed_tokens` / `observed_records` / `coverage_bp` / `complete`。
看板另提供 `cache_write_ttl_history`，区分已保留的请求历史覆盖和 TTL 采集覆盖。
按每个实际查询粒度选择完整聚合或 raw，缺口只选一个可证明的子集，不相加
重叠来源、不将整日 TTL 数量分配给未知小时/渠道。无 POPULATE，不恢复已删除的
旧 TTL 细分；若 raw 仍完整可以恢复旧查询。新增的是观察能力，价格公式、四金额
和账单的 TTL 字段不变。验收记录见 [TTL 长期统计核对](cache-ttl-retention-audit.md)。

| MV | 主键 | 服务场景 |
| --- | --- | --- |
| mv_user_day | (user_id, day) | 用户概览、消耗趋势、本月节省、排行榜 |
| mv_apikey_day | (api_key_id, day) | 按 key 统计（#4971） |
| mv_model_hour | (model, hour) | Top 模型、模型速度 |
| mv_group_day | (group_code, day) | 分组经营 |
| mv_channel_5min | (channel_id, ts5) | 渠道请求、错误率、费用、切换率、粘性命中率；旧 TTFT 状态保留但不再作为分位数读源 |
| mv_model_ttft_hour | (model, hour) | 模型有效 TTFT 分位数、有效样本数和请求覆盖数 |
| mv_channel_ttft_5min | (channel_id, ts5) | 渠道有效 TTFT 分位数及 5 分钟时间线、有效样本数和请求覆盖数 |
| mv_ttft_reporting_hour | 与 mv_analysis_hour 相同的小时及完整维度 | 平均 TTFT 的有效毫秒总和、样本数与请求覆盖数；明确 0ms 有效，非流式排除 |
| mv_latency_reporting_hour | 与 mv_analysis_hour 相同的小时及完整维度 | 已采集总耗时、同批请求的输出 Token、样本数和请求覆盖数 |
| mv_usage_sources_5min | 五分钟及 mv_analysis_hour 的完整维度 | 输入/输出的实报、估算、复核请求数与结算 Token；同批实报输入/缓存读取、来源覆盖。无 TTL，支持按小时/日及渠道时间线重组 |
| mv_token_details_5min | 五分钟及 mv_analysis_hour 的完整维度 | 九个音频/图片/缓存交集/推理轴的已观察数值和、样本数及请求覆盖。无 TTL；同粒度完整聚合优先，升级缺口择一恢复 raw，不拼加重叠来源。见长期细分核对。 |
| mv_cache_totals_5min | 五分钟及 mv_analysis_hour 的完整维度 | 缓存写入保存值、明确读写采集数及请求覆盖，无 TTL。与 raw/可唯一分配的旧日状态择一，数量和样本同范围，见 [缓存聚合核对](cache-write-aggregate-audit.md)。 |
| mv_input_units_5min | 五分钟及 mv_analysis_hour 的完整维度 | 请求覆盖、有效字符数量/请求及明确 Token 单位请求，无 TTL；与 raw 按粒度覆盖择一，零/未知/子集分开，见 [字符单位核对](character-unit-audit.md)。 |
| mv_output_rate_5min | 五分钟及 mv_analysis_hour 的完整维度 | 全部请求/单位覆盖、明确 Token 请求与配对测时输出/耗时/样本，无 TTL；速度排除字符并暴露未知单位。实施与回归进度见 [输出速度单位核对](output-rate-unit-audit.md)。 |
| mv_model_latency_hour / mv_channel_latency_5min | (model, hour) / (channel_id, ts5) | 有效总耗时分位数、均值与覆盖数；包括字符、非流式及失败请求。Token 速度读取独立 mv_output_rate_5min |
| mv_user_model_day | (user_id, model, day) | 用户下钻 |
| mv_error_hour | (error_code, hour, channel_id, model) | 错误码分布（IMPLEMENTATION §11.12）。MV 内 `WHERE is_error = 1` 插入期过滤，行数 ∝ 错误码×小时×渠道×模型，与总请求量无关 |
| mv_client_day | (client_type, day) | 客户端类型分布（#5277）。含 `uniqState(user_id)`——"多少用户在用 Claude Code"比请求数更能说明生态渗透 |
| mv_key_model_day | (user_id, api_key_id, model, day) | 用户门户看板单一数据源（IMPLEMENTATION §11.12）：token 四轴（prompt/cached/completion/reasoning）+ amount/discount/errors。主键前缀使 key 视角 `(user_id, api_key_id)` 与 user 视角 `(user_id)` 都是前缀扫描；行数 ∝ 活跃 key × 当日模型数 |
| mv_cache_write_day | (user_id, api_key_id, model, day) | 缓存写入附加聚合：`write_tokens` 求和状态与 `known_requests` 已知样本计数。门户按日／模型与用量主表对齐，只有已知样本数等于请求数才返回缓存写入总数，否则为 null。 |
| mv_cube_hour | (hour, user_id, api_key_id, group_code, model, channel_id) | **分析立方体**（IMPLEMENTATION §11.13）：管理端"带任意维度过滤的趋势 / 拆分 / 流向"三个端点的历史基础聚合，与新增 `mv_analysis_hour` 的覆盖残差合并。上面各单维 MV 各答一个固定问题；这张答"过滤到某用户/某渠道/某模型之后，按另一个维度怎么分、随时间怎么走"（new-api #7150 与 Sub2API `TrendParams` 的诉求；new-api 的 quota_data 八维表同一思路）。列：requests、token 四轴、amount/discount/upstream_cost、errors、latency_sum、ttft_sum/ttft_samples（avg = sum/n；**不放 quantilesState**——每行一个 sketch 在这种基数下代价过高，分位数走 mv_model_ttft_hour / mv_channel_ttft_5min）。行数 ∝ 每小时出现过的五元组合数（上界为请求数，实际压缩极大）；主键以 hour 开头让时间窗裁剪先生效。provider 是 channel_id 的函数，查询时由 PG 回填，不进键 |

**TTFT 分位数升级（2026-09-28）：** `ensure_schema` 增加 `ttft_reported` 并创建两个独立条件聚合 MV，不删除或重写原有金额、请求和 Token 状态。有效样本条件为 `stream=1 AND ifNull(ttft_reported, toUInt8(ttft_ms>0))=1`；旧零值视为未采集，新显式零值参与分位数。使用 `quantilesIfState/Merge`，没有有效样本返回 `null`。

三个质量接口（模型、渠道、渠道时间线）先读原有请求总量，再核对新 TTFT MV 的请求覆盖；数量一致才使用新聚合。存在升级缺口时，在相同时间窗内只为展示中的实体从 raw 重新计算整个范围，绝不把 raw 和 MV 相加。raw 请求数也不完整时，保留原请求、Token 和金额，分位数返回 `null`。`ttft_samples` 是所选数据源的有效样本数；`ttft_observed_requests` / `ttft_history_coverage_bp` / `ttft_history_complete` 表示历史请求覆盖，不表示每笔请求都测得了 TTFT；`ttft_source` 为 `aggregate`、`raw` 或 `incomplete`。查询期间持续入库导致覆盖数暂时不一致也按不完整处理。历史恢复是有超时/内存护栏的只读明细扫描，未做大规模性能验证。平均 TTFT 已另用 `mv_ttft_reporting_hour` 条件和/计数状态，不再读取旧立方体的无条件和及正值计数。管理趋势、拆分、堆叠与个人用量/活动按样本数加权，沿用同一覆盖字段；缺口仅在限定时间、用户及粒度内择一恢复 raw，覆盖不足返回 null。旧维度残差只有在整体和已拆分部分都完整时才可相减恢复首字统计。详见 [平均首字核对](ttft-average-accounting.md)。

**字符单位分离（2026-09-30）：** PG `usage_details.input_unit/input_characters`、`pricing_snapshot` 和 outbox 同源保存；字符报价的使用详情不再将字符塞进 TokenUsage。CH 增量增加 `input_unit LowCardinality(String) DEFAULT ''`、`input_characters Nullable(UInt32) DEFAULT NULL`。独立 `mv_input_units_5min` 无 raw TTL，保留总请求、有效字符数量/请求和明确 Token 单位请求；非法字符与 Token 并存不算已知单位。raw 与 MV 按同粒度覆盖择一，不相加或 POPULATE。`input_units` 提供有效字符小计、单位覆盖和未知请求；完整覆盖时才返回全量字符，明确零仍保留。空单位表示历史未记录，字符 null 不等于零；历史归类不能猜模型名称。历史混合主 Token 校准仍待核对，见 [字符单位核对](character-unit-audit.md)；速度分母已按下述独立单位聚合接入，本段不证明整体历史统计已正确。

**Token 速度独立单位聚合（2026-09-30）：** `mv_output_rate_5min` 按完整 15 维保存 `requests=countState()`、`known_units/countIfState`、`token_requests/countIfState`、`samples/countIfState`、`total_ms/sumIfState(UInt64)` 与 `output_tokens/sumIfState(UInt64)`。明确 Token 条件为 `input_unit='tokens' AND isNull(input_characters)`；配对再要求 `ifNull(latency_reported,toUInt8(latency_ms>0))=1`。合法字符请求计入已知单位而不计速度配对；字符与主要 Token 轴并存不算已知单位。无 TTL、无 POPULATE 或自动历史回填，先由 worker 应用幂等 schema，再切换控制台。

速度 = 配对输出 / 配对毫秒 × 1,000,000，兼容两种 milli 字段；`performance_completion_tokens` 仅为这批 Token 配对输出，不能再用总耗时的 `latency_sum_ms` 重算速度。`output_tps_history_*` 是全部请求历史覆盖，`output_tps_unit_coverage_bp/unit_complete` 是单位覆盖，`output_tps_samples/sample_coverage_bp` 是测时采集覆盖（分母为明确 Token 请求数）。历史或单位覆盖不全，全范围速度为 null，子集速度 `observed_output_tps_milli` 单列；无 Token 样本或配对毫秒总和为零也返回 null，明确输出零且正耗时返回 0。均值/分位数仍使用全部有效测时请求，与速度分母独立。

管理分析的七类观察聚合逐粒度探测覆盖；完整新 MV 优先，缺口与完整 raw 择一，不叠加重叠数据。旧维度残差仅在两侧请求覆盖完整且差值非负时计算。个人日/活动/图表、实体、质量与日志用相同语义；PG 配对仅纳入状态 20/30/40 且 `latency_ms>=0`，pending 排除。原财务金额和主 Token 总量不由此改写。最终验证记录及历史限制见 [输出速度单位核对](output-rate-unit-audit.md)。

**Token 来源（2026-09-28）：** PG `usage_details.tokens.upstream_usage` 与 outbox 保存原始输入/输出计数，外层缺失表示旧数据或未记录来源，内层 null 表示估算轴。PG 明细与 outbox 同时保存 `prompt_source` / `completion_source`：`upstream`、`estimated`、`local_override`、`unknown`。CH 增量增加同名来源列（默认 unknown）及 nullable `upstream_prompt_tokens` / `upstream_completion_tokens`。来源对象纳入原有结算回执，未记录来源时省略该字段以保持旧回执形状；已记来源是幂等比对的一部分，不得在重放时篡改。详见 [来源与部分用量](token-usage-provenance.md)。

**细分采集状态（2026-09-29）：** `usage_details.tokens.reported_details` 及 outbox 保留输入/输出/缓存 audio、image 与 reasoning 的采集状态，CH 对应 `audio_prompt_reported`、`image_prompt_reported`、`audio_completion_reported`、`image_completion_reported`、`cache_read_audio_reported`、`cache_read_image_reported`、`cache_write_audio_reported`、`cache_write_image_reported`、`reasoning_reported` 为 `Nullable(UInt8)`，NULL 表示历史未知，0 未采集、1 已采集。两种日志明细返回同一对象，日志汇总增加 `token_detail_observations`，全范围读数仅在覆盖完整时返回数字，子集总量与覆盖率单列；旧 `*_samples` 是数值保存记录数，不能作为供应商采集数。旧记录未知，明确零已采集，缺失占位零未采集；标记不改计费公式或金额。协议转换与累计合并必须保留该区别，详见 [细分采集核对](token-breakdown-observation-audit.md)。

**长期细分：** 独立 `mv_token_details_5min` 保存每个观察轴的 `sumIfState(UInt64)`、`countIfState` 及所有请求的 `countState`，完整维度与 `mv_usage_sources_5min` 一致。无 TTL、无历史 POPULATE。管理/个人统计的 `token_detail_observations` 沿用日志语义，另附历史覆盖对象；已过期旧明细不能补成观察零，旧维度残差不得在覆盖不全时相减。设计与实际验证见 [长期 Token 细分核对](token-detail-aggregate-audit.md)；随后发现的缓存写入历史残差漏量已在 [缓存聚合核对](cache-write-aggregate-audit.md) 阶段修正。

**缓存量与覆盖（2026-09-29）：** `mv_cache_totals_5min` 独立保存请求数、写入数量、写入样本（标记为 1 且数字非空）和读取样本；明确零仍是有效样本。覆盖探针逐粒度检查五种观察聚合，完整时走聚合，缺口择一恢复 raw。旧日写入数量来自 `mv_cache_write_day`，采集标记来自 `mv_cache_reporting_day`；父请求必须完整且只有唯一子粒度才可分配，禁止把整日量平分或贴到任意小时/渠道。无法分配的细项保持未知，完整总计和厂商按各自范围重新恢复；新旧残差的读写数量与计数分别满足可减条件后派生。高级维度只读实际已采集范围，字符串筛选仍绑定。详细失败、回归和未验收的性能边界见 [缓存聚合核对](cache-write-aggregate-audit.md)。

**Token 来源聚合（2026-09-29）：** 新增独立五分钟 MV，不 POPULATE、不改旧金额/Token 视图。查询在同一粒度的 MV 与 raw 中择一使用，禁止叠加；历史不足的剩余请求及 Token 保持 unknown。`token_provenance` 的输入/输出分别返回四类请求数、结算 Token 数及各自基点占比，另列历史覆盖，空范围占比为 null。实报须有原始计数且等于结算计数；未知/错误来源字符串不算实报。主 `cache_hit_bp` 仅在全部请求都有实报输入和明确缓存读取时返回，使用同批计数加权；`measured_cache_hit_bp` 表示实报子集并附请求数/覆盖率。原结算分母的比率另列 `settled_cache_hit_bp`，不能解释为实测命中率。缺少输入/输出历史总量的接口在不能恢复时返回对应 unknown Token 为 null，不补造轴拆分。本地实际结果见 [聚合来源核对](token-source-aggregate-audit.md)，不能代替真实供应商账单与生产规模性能验收。

**总耗时与速度升级（2026-09-28）：** `latency_reported` 区分已测量零与缺失，新条件聚合不改旧账单总量。均值按已采集样本向下取整；输出速度只使用同批样本的 `performance_completion_tokens` 和 `latency_sum_ms`，而非把所有输出与部分耗时混算。正耗时总和为零时速度返回 null。历史完整性和采集比例分别由 `latency_history_*`、`latency_samples` / `latency_sample_coverage_bp` 表示。不能恢复完整历史时均值、速度及分位数均返回 null；原请求、Token、费用仍保留。PG 个人日志均值纳入已测得耗时的失败记录，与 CH 使用同一向下取整口径，待结算记录排除。详见 [总耗时核对](latency-statistics-audit.md)。

上述 2026-09-28 的速度分母已由 2026-09-30 的独立单位聚合替代；总耗时均值/分位数继续使用原耗时聚合。历史记录保留该阶段证据，当前速度契约以本节的 Token 速度独立单位聚合为准。


**MV 只向前聚合**：`ensure_schema` 的 `CREATE ... IF NOT EXISTS` 对已存在的库只新建缺失的 MV，且 MV 只捕获创建之后写入 raw 的行。新增 MV 后若需历史数据，手动回填一次即可（AggregatingMergeTree 接受 `-State` 插入）：

高级分析升级同样由 `ensure_schema` 添加 raw 字段并创建 `mv_analysis_hour`：请求模型、上游模型、双向规范化端点、调用类型、计价方式、成本已知标记和实际入库时间。新 MV 在原六个键上增加这些维度及 node/stream，保存基础度量、缓存写入已知样本、成本已知样本及对应收入/成本、事件/入库最大时间。管理分析查询将新聚合与旧 `mv_cube_hour` 减去已覆盖部分的残差合并，残差新维度为空、stream=2（未知）；不回填、不双算。已有的非零历史成本仍保留在上游成本合计中，但缺少已知标记的历史记录不能被用于全量毛利断言。先启动新 worker 应用幂等 schema，再切换新控制台；新网关产生的新字段随后自然入库。

缓存写入升级由 `ensure_schema` 先执行 `ADD COLUMN IF NOT EXISTS`，再建 `mv_cache_write_day`。先升级／启动 worker 完成 schema 初始化，再让新控制台读取新统计字段。新网关的结算 outbox 携带 `cache_write_tokens`，旧 outbox 缺字段时 chsink 写 `NULL`。旧链路未采集的缓存写入无法补算，不能用 0 或已知部分和覆盖整段历史。门户的耗时与 TTFT 附加指标读取已有 `mv_cube_hour`，也按样本覆盖完整性返回值或 `null`。完整口径及差异见 [图表对照](chart-parity.md)。

```sql
INSERT INTO mv_key_model_day
SELECT user_id, api_key_id, model, toDate(ts) AS day, countState(), sumState(toUInt64(prompt_tokens)),
       sumState(toUInt64(cached_tokens)), sumState(toUInt64(completion_tokens)), sumState(toUInt64(reasoning_tokens)),
       sumState(amount_micro), sumState(discount_micro), sumState(toUInt64(is_error))
FROM request_log_raw WHERE ts < '<MV 创建时刻>' GROUP BY user_id, api_key_id, model, day;
```

时间上界必须取 MV 创建时刻，否则创建后写入的行会被算两遍。开发环境直接 `scripts/dev-reset.sh`。

`mv_cube_hour` 同法（创建时刻取 `SELECT metadata_modification_time FROM system.tables WHERE name = 'mv_cube_hour'`）：

```sql
INSERT INTO mv_cube_hour
SELECT toStartOfHour(ts) AS hour, user_id, api_key_id, group_code, model, channel_id,
       countState(), sumState(toUInt64(prompt_tokens)), sumState(toUInt64(cached_tokens)),
       sumState(toUInt64(completion_tokens)), sumState(toUInt64(reasoning_tokens)),
       sumState(amount_micro), sumState(discount_micro), sumState(upstream_cost_micro),
       sumState(toUInt64(is_error)), sumState(toUInt64(latency_ms)), sumState(toUInt64(ttft_ms)),
       countIfState(ttft_ms > 0)
FROM request_log_raw WHERE ts < '<MV 创建时刻>'
GROUP BY hour, user_id, api_key_id, group_code, model, channel_id;
```

（列序与 MV 定义一致。）不回填的后果不是报错而是**用量分析页的数字小于总览页**——两者数据源不同，前者只见 MV 创建之后的流量。

以下是保留的旧 MV 结构；其中无条件 `ttft_q` 不再供质量 API 读取，新 TTFT 聚合以 `ch_schema.sql` 为准。

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
- 聚合在**写入时增量完成**（MV 随 chsink 批写触发），无夜间批任务；积压或故障会延迟可见性。「今日实时」秒级读 Redis KPI。
- **保留期分层**：raw 默认 180 天可配；MV 聚合表保留 ≥2 年（聚合体量小，成本可忽略）——明细过期后，用户的历史账单趋势与月度汇总仍可查。
- 十亿+/日 走 CH 分片 + Distributed 表（IMPLEMENTATION §12.1 档位三），MV 定义不变。

### 3.3 写入与查询护栏

- 写入：worker 每秒调度，PG 冻结批次最多 500 行；先提交批次/事件回执，再以 `billing-batch-v1-<UUID>` token 投递原行，重试不混入新事件。直连和 NATS 共用回执，完成事件即使已清理 outbox 或重新发布也不再次写 CH。relay 发布 ID 及消息 `_billing_event_id` 来自 outbox 服务端 UUID；NATS 在 PG 已持久接管后 ack，CH 五次失败由 PG 批次入 DLQ。选中任一成员重投会恢复整个原批次，HTTP/MCP 返回实际成员数，MCP dry_run 提供 `requeue_members`；重投保留 token 和行。**丢弃必须选择全部待处理成员，部分选择返回 400/`delivery_batch_members` 且不修改任何行**。DLQ 列表返回 `delivery_batch_id`/`delivery_batch_size`，按 `batch_id` 筛选时可读取最多 500 行。旧 DLQ 无批次身份仍重入 outbox。表侧 `non_replicated_deduplication_window=1000`，插入带 `deduplicate_blocks_in_dependent_materialized_views=1`；**CH 成功、PG 未完成且原 token 已被驱逐时仍不能保证去重**。历史重复/旧模糊批次不会自动修复，详见 [投递核对](billing-delivery-idempotency-audit.md)。
- 查询（console/MCP 统一继承）：`max_execution_time=15s`、`max_memory_usage=2GiB`、结果缓存 60s–10min + singleflight。
- 退款冲销：chsink 同时消费 `billing.refunded`，写负额修正行（同 request_id，log_type=6 退款，对齐 new-api LogTypeRefund），聚合口径自动一致。 退款事件沿用原成本 NULL/非 NULL 生成 `upstream_cost_known`，明确零成本为 true；保留原 `pricing_snapshot` 为 `ratio_snapshot`。活动账单保留原 `pricing_epoch`，归档回执只从原快照读取明确整数 epoch，缺失为 NULL，不查当前版本。Token 调整为零、工具实测不复制。四金额 checked 取反，溢出回滚整笔退款；已投递旧事件的补偿另行验证。

### 3.4 可关闭性

`clickhouse.enabled=false` 时：chsink 停用、统计接口 fail-closed 返回 501 error_code（与老仓库行为一致），计费/账本完全不受影响。

### 3.5 与 new-api 统计字段对照（迁移完备性基线）

**日志诊断补齐（2026-09-30）：** 新请求的 `usage_details.diagnostics` 与 outbox、CH 同源保存错误摘要、失败阶段、渠道尝试、实际返回模型、推理强度和显式客户端会话/UA。只保留有界字段，不存请求正文；摘要脱敏凭证。个人 API 仅投影错误、模型、强度、流结束原因及媒体参数，不返回渠道/key 尝试、会话和 UA。实际返回模型的观察独立于 `bill_by_response_model`，不会自动改变计费模型。尝试耗时表示响应准备阶段，不等于整段生成耗时。历史记录不补造诊断。

请求失败与账务状态分别展示：个人 `errors_only` 包含 `log_type=5`、账务失败以及诊断标记的流中断，保留退款/已结算状态；正常退款不算请求失败。管理员明细批量读取 PG 当前账务状态，退款后不沿用 CH 投递时的结算状态；无 PG 记录时回退投递状态。首字和总耗时使用采集标记，未采集返回 null，真实 0 ms 保留。绝对时间参数显式采用 UTC，不随查询的本机日历时区偏移。仍仅覆盖已持久化账单请求，鉴权等入口拒绝不会因此新增账单日志。

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

**日志页统计条（Stat = 消耗 quota + 最近 60s RPM/TPM，可按用户/令牌/模型/渠道/分组过滤）**：无过滤走 Redis KPI 秒桶（秒级）；带维度过滤走 CH raw 60s 窗口查询（仅扫最新分区，毫秒级）。【已实现：`GET /admin/logs/stat`，响应 `rate_source` 字段标注数据源；明细检索 `GET /admin/logs` 同批落地，字符串过滤走 CH 服务端绑定参数（`query_with_params`），见 IMPLEMENTATION §11.12】

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

### 3.7 渠道 / 模型 / key 关系的取舍（2026-08-31 五方对照定案）

对照 new-api（QuantumNous main）、Sub2API、LiteLLM Router、老 ok-api 与本项目，五种关系模型的差异集中在三个问题上：

| | 调度单元 | 模型身份 | "谁能用哪些上游" | 路由策略归属 |
| --- | --- | --- | --- | --- |
| **Okapi** | channel(1) → channel_keys(N) 建表 | 全局唯一 + 一等定价 | pool（本次改造前为 price_group 直绑） | channel_pools.routing_strategy |
| new-api | channel 一行多 key，**key 状态存 JSON map** | 仅字符串 | 物化 `abilities(group, model, channel_id)` | 无 |
| Sub2API | account = 凭证即单元 | 仅字符串 | api_key → group（账号池） | group.model_routing JSONB |
| LiteLLM | deployment = 模型×凭证×端点 | public 别名 | 同名 deployment 隐式成组 | routing_strategy + fallbacks |
| 老 ok-api | provider_api_key = 凭证即单元 | **按 provider 分域** | api_key → model_group → 模型+凭证 | model_groups.routing_mode |

**保留本项目的两处**：

1. `channel(1) → channel_keys(N)` 建表。new-api 把每把 key 的状态放在按数组下标索引的 JSON map（`MultiKeyStatusList map[int]int`）里，删一把 key 下标即错位，也无法按 key 查冷却、出统计；Sub2API / LiteLLM / 老 ok-api 则以凭证为调度单元，20 把同端点 key 要重复 20 份 base_url 与模型清单。建表方案两个问题都没有。
2. 全局唯一模型名 + 一等定价。老 ok-api 的 `UNIQUE(provider_id, model_code)` 使同一个 gpt-4o 跨两家上游成为两行，要定价两次，用户侧模型名还会歧义。

**改掉的一处**：`price_groups` 原本同时承担"付多少钱"（group_ratio）与"能打哪些渠道"（group_channel_bindings）。这两件事没有内在关联，"同价不同池"（stable / fast 同价）或"同池不同价"（限时促销）都得复制分组并手工同步倍率。更要紧的连带后果是**没有任何一张表拥有"怎么在候选里选"**，所以 least_latency、模型级 fallback 这类能力没有归属处。故拆出 `channel_pools`（§1.2）。

**借入的四项能力**（各有出处）：`channel_keys.rpm_limit` / `daily_spend_cap_micro`（老 ok-api provider_api_keys、Sub2API account 并发）、`channel_keys.model_subset`（老 ok-api supported_models）、`models.fallback_models`（LiteLLM fallbacks、老 ok-api fallback_model_code）、`channel_pools.routing_strategy`（老 ok-api routing_mode、LiteLLM routing_strategy）。

**明确不借**：老 ok-api 的 provider 分域模型（定价重复 + 名字歧义）、绝对价存储（倍率心智已与主流对齐，见 §11.5）、new-api 的物化 `abilities` 表——候选查询当前不是瓶颈，物化会引入一致性维护成本，待真成瓶颈再议（届时 pool 已把可见性收敛为一次 join，物化更容易）。

**2026-09-02 复核后的四处修正（IMPLEMENTATION §11.14）**：① 可见性收敛为一条规则"渠道只服务它所在的池"，内置 `default` 池 + 分组必有池，退役 strict 三态；② 池级降级 `fallback_pool_code`（对应 new-api 令牌 `auto` 分组的核心诉求：vip 优先专属渠道、打不通退公共渠道，计费仍按分组倍率）；③ `pool_channels` 成员级 priority / weight 覆盖（Sub2API `account_groups.priority` 对齐：同一渠道在 stable 池主力、fast 池备胎）；④ `price_groups.self_select` 用户自选档位（new-api `UserUsableGroups`）。

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
| chsink | BILLING | 30s | -1 | PG 持久接管后 ack；CH 重试五次由 PG 批次入 DLQ；无效消息直接持久入 DLQ |
| audit | BILLING | 30s | 5 | 对账抽样比对 |
| notifier | NOTIFY | 60s | 3 | 通知分发（M4 全量） |

单机无 NATS 形态：直接从 billing_outbox 持久组批；NATS 形态继续处理已分配的直连批次，形态切换共用事件回执。新消费者以 create-or-update 升级原 max_deliver=5 配置，PG 临时不可达不应提前耗尽消息投递。

## 5. 一致性与对账

- **三方对账**：Redis `bal:{uid}.avail` ↔ PG billing_events 重放余额 ↔ CH 金额汇总；reconciler 每 5min 分页全量，差异 > 0 即告警并生成修正 adjust 事件（人工确认）。
- **幂等锚点**：commit/refund 的账本幂等与统计投递分开；服务端 outbox 事件 UUID、PG 事件回执、不可变 CH 批次 token 负责统计侧重试。同 request_id 的消费/退款是不同事件，不以 request_id 单列去重。
- **在途预扣泄漏**：reconciler 扫 `bal:{uid}` 中超过 deadline 的 `r:*` 字段 → 按 billing_records 终态决定 commit 或 refund 补偿。
- 余额快照列 `users.balance_micro` 由 worker 周期从事件流重放校准（展示与导出用，不参与计费判定）。


`0013_image_batch_downloads.sql` 增加 `image_batch_downloads(id UUID PK, batch_id UUID FK, expires_at TIMESTAMPTZ)`，并对 `(batch_id, expires_at)` 建索引。ZIP GET 在锁定批任务、复核归属/终态/到期及清理状态后签发 600 秒租约，每个任务最多 16 个有效租约；HEAD 不签发。逐张内容查询必须带有效租约，流不持有长期事务。清理领取排除有效下载租约，且拿到批任务行锁后再次检查；已获授权的传输可在用户删除/任务到期后完成，新的下载立即拒绝。响应体结束/取消释放租约，进程异常等待到期；下一次下载或最终产物清理删除陈旧记录。列表筛选在 SQL LIMIT 前执行，名称为字面子串，时间为左闭右开，downloaded 根据首次下载尝试判断；同归属已删除的游标仍能用于继续分页。


`0014_image_batch_statistics.sql` 增加创建时成员归属与 results_ready_at，以及独立 image_batch_statistics 投递表。公开终态与统计意图同事务，金额/Token 取已关闭凭据、recorded_at 取首次账单时间；租约 120 秒，失败 30 秒后重试。Redis 成员/月用量/渠道消费/KPI 每项具有同槽去重凭据和原时间桶，统计失败不重复结算。清理图片保留此投递记录；成员快照不从当前 key 回填历史。详细边界见 [统计补记与软限额](native-image-batch-jobs.md#统计补记与软限额)。

## 审计修复后的恢复与时间契约

`video_tasks`（迁移 0030）以 `(user_id,task_id)` 隔离任务，request_id 唯一绑定原始账单；保存 channel_key_id、pending/completed/refunded 状态、next_poll_at。创建账单与任务映射同事务；每分钟 worker 领取最多 100 个到点任务，失败/取消走原账单幂等退款。轮询回源映射不再仅依赖 Redis 的 48 小时 TTL。

Redis `settlement:{retry}:payloads`（HASH，request_id → 完整结算输入）与 `settlement:{retry}:order`（ZSET，下次恢复的毫秒时间）同槽原子写入、不设置 TTL。PG 接受账单后删除；worker 每秒补写最多 500 笔（可配置，最多 8 笔并发），重放仍由 PG request_id 幂等闸和 UserGuard 串行化。进程宕机后可继续恢复，前提是 Redis 按热账本要求启用持久化与禁止淘汰。暂时失败指数退避，无效或冲突载荷保留在 quarantine 中供修复。

余额到期只移除正可用余额，不移除在途预扣。expire 事件、PG 快照、清除到期标记与 fund_transfers 在同一事务提交，再按 transfer 回执更新 Redis；应用失败保留恢复意图。repair.lua 在途累加与 target 减法均检查 Lua 安全整数范围，超过范围拒绝写入。

对账的 limit 表示每页大小；按 user_id 游标遍历所有未删除用户，不再永久只检查前 1000 人。

已成功投递且超过 7 天的 outbox、CH 事件回执与冻结批次按依赖顺序清理；有 DLQ 引用或未完成批次一律保留。7 天覆盖 JetStream 48 小时消息保留窗口，超过窗口的外部人工旧消息重放不在投递幂等保证内。

时间口径来自机器 `TZ`、`/etc/localtime` 或 `/etc/timezone` 的 IANA 名称；多副本应配置一致。PG 每条连接设置此时区，CH 查询指定 session_timezone。CH `ts`、ingested_at、历史校准时间显式 UTC，写入 UTC 墙钟字符串不会随容器时区改变。新增 `mv_calendar_minute`（minute,user_id,api_key_id,group_code,model,client_type）：countState 已投递记录数（物理列仍名为 requests，含退款，不能直接作为 API 调用数）；Token 四轴与合计、四金额、错误的 sumState；缓存写入 sumState、数字存在与读写上报的 countIfState。无 TTL，保持原始日志到期后的本地日历统计。该 MV 独立新增，不 POPULATE、不覆盖旧表。

非 UTC 日查询在每个旧小时与所需维度比较新分钟聚合的财务记录覆盖；完全相等时选择分钟，否则整桶选择旧小时，不能相加。旧小时的起止时刻必须属于同一本地日期；否则返回 `statistics_calendar_history_incomplete` 存储错误，禁止把半小时/四分之三小时时区的午夜两侧混算。分钟起止日期也需相同，避免历史秒级偏移误分。四金额在分钟来源独立保留；旧 cube 来源的 original 仍按 amount+discount 恢复。客户端保留原 uniq 聚合状态；缓存数值/采集计数保持相同来源。仅有旧 UTC 日桶而没有小时/分钟证据的客户端与缓存历史仍无法重建，原表保留供核查。历史 raw 回填必须单独检查覆盖与幂等，当前升级不自动回填。查询性能仍须在生产规模另行验收。

旧 UTC 日状态也参与覆盖检查：按原视图维度比较日请求（缓存视图比较相应已观察计数）与 UTC 小时合计。旧日更多时，两个可能受影响的本地日期保留带错误闸的原聚合状态；读取这些日期的度量会明确拒绝，不能把保留的旧消耗显示成零。其他用户/维度和无关日期不受此错误闸影响。该路径只暴露证据不足，不把 UTC 日数额伪装成本地日数额。

会话列表的 sid 字段是 SHA-256 前 128 位指纹，仅用于展示与吊销，不是 cookie 凭证。吊销在已鉴权用户的会话内匹配指纹。会话与 OAuth state cookie 使用 Secure/HttpOnly/SameSite=Lax；OAuth 回调须带发起浏览器的 provider 专属 state cookie。

请求统计仅计进入预扣后的终态（成功与失败）；无效 key、预扣前限流或余额不足不进入账单统计，应通过入口访问日志观察。API key 模式的前端仍把密钥保存在 localStorage，浏览器脚本可读取；使用者应按共享设备与浏览器扩展风险选择该模式。开启注册赠送/邀请奖励时应启用邮箱验证与反自动化验证，否则不同邮箱自邀属于配置滥用风险。

调用计数排除 `log_type=6` 退款调整。原日/小时与日历分钟聚合的 countState 仍是财务记录数，保留作覆盖证据。新增独立 `population_v1_mv_*`：由嵌入式 MV 定义确定相同粒度和状态类型，调用与测量使用 StateIf 外层的 `log_type IN (2,5)`；四金额/成本已知数/财务时间保持全部事件；额外 countState `financial_records` 及 countStateIf `population_classified` 保存总记录与 2/5/6 分类覆盖。不覆盖既有表、不 POPULATE，无 raw TTL，原 materialized-view 幂等设置保留。

`population_source_v2_mv_*` 是只读选择视图（v1 旧读取定义保留，v1 分类物理状态沿用）：每个完整粒度先比较原记录数与新总记录数，完整且可解释的新源优先；新源不足时仅用记录数足够且类型明确的 raw 重算。不能叠加两种来源。旧聚合缺分类且 raw 不全，主调用/Token/错误状态以 `statistics_request_history_incomplete` 拒绝猜值，财务专用读取保留旧金额。独立测量读取分类调用子集，与主调用数核对覆盖，不能使用旧测量记录数扩大财务事实。原始日志筛选不改写为调用源；另有 `request_log_calls` 只读视图供采集与性能重算。分析补充范围按总财务记录而非调用数决定，退款只发生窗口仍保留负金额。成本覆盖以财务记录数为分母，新增 `financial_records`/`cost_known_records` 字段，兼容旧 `cost_known_requests` 的记录计数，不视作纯调用计数。

读写限制聚合哈希表预分配到 8192 个元素，不截断真实分组；查询执行时间 15 秒、内存 2,000,000,000 字节不变。每次读取先以原记录和分类记录状态核对被引用主事实表（金额/错误）的完整粒度，完整表直接读取分类聚合；未证明完整的主事实表走恢复视图。独立测量表读已分类的测量子集，覆盖不足由调用方与主调用数比较并择一恢复完整 raw，否则输出部分覆盖/null；旧测量计数不作为主财务记录数。证明不缓存，但与最终统计是两次读取，只承诺既有异步新鲜度。简单存储键 WHERE 引用按实际范围并集单独证明，重复条件只保留一次；复杂/别名引用另保守核对全表。两份证明不混用：某个复杂引用未完整不能迫使已证明的简单范围恢复，也不能由简单范围证明授权该复杂引用。两种路径的历史大基数成本仍须性能验收；恢复 CTE 的 view() 隔离减少外层列引用对子树重复哈希。

以上实现正在联测，不能宣称所有端点或生产历史已验收；证据和未完成边界见 [退款与调用统计样本范围](request-population-audit.md)。

### 复扫加固约定

会话与 OAuth state cookie 缺省 Secure；纯 HTTP 内网部署可显式配置 `OKAPI_COOKIE_SECURE=false`，HttpOnly 与 SameSite 保留。模型列表接受 Gemini `?key=`，头凭证优先。MCP `billing.read.own` 只能解释本人账单；角色权限仅接受已登记权限及 own/all 范围。Turnstile 自定义验证地址同样受 SSRF 策略约束。

对账每页最多读取 1000 个用户，只汇总该页账本并立即提交，Redis IO 不持保留清理锁。双后端失败的错误记账改由受下线计数及积压准入跟踪的后台任务继续恢复，不持结算许可睡眠、不阻塞 HTTP 返回。

Redis journal 保留先写后结算的持久接受顺序，正常账单的保存/删除两次写是恢复契约的成本；`OKAPI_SETTLEMENT_JOURNAL_MAX` 缺省 100000 条，满时保留旧账并拒绝新增日志。worker 每秒并发恢复，`OKAPI_SETTLEMENT_RECOVERY_BATCH` 缺省 500 条；失败指数退避，无效载荷从活动队列移入同槽 quarantine HASH，保留载荷供人工修复而不永久循环。投递清理每次只删除有限行，逐批提交。

quarantine 同样计入容量上限。无效结算、预扣冲突以及 SQL 数据/约束错误进入隔离；连接故障、死锁、锁超时继续退避重试。修复前必须核对 PG 幂等回执；复投时用同槽 Lua 原子将修正载荷移回 payloads、设置 order 的到点时间并清除 attempts。保留原 request_id，不以新 UUID 重记同一笔账。

行为变化：错误邮箱验证码一次尝试即消费；透传及视频的非 per_call 模型会显式拒单。私网 webhook 需显式配置 ssrf_policy，拒绝的通知配置会记录持久审计告警。

HTTP 请求 span 仅记录 URL 路径，查询字符串中的 Gemini key、OAuth code/state 不进入访问日志。失败账单保留实际错误码，包括上游超时、路由无候选和定价计算失败。

复核补充：无初始化令牌时只允许无转发头的直接环回连接；反代请求即使声称环回来源也须提供 `OKAPI_SETUP_TOKEN`。所有 HTTP 面统一返回禁止框架嵌入的响应头。OAuth 和找回密码共享配置站点地址的验证逻辑，配置读取故障传播为内部错误。

视频成片下载最多手动处理五次 301/302/303/307/308，每一跳都按 SSRF 策略验证，拒绝 HTTPS 降级与无效 Location；只有原始上游 origin 接收渠道凭证及自定义头，CDN 等其它 origin 不接收这些头。跳转响应在服务器消费，最终视频继续流式传输，下载建连与跳转总窗口 120 秒。其余数据面出站仍不跟随跳转。

维护、图片与结算恢复 worker 的提前退出或 panic 会记录任务名并退避重启；下线期间不重启，仍受 30 秒总排空窗口约束。通知配置读取故障显式告警。结算 journal 单测使用每个 AppState 独立的 Redis hash-tag，生产键名及重启恢复范围保持不变；整套数据库测试须使用独立 PG/Redis/CH/NATS 实例。

UTC 存储的历史扫描游标必须显式按 UTC 解析；统计原始流量恢复的五分钟文本桶须与保留聚合采用相同机器时区，避免 UTC/本地文本键不一致导致恢复样本丢失。

Chat 到 Responses 的 reasoning 增量转换为独立 reasoning summary 项，增量字符只计一次，终态保留完整 summary；真实 usage 仍优先。合成 SSE 错误经入口协议统一组装，携带稳定错误码与 request_id；不生成成功终态来掩盖结算错误。对账 `limit` 继续表示每页用户数，worker 与 `all=true` 完整扫描，不以页大小截断覆盖范围。

直接从小时或五分钟桶按机器日期取数时也检查桶是否跨越本地午夜。缺少更细证据时返回 `statistics_calendar_history_incomplete`，不能把整个跨日桶归给起点日期；已保留的分钟日聚合使用细粒度来源重建。复制的 UTC tzfile 可通过 Etc/UTC 等候选识别，绝对 TZ 路径统一成 IANA 名称，启动记录生效时区。

### 第三轮加固（迁移 0031）

`users.totp_last_counter` 记录最近成功的 TOTP 时间片；登录/绑定/解绑通过条件 UPDATE 单次消费，新时间片必须严格更大。绑定要求原密钥为空，禁止覆盖。`image_batches_live_capacity_idx` 仅覆盖未清理批次；历史幂等元数据保留但不占运行准入预算。视频创建超过 24 小时通过既有退款 request_id 幂等路径终结；0032 增加 refund_pending 持久化退款意图，见下文。

NATS relay 使用 `billing_outbox.next_retry_at` 作为 10 分钟认领租约；最终状态更新必须重新匹配原租约并取行锁。网络 I/O 不持 PG 事务。毒消息 DLQ 用 `outbox:<event_id>` 唯一键保留原始载荷，重投恢复原 outbox 身份，避免重建 event_id 导致重复统计。DEFAULT 分区旧行每批最多 1000，在保留策略排他锁下先移入 `billing_event_carry` / `billing_record_receipts` 再删除，同一事务失败则整体回滚。

迁移 0032 增加 `video_tasks.refund_pending`：失败/超时与成功完成通过条件 UPDATE 原子争抢 pending 终态；先持久化退款意图、释放 PG 事务后执行幂等退款。进程中断或退款后状态更新失败，worker 继续扫描 refund_pending 并重复同一 request_id 退款，完成后标记 refunded。已完成的任务不会被超时分支退款。


统计覆盖探测与来源构建在展开前应用全部可用主维度（user/key/channel、已计费模型、分组及模型/分组数组）；字符串通过同一组 CH 绑定参数传递，不拼接输入。详细维度不在旧主聚合伪造。非法指标与流向阶段先于覆盖查询校验。JSONEachRow 查询及其覆盖探测的 HTTP200 部分响应如含 exception，整次读取失败；缺分类 raw 汇总不得填零，原始明细可照常检索。 历史缺失错误只接受 CH `throwIf` 的 Code 395 且主异常消息以完整契约错误码开头（兼容纯文本及 JSON `exception` 封装）；SQL 回显、堆栈或其他 CH 错误码提及同名字符串不构成历史缺失证据。普通查询错误保留存储错误类型，不能误提示历史不可恢复。后续固定源码验收记录在样本范围审计中。

CH 查询限定聚合与 Join 哈希预分配 8192（不截断实际分组），读块 8192；写入限定聚合预分配、块 8192、MV 目标块 8192 行/4MiB、JSON 串行解析。15 秒/2GB 查询执行护栏未扩大。该设置改变写入块形成，本轮八项实际投递幂等回归已通过，仍不代表生产容量验收。

统计分类快路径证明可按实际 SELECT 范围裁剪：可确认仅使用持久粒度键 WHERE 的同名引用，在范围并集核对旧记录、新记录与分类覆盖，并使用数据查询的绑定参数和机器时区；复杂引用另核对全表。仅给各自证明覆盖的引用选择分类快路径，复杂引用未证明完整时继续恢复。该证明仍为每次读取生成，不改变恢复择一/缺分类拒绝的语义，也不扩大计划优化数或执行资源上限。

### 原生服务端工具准入快照（2026-10-01）

`pricing_snapshot.server_tool_admission` 保存准入时的原生工具完整定义（已补
`max_uses`）及该请求所有允许模型报价的预扣最大金额。实际工具计数仍只来自
`usage_details.tokens.server_tool_usage`；上限不是已发生用量，不能汇入 Token/TPM
或长期使用量。此阶段不改 PG/CH 金额列、Redis 键或 Lua 契约；失败沿既有退款链。

### 工具请求作用域与计数（2026-10-01）

新报价的 `pricing_snapshot.server_tool_fees[]` 增加可选 `requested` 布尔值，明确本次是否声明该工具；
老报价省略字段保留未知授权。`requested=false` 的未观察/明确零计数对应四费用
分量零；`quantity` 保持原生观察值或 null，不伪造源用量。未请求非零用量拒绝结算。
此标记随原快照进入 PG、outbox、CH，不改原生计数列、金额列或 Redis/Lua 契约。

原生工具请求作用域也适用于已发布工具价格、但本次没有声明工具的请求；上游显式零计数仍保留为零，其他未报告轴保留未知，不为它们要求付费计数。四种入口携带的明确 Anthropic 搜索/读取声明均参与渠道 tools 能力过滤；明确禁用的渠道不接原生工具调用，普通调用继续可选。


### 原生工具长期统计契约（retained tools v1）

新增 `server_tool_minute_v1` 独立 UTC 分钟聚合，保留完整分析维度（用户、密钥、分组、计费/请求/上游模型、渠道、端点、上游端点、节点、流式/请求/计费类型）。无 POPULATE、无 TTL，旧数据只在原始明细仍完整时恢复；无法恢复的历史保留缺失覆盖率，不填零。原有 Token、四金额、TPM 和价格公式不改。

三轴 `web_search`、`web_fetch`、`code_execution` 的实测次数只来自严格的 Anthropic 原生整数计数（0..=i32::MAX）；缺失/null/错误类型不是零。数量分母为实际调用（log_type 2/5），退款 6 不产生新调用。执行次数仍不是容器计费时长。

费用独立读取原请求冻结的 `server_tool_fees`，仅认可已支持的 `anthropic_server_tool_use_v1/request` 搜索/抓取费用行；金额和原金额必须为非负 i64 整数，优惠允许有符号 i64，且 original=amount+discount，同一工具重复行或缺失/未知契约不认可；included/未声明行必须明确为零，additional 行须有合法请求单价、数量与一致 list_price，不从名称推断免费。Included/未声明的明确零费用可以有完整费用覆盖而没有完整数量覆盖。退款沿用原快照，按冲销事件取反；不读取当前价格。执行时长没有受支持的冻结费用契约，费用保持未知。

每个小时与完整维度组只选一个来源：分钟保留完整时用保留值；否则在完整原始明细可用时恢复；都不完整时选择覆盖较多的一个部分来源，缺少的调用/财务分母保留为未知观察。来源不叠加；超过账务基准的计数拒绝查询。粗粒度缺失小时跨本地午夜或查询边界时拒绝按日期猜分；分钟完整数据支持夏令时与非整小时时区边界。

`GET /admin/stats/tools` 要求 billing.read；`GET /api/me/stats/tools` 固定鉴权用户，默认当前密钥，scope=user 可看本人所有密钥。日期窗口、模型来源与分析维度过滤、按日/UTC 小时和维度拆分均在服务端验证；默认每页20行、最多100行，独立返回完整窗口 total 与过滤后的 total_rows。数量和费用分别返回 observed 值、覆盖率和 complete；任何范围/数值溢出拒绝返回。选择范围内未分类的历史事件不能伪装成零调用。

2026-10-01 有符号费用修复契约：`server_tool_minute_v1` 维度、列类型和聚合状态不变，旧投影通过 MODIFY QUERY 更新未来写入的优惠校验；系统元数据中两轴 v2 标记存在时保持幂等。不删历史、无 POPULATE。旧费用覆盖不足且 raw 仍完整时，仅在原始调用/财务分母匹配、各观察轴覆盖不下降并有更多费用观察时替换整个小时来源；raw 过期的旧缺失费用保持未知。原金额/金额/单价/list_price 仍非负，优惠为有符号 i64 且原金额减金额等于优惠；退款翻转冻结三金额，不能按新价恢复旧丢失观察。具体迁移和真实账单验证见 [工具费用核对](server-tool-accounting.md)。


工具统计分页的25模型实测曾触发旧隔离 CH 总内存护栏（3.61 GiB > 3.60 GiB），保留失败证据。v9 下调聚合哈希预分配到1024仍在既有请求分类测试触发3.80 GiB总内存，不能据此认定原因或解决容量问题；最终源码恢复原8192。新建本轮专属、相同镜像和4GiB容量的隔离 CH 继续功能验证，原服务、数据和配置保留。Join预分配8192、读写块8192、MV目标块8192行/4MiB、去重token、15秒/2GB查询护栏全部不变。新环境通过只证明所选功能与模拟样本；原统计500、旧隔离库内存与生产大基数容量仍待验证。


v10 非整小时时区实测发现：缺失分钟后的粗小时在范围过滤中可能先被裁掉，导致小时内不可恢复的记录错误变成空结果。工具统计必须在最终日/分钟过滤之外，对完整窗口与维度的 mode 单独投影完整性标志，并与总计行一起返回；Rust 在输出任何结果前校验标志，缺失边界拒绝，不以空 total 绕过校验。这是同一服务端查询中的独立校验，不扩容、不降低测试断言。


v11 独立校验仍复现 Kolkata 空结果；实际 ClickHouse 表达式确认本地00:00取整后是UTC18:30，而保留聚合的整小时桶为UTC18:00。最终契约要求先将日期窗口端点转为UTC再向小时下/上取整，保证扫描包括所有相交的UTC小时；日/分钟精确过滤仍用原本地窗口，独立完整性校验保留。完整分钟历史必须正确返回单日本地午夜后的首分钟，缺失分钟的相交粗小时必须拒绝猜分。

### Channel account controls (migration 0034)

`channels.settings.account_control` stores validated admission/refresh/cooldown policy.
`channel_usage_windows` stores request-attempt counts keyed by `(channel_id, period,
window_start)`, with UTC boundaries computed from the machine's calendar timezone.
Admission holds a per-channel PG transaction advisory lock. Token and upstream cost
usage is read from `billing_records`, and is not a new customer ledger. Expired windows
are pruned in bounded 1000-row batches after 45 days. See
[channel-account-controls.md](channel-account-controls.md) for limits and semantics.

### Egress proxies (migration 0037, IMPLEMENTATION §11.41)

Proxies are first-class resources. A channel binds its egress through three columns,
`channels.egress_mode` (`NULL` = inherit the global default, `direct`, `proxy`, `group`),
`egress_proxy_id` and `egress_group_code`, whose shape is enforced by the
`channels_egress_shape` CHECK. The global default is the `settings` key `egress_default`
(the same JSON shape; written only through `PUT /admin/egress/default`).

| Object | Notes |
| --- | --- |
| `proxies` | `url_ciphertext` holds the whole URL (credentials included) in the credential envelope; `scheme/host/port/username` are display copies. `status` 1 enabled / 2 disabled. Passive breaker: `failed_count`, `cooldown_until`, `last_error` (cooling never changes `status`; an expired cooldown is half-open without a worker). `max_keys` caps pinned assignments per proxy across groups. Last console test: `exit_ip`, `exit_country`, `latency_ms`, `checked_at`. |
| `proxy_groups` | `mode` = `pinned` (each key keeps one proxy) or `rotate` (per request: priority tier, weighted random). |
| `proxy_group_members` | `(group_code, proxy_id)` with `priority` and `weight > 0`; cascades with either side. |
| `channel_keys.egress_proxy_id` | The pinned assignment of this key (= one upstream account). Honoured only while the key's effective egress is a pinned group that still contains the proxy; `ON DELETE SET NULL`. |
| view `channel_egress` | Effective egress per channel (channel binding, else the global default, else direct). A dangling default keeps `mode` and yields no proxy, so it fails closed. |
| function `egress_pick(channel, key, healthy_only)` | The one resolution used by candidates, custom_pass, video polling, control-plane calls and diagnosis. Returns `mode` plus at most one proxy; no proxy with `mode <> 'direct'` means unavailable, never direct. Disabled proxies are never returned; cooling ones only when `healthy_only = false`. Declared `VOLATILE` because rotation uses `random()`. |

Pinned assignments are reconciled in full, under one transaction-scoped advisory lock,
by every write that changes bindings, members, capacity or the global default: stale
assignments are released and waiting keys are assigned to an enabled, non-full member
(healthy first, then least loaded, then priority). A tripped or disabled proxy never
causes reassignment. Proxies or groups bound directly by a live channel, or used as the
global default, cannot be deleted (409). Writes of the retired
`settings.proxy_url` are rejected with 400; the one-off conversion of legacy values was
dropped when the migrations were squashed into the baseline (IMPLEMENTATION §11.10).

Migration 0038 adds `proxies.max_concurrency` (in-flight cap through the proxy, enforced
with the Redis lease `conc:px:{id}:v1` next to the key lease), `previous_exit_ip` and
`exit_ip_changed_at` (set when a manual test or a background probe sees a different exit
IP), and recreates `egress_pick` so it also returns the picked proxy's `max_concurrency`.
