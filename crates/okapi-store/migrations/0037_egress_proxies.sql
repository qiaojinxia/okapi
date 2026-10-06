-- 出口代理（IMPLEMENTATION §11.41）：代理成为一等资源，渠道按「继承 / 直连 / 单个代理 / 代理组」
-- 绑定出口；继承链 = 渠道绑定 → 全局默认（settings.egress_default）→ 直连。
-- 取代 §11.30 的 channels.settings.proxy_url：旧值迁成 proxies 行并回填为「单个代理」绑定。

CREATE TABLE proxies (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name           VARCHAR(128) NOT NULL,
    -- 与 channels.owner_id 同义：own 范围的渠道管理员只看得见、绑得上自己的代理
    owner_id       BIGINT REFERENCES users(id),
    -- 完整代理 URL（含认证），与渠道凭证同一信封（credential.rs）；无前缀 = 迁移来的明文，
    -- 由 `okapi seal-credentials` 收口
    url_ciphertext BYTEA NOT NULL,
    -- 展示与筛选用的非密字段，写入时由 URL 解析得出；真值以密文里的 URL 为准
    scheme         VARCHAR(8)   NOT NULL,
    host           VARCHAR(255) NOT NULL,
    port           INT          NOT NULL,
    username       VARCHAR(255),
    status         SMALLINT     NOT NULL DEFAULT 1,
    max_keys       INT,
    -- 被动熔断：与 channel_keys 的冷却同一套连续失败 + 指数退避，但不改 status——
    -- cooldown_until 一过即半开放行，不依赖 worker 复位
    failed_count   INT          NOT NULL DEFAULT 0,
    cooldown_until TIMESTAMPTZ,
    last_error     VARCHAR(255),
    -- 最近一次测试（控制台「测试」）的结果
    exit_ip        VARCHAR(64),
    exit_country   VARCHAR(8),
    latency_ms     INT,
    checked_at     TIMESTAMPTZ,
    note           VARCHAR(255),
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT proxies_scheme_chk CHECK (scheme IN ('http', 'https', 'socks5', 'socks5h')),
    CONSTRAINT proxies_port_chk CHECK (port BETWEEN 1 AND 65535),
    CONSTRAINT proxies_status_chk CHECK (status IN (1, 2)),
    CONSTRAINT proxies_max_keys_chk CHECK (max_keys IS NULL OR max_keys > 0)
);
CREATE INDEX idx_proxies_owner ON proxies(owner_id);
COMMENT ON COLUMN proxies.status IS '1 启用 / 2 手动停用；熔断不改 status，只看 cooldown_until';
COMMENT ON COLUMN proxies.max_keys IS
    '固定分配组最多把它分给几把 key（一个出口 IP 挂几个账号）；null = 不限。只约束新分配，不驱逐已分配';

CREATE TABLE proxy_groups (
    code        VARCHAR(32)  PRIMARY KEY,
    name        VARCHAR(128) NOT NULL,
    mode        VARCHAR(8)   NOT NULL DEFAULT 'pinned',
    owner_id    BIGINT REFERENCES users(id),
    description VARCHAR(255),
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT proxy_groups_mode_chk CHECK (mode IN ('pinned', 'rotate'))
);
COMMENT ON COLUMN proxy_groups.mode IS
    'pinned：每把 key 分到组内一个固定代理（持久化在 channel_keys.egress_proxy_id），代理熔断时等它恢复、不换 IP；'
    'rotate：每次请求在健康成员里按 priority 分层 + 层内加权随机选';

CREATE TABLE proxy_group_members (
    group_code VARCHAR(32) NOT NULL REFERENCES proxy_groups(code) ON DELETE CASCADE,
    proxy_id   BIGINT      NOT NULL REFERENCES proxies(id) ON DELETE CASCADE,
    priority   INT         NOT NULL DEFAULT 0,
    weight     INT         NOT NULL DEFAULT 1,
    PRIMARY KEY (group_code, proxy_id),
    CONSTRAINT proxy_group_members_weight_chk CHECK (weight > 0)
);
CREATE INDEX idx_proxy_group_members_proxy ON proxy_group_members(proxy_id);

-- 渠道的出口绑定。egress_mode NULL = 继承全局默认；单个代理 / 代理组被引用时不可删（FK 无级联）
ALTER TABLE channels
    ADD COLUMN egress_mode       VARCHAR(8),
    ADD COLUMN egress_proxy_id   BIGINT REFERENCES proxies(id),
    ADD COLUMN egress_group_code VARCHAR(32) REFERENCES proxy_groups(code),
    ADD CONSTRAINT channels_egress_shape CHECK (
        CASE COALESCE(egress_mode, '')
            WHEN 'proxy' THEN egress_proxy_id IS NOT NULL AND egress_group_code IS NULL
            WHEN 'group' THEN egress_group_code IS NOT NULL AND egress_proxy_id IS NULL
            WHEN 'direct' THEN egress_proxy_id IS NULL AND egress_group_code IS NULL
            WHEN '' THEN egress_proxy_id IS NULL AND egress_group_code IS NULL
            ELSE false
        END
    );
CREATE INDEX idx_channels_egress_proxy ON channels(egress_proxy_id) WHERE egress_proxy_id IS NOT NULL;
CREATE INDEX idx_channels_egress_group ON channels(egress_group_code) WHERE egress_group_code IS NOT NULL;
COMMENT ON COLUMN channels.egress_mode IS 'NULL 继承全局默认 / direct 直连 / proxy 单个代理 / group 代理组';

-- 固定分配的结果挂在 key（= 账号）上：同一账号的推理、刷新、测活都从这个出口出去
ALTER TABLE channel_keys
    ADD COLUMN egress_proxy_id BIGINT REFERENCES proxies(id) ON DELETE SET NULL;
CREATE INDEX idx_channel_keys_egress_proxy ON channel_keys(egress_proxy_id)
    WHERE egress_proxy_id IS NOT NULL;
COMMENT ON COLUMN channel_keys.egress_proxy_id IS
    '固定分配组分给这把 key 的代理；仅当有效绑定是 pinned 组且该代理仍是组员时生效';

-- 有效出口（继承已展开）。候选查询、控制面解析、固定分配对账共用这一处口径。
-- 全局默认指向的代理 / 组缺失或形状不对时 mode 仍保留 proxy/group 而 id 为空——
-- 解析不到代理即不可调度（fail-closed），绝不悄悄退回直连。
CREATE VIEW channel_egress AS
SELECT c.id AS channel_id,
       c.egress_mode IS NULL AS inherited,
       CASE
           WHEN c.egress_mode IS NOT NULL THEN c.egress_mode
           WHEN d.value ->> 'mode' IN ('proxy', 'group') THEN d.value ->> 'mode'
           ELSE 'direct'
       END AS mode,
       CASE
           WHEN c.egress_mode IS NOT NULL THEN c.egress_proxy_id
           WHEN d.value ->> 'mode' = 'proxy' AND d.value ->> 'proxy_id' ~ '^[0-9]{1,18}$'
               THEN (d.value ->> 'proxy_id')::bigint
       END AS proxy_id,
       CASE
           WHEN c.egress_mode IS NOT NULL THEN c.egress_group_code
           WHEN d.value ->> 'mode' = 'group' THEN d.value ->> 'group_code'
       END AS group_code
FROM channels c
LEFT JOIN settings d ON d.key = 'egress_default';

-- 一把 key 此刻的出口：mode 恒有值；proxy_id 为空且 mode <> 'direct' = 不可用（不可调度，绝不直连）。
--   proxy：绑定的那个代理；pinned 组：分给这把 key 且仍是组员的那个；rotate 组：健康成员里
--   priority 分层、层内按 weight 加权随机抽一个（指数时钟，与渠道调度同一抽样法）。
-- healthy_only：数据面传 true（熔断中的代理不用）；控制面传 false（固定出口熔断时照样走它——
-- 宁可失败也不换 IP；轮换组则健康成员优先）。手动停用（status=2）的代理任何时候都不用。
-- random() 是 VOLATILE，函数也必须标 VOLATILE，否则规划器可能按参数复用上一次的抽样结果。
CREATE FUNCTION egress_pick(p_channel BIGINT, p_key BIGINT, healthy_only BOOLEAN)
RETURNS TABLE (mode TEXT, proxy_id BIGINT, url_ciphertext BYTEA)
LANGUAGE sql VOLATILE AS $$
    SELECT e.mode, px.id, px.url_ciphertext
    FROM channel_egress e
    LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
    LEFT JOIN channel_keys ck ON ck.id = p_key AND ck.channel_id = e.channel_id
    LEFT JOIN LATERAL (
        SELECT p.id, p.url_ciphertext
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

-- 旧 settings.proxy_url → proxies（按 URL 去重）+ 渠道绑定「单个代理」，然后删掉旧键。
-- 软删渠道一并迁，保持「恢复即可达」。解析不了的历史值（写入路径本就校验过，理论上没有）
-- 保留在 settings 里不动，由人工处理，不悄悄变成直连。
WITH legacy AS (
    SELECT DISTINCT btrim(settings ->> 'proxy_url') AS url
    FROM channels
    WHERE jsonb_typeof(settings -> 'proxy_url') = 'string'
      AND btrim(settings ->> 'proxy_url') <> ''
), parsed AS (
    SELECT url,
           regexp_match(
               url,
               '^(https?|socks5h?)://(?:([^:@/]*)(?::[^@/]*)?@)?(\[[0-9A-Fa-f:.]+\]|[^:/@\[\]]+)(?::([0-9]{1,5}))?/?$',
               'i'
           ) AS m
    FROM legacy
), shaped AS (
    SELECT url,
           lower(m[1]) AS scheme,
           m[3] AS host,
           COALESCE(m[4]::int, CASE lower(m[1]) WHEN 'http' THEN 80 WHEN 'https' THEN 443 ELSE 1080 END) AS port,
           NULLIF(m[2], '') AS username
    FROM parsed
    WHERE m IS NOT NULL
)
-- 属主：用它的渠道属主一致时随渠道（own 范围的管理员才看得见、改得了自己渠道的代理），否则归全站
INSERT INTO proxies (name, owner_id, url_ciphertext, scheme, host, port, username, note)
SELECT left(host || ':' || port, 128),
       (SELECT CASE WHEN bool_and(c.owner_id IS NOT NULL) AND count(DISTINCT c.owner_id) = 1
                    THEN min(c.owner_id) END
          FROM channels c WHERE btrim(c.settings ->> 'proxy_url') = shaped.url),
       convert_to(url, 'UTF8'), scheme, host, port, username,
       'migrated from channels.settings.proxy_url'
FROM shaped
WHERE port BETWEEN 1 AND 65535;

UPDATE channels c
SET egress_mode = 'proxy',
    egress_proxy_id = p.id,
    settings = c.settings - 'proxy_url'
FROM proxies p
WHERE p.note = 'migrated from channels.settings.proxy_url'
  AND convert_from(p.url_ciphertext, 'UTF8') = btrim(c.settings ->> 'proxy_url');

-- 空串 / 非字符串的旧键本就等于直连：直接清掉
UPDATE channels
SET settings = settings - 'proxy_url'
WHERE settings ? 'proxy_url'
  AND (jsonb_typeof(settings -> 'proxy_url') <> 'string' OR btrim(settings ->> 'proxy_url') = '');

-- 剩下的是解析不了的非空旧值：此前它让每个请求在建 client 时失败（fail-closed），
-- 迁移后没人再读这个键，放着不管就成了直连。停用渠道、保留原值，等人工换成出口绑定。
UPDATE channels
SET status = 2
WHERE settings ? 'proxy_url' AND status = 1;
