-- 适配规则 P2（docs/compat-rules.md §17）：适配档案 + 草稿 / 审批。

-- 1) 适配档案：管理员命名的可复用规则包，等于自定义厂商。渠道用 channels.vendor = 'custom:<slug>' 引用它。
--    档案的规则集在 rule_sets 里（scope_kind = 'profile'，scope_id = compat_profiles.id）。
--    extends：叠在某个内置厂商之上（厂商 100 → 档案 200 → 渠道 300 → 护栏 900），只写差异。
--    hosts：只用于「检测到这个地址，要套用某某档案吗？」的建议，不会静默改变渠道的行为。
--    被渠道引用的档案不能删（控制台删除时检查，同事务里连带清掉它的规则集）。
CREATE TABLE compat_profiles (
    id          bigint GENERATED ALWAYS AS IDENTITY,
    slug        varchar(64)  NOT NULL,
    name        varchar(128) NOT NULL,
    description varchar(512),
    extends     varchar(64),
    hosts       text[] NOT NULL DEFAULT '{}',
    created_by  bigint,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT compat_profiles_pkey PRIMARY KEY (id),
    CONSTRAINT compat_profiles_slug_key UNIQUE (slug),
    CONSTRAINT compat_profiles_slug_shape CHECK (slug ~ '^[a-z0-9][a-z0-9._-]*$'),
    CONSTRAINT compat_profiles_extends_shape
        CHECK (extends IS NULL OR extends ~ '^builtin:[a-z0-9][a-z0-9._-]*$'),
    CONSTRAINT compat_profiles_hosts_limit CHECK (cardinality(hosts) <= 16)
);
COMMENT ON TABLE compat_profiles IS '适配档案：可复用的命名规则包（channels.vendor = custom:<slug>）；规则集在 rule_sets（scope_kind = profile）';

-- 2) 草稿的来源与依据。每个范围至多一个草稿；base_version = 起草时的已发布版本，
--    发布时与当前已发布版本不一致就拒绝（有人在这期间发布过，要重新起草）。
--    source：admin = 管理员在界面 / 接口里存的草稿，mcp = AI 经 MCP 提议的，system = 系统生成。
ALTER TABLE rule_sets
    ADD COLUMN base_version integer,
    ADD COLUMN source       varchar(16) NOT NULL DEFAULT 'admin',
    ADD COLUMN rationale    varchar(1000),
    ADD CONSTRAINT rule_sets_source_known CHECK (source IN ('admin', 'mcp', 'system'));
CREATE UNIQUE INDEX uq_rule_sets_draft
    ON rule_sets (kind, scope_kind, COALESCE(scope_id, 0)) WHERE status = 'draft';
