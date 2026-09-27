-- Failed users must not monopolize a bounded maintenance/recovery batch.
ALTER TABLE user_subscriptions ADD COLUMN maintenance_retry_after TIMESTAMPTZ;
ALTER TABLE subscription_grants ADD COLUMN retry_after TIMESTAMPTZ;
ALTER TABLE subscription_sync ADD COLUMN retry_after TIMESTAMPTZ;
