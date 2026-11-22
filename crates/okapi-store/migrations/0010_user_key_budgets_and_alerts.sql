-- 密钥级周期消费上限（日 / 月 micro-USD）与用户级告警偏好（低余额 / 密钥即将到期）。
ALTER TABLE api_keys
    ADD COLUMN IF NOT EXISTS daily_quota_micro bigint
        CHECK (daily_quota_micro IS NULL OR daily_quota_micro > 0),
    ADD COLUMN IF NOT EXISTS monthly_quota_micro bigint
        CHECK (monthly_quota_micro IS NULL OR monthly_quota_micro > 0);

COMMENT ON COLUMN api_keys.daily_quota_micro IS '令牌级每日消费上限（micro-USD，UTC 自然日）；null = 不限';
COMMENT ON COLUMN api_keys.monthly_quota_micro IS '令牌级每月消费上限（micro-USD，UTC 自然月）；null = 不限';

CREATE TABLE IF NOT EXISTS user_alert_prefs (
    user_id bigint PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    low_balance_threshold_micro bigint
        CHECK (low_balance_threshold_micro IS NULL OR low_balance_threshold_micro > 0),
    key_expiry_warn_days integer
        CHECK (key_expiry_warn_days IS NULL OR (key_expiry_warn_days >= 1 AND key_expiry_warn_days <= 90)),
    email_enabled boolean NOT NULL DEFAULT false,
    webhook_url text,
    webhook_secret text,
    min_interval_secs integer NOT NULL DEFAULT 3600
        CHECK (min_interval_secs >= 60 AND min_interval_secs <= 604800),
    updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE user_alert_prefs IS '用户级自助告警偏好：个人低余额阈值、API key 即将到期提前天数与通知通道';
