-- Team 层（IMPLEMENTATION §6.1 定案）：team 即 user 主体（kind='team'，无登录凭证），
-- 钱包/预扣/结算/限流全线复用 user 机制；成员限额 = Redis 月度计数器（软实时）。
ALTER TABLE users ADD COLUMN kind VARCHAR(8) NOT NULL DEFAULT 'user';  -- user | team

CREATE TABLE team_members (
    team_user_id              BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    member_user_id            BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role                      VARCHAR(16) NOT NULL DEFAULT 'member',  -- owner | admin | member
    monthly_spend_limit_micro BIGINT,                                 -- null = 不限
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (team_user_id, member_user_id)
);
CREATE INDEX idx_team_members_member ON team_members(member_user_id);

-- key 归属成员（分账与限额锚点；null = 非团 key）
ALTER TABLE api_keys ADD COLUMN member_user_id BIGINT REFERENCES users(id);
