-- OAuth/OIDC 身份绑定（IMPLEMENTATION §6.4，M3）：
-- 一个用户可绑多家；(provider, subject) 全局唯一。
CREATE TABLE oauth_identities (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    provider    VARCHAR(32)  NOT NULL,             -- github / discord / linuxdo / oidc:<code>
    subject     VARCHAR(255) NOT NULL,             -- IdP 侧稳定主体 id
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    display     VARCHAR(255),                      -- IdP 侧展示名（审计用）
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);
CREATE INDEX idx_oauth_identities_user ON oauth_identities(user_id);
