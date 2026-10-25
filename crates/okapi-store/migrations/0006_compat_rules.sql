-- 适配规则（docs/compat-rules.md）：通用版本表 + 渠道的厂商列 + 把旧的 strip / inject 设置复制成渠道层规则。

-- 1) 渠道选定的内置厂商（带命名空间，如 builtin:deepseek）。NULL = 没选：不套厂商层规则，
--    现有渠道一律是 NULL，所以这条迁移不改变任何渠道的行为。
ALTER TABLE channels ADD COLUMN vendor varchar(64);
ALTER TABLE channels ADD CONSTRAINT channels_vendor_namespaced
    CHECK (vendor IS NULL OR vendor ~ '^(builtin|custom):[a-z0-9][a-z0-9._-]*$');
COMMENT ON COLUMN channels.vendor IS '内置厂商（builtin:<id>）；NULL = 不套厂商层适配规则';

-- 2) 规则集版本表。kind 区分适配（compat）与将来的模型路由（route）；scope 决定它管谁：
--    channel = 单个渠道，global = 整个网关的护栏（scope_id 为 NULL），profile = 适配档案（预留）。
--    已发布的行不可变：改规则 = 新增一个版本再发布，回滚 = 把旧版本重新发布。
CREATE TABLE rule_sets (
    id             bigint GENERATED ALWAYS AS IDENTITY,
    kind           varchar(16) NOT NULL,
    scope_kind     varchar(16) NOT NULL,
    scope_id       bigint,
    version        integer NOT NULL,
    status         varchar(16) NOT NULL,
    schema_version integer NOT NULL DEFAULT 1,
    rules          jsonb NOT NULL,
    note           varchar(255),
    created_by     bigint,
    created_at     timestamptz NOT NULL DEFAULT now(),
    published_by   bigint,
    published_at   timestamptz,
    CONSTRAINT rule_sets_pkey PRIMARY KEY (id),
    CONSTRAINT rule_sets_version_key UNIQUE (kind, scope_kind, scope_id, version),
    CONSTRAINT rule_sets_kind_known CHECK (kind IN ('compat', 'route')),
    CONSTRAINT rule_sets_scope_known CHECK (scope_kind IN ('channel', 'global', 'profile')),
    CONSTRAINT rule_sets_status_known CHECK (status IN ('draft', 'published', 'archived')),
    CONSTRAINT rule_sets_scope_id_shape CHECK ((scope_kind = 'global') = (scope_id IS NULL)),
    CONSTRAINT rule_sets_rules_array CHECK (jsonb_typeof(rules) = 'array'),
    CONSTRAINT rule_sets_published_stamped CHECK (status <> 'published' OR published_at IS NOT NULL)
);
-- 每个 (kind, scope_kind, scope_id) 至多一个已发布版本；global 的 scope_id 是 NULL，用 COALESCE 才能唯一
CREATE UNIQUE INDEX uq_rule_sets_published
    ON rule_sets (kind, scope_kind, COALESCE(scope_id, 0)) WHERE status = 'published';
COMMENT ON TABLE rule_sets IS '规则集版本表（适配规则 / 模型路由共用）；已发布行不可变';

-- 3) 旧的渠道设置 strip_request_fields / inject_request_fields 复制成渠道层规则（版本 1，已发布）。
--    只复制、不删原键：新代码不再读它们，但回滚到旧镜像时旧代码还能读到，剥字段 / 注入不会静默失效。
--    原键等确认稳定后用后续迁移清掉。
--    与旧实现等价：先剥后注；受保护键（旧清单）丢弃；写不成 JSON Pointer 的键（空串、超 128 字符、
--    字面 `*`）丢弃；`provider` 不进 strip——它在整形前就被路由指令剥掉了，写进去也是空操作，
--    而新注册表把它列为受保护路径，写进去反而让整个规则集过不了校验。
--    剥 / 注 stream_options 会让流式结算落到字符估算，规则必须带 billing_critical_ack。
--    crates/okapi-compat/tests/legacy_equivalence.rs 的 rules_from_legacy 是这段的 Rust 镜像，改一边要改另一边。
-- >>> legacy copy
WITH legacy AS (
    SELECT c.id AS channel_id,
           CASE WHEN jsonb_typeof(c.settings -> 'strip_request_fields') = 'array'
                     AND NOT EXISTS (
                         SELECT 1 FROM jsonb_array_elements(c.settings -> 'strip_request_fields') x
                         WHERE jsonb_typeof(x) <> 'string')
                THEN c.settings -> 'strip_request_fields'
                ELSE '[]'::jsonb END AS strip,
           CASE WHEN jsonb_typeof(c.settings -> 'inject_request_fields') = 'object'
                THEN c.settings -> 'inject_request_fields'
                ELSE '{}'::jsonb END AS inject
    FROM channels c
),
strip_rule AS (
    SELECT l.channel_id,
           jsonb_build_object(
               'id', 'strip-request-fields',
               'do', jsonb_build_array(jsonb_build_object('op', 'remove', 'paths', jsonb_agg(p.path ORDER BY p.pos))),
               'billing_critical_ack', bool_or(p.name = 'stream_options'),
               'doc', jsonb_build_object('why', '由渠道设置 strip_request_fields 迁移而来')
           ) AS rule
    FROM legacy l
    CROSS JOIN LATERAL (
        SELECT e.name,
               '/' || replace(replace(e.name, '~', '~0'), '/', '~1') AS path,
               min(e.pos) AS pos
        FROM jsonb_array_elements_text(l.strip) WITH ORDINALITY AS e(name, pos)
        WHERE char_length(e.name) BETWEEN 1 AND 128
          AND e.name <> '*'
          AND e.name NOT IN ('model', 'messages', 'stream', 'previous_response_id', 'conversation', 'provider')
        GROUP BY e.name
        ORDER BY min(e.pos)
        LIMIT 256
    ) p
    GROUP BY l.channel_id
),
inject_rule AS (
    SELECT l.channel_id,
           jsonb_build_object(
               'id', 'inject-request-fields',
               'do', jsonb_agg(jsonb_build_object('op', 'set', 'path', i.path, 'value', i.value) ORDER BY i.name COLLATE "C"),
               'billing_critical_ack', bool_or(i.name = 'stream_options'),
               'doc', jsonb_build_object('why', '由渠道设置 inject_request_fields 迁移而来')
           ) AS rule
    FROM legacy l
    CROSS JOIN LATERAL (
        SELECT k.key AS name,
               '/' || replace(replace(k.key, '~', '~0'), '/', '~1') AS path,
               k.value
        FROM jsonb_each(l.inject) AS k
        WHERE char_length(k.key) BETWEEN 1 AND 128
          AND k.key <> '*'
          AND k.key NOT IN ('model', 'messages', 'stream', 'provider', 'previous_response_id', 'conversation')
        ORDER BY k.key COLLATE "C"
        LIMIT 32
    ) i
    GROUP BY l.channel_id
)
INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, schema_version, rules, note, created_at, published_at)
SELECT 'compat', 'channel', l.channel_id, 1, 'published', 1,
       (CASE WHEN s.rule IS NULL THEN '[]'::jsonb ELSE jsonb_build_array(s.rule) END)
       || (CASE WHEN i.rule IS NULL THEN '[]'::jsonb ELSE jsonb_build_array(i.rule) END),
       '由渠道设置 strip_request_fields / inject_request_fields 迁移',
       now(), now()
FROM legacy l
LEFT JOIN strip_rule s ON s.channel_id = l.channel_id
LEFT JOIN inject_rule i ON i.channel_id = l.channel_id
WHERE s.rule IS NOT NULL OR i.rule IS NOT NULL
ON CONFLICT DO NOTHING;
-- <<< legacy copy
