-- Existing rows use the best available plan at upgrade time. Previously edited
-- terms cannot be reconstructed; new purchases and grants freeze their own terms.
CREATE FUNCTION subscription_plan_snapshot(p plans) RETURNS jsonb
LANGUAGE SQL IMMUTABLE AS $$
    SELECT CASE WHEN p.kind=1 THEN jsonb_build_object(
        'id',p.id,'plan_code',p.plan_code,'display_name',p.display_name,
        'quota_micro',p.grant_micro,'group_code',p.group_code,'price_micro',p.price_micro,
        'period',p.period,'duration_days',p.duration_days,'sort_order',p.sort_order,
        'description',p.description) END
$$;
ALTER TABLE user_subscriptions
    ADD COLUMN plan_code_snapshot VARCHAR(64),
    ADD COLUMN display_name_snapshot VARCHAR(128),
    ADD COLUMN period_snapshot SMALLINT CHECK (period_snapshot IN (1,2,3)),
    ADD COLUMN group_code_snapshot VARCHAR(64);
UPDATE user_subscriptions s SET plan_code_snapshot=p.plan_code,
    display_name_snapshot=p.display_name,period_snapshot=p.period,group_code_snapshot=p.group_code
    FROM plans p WHERE p.id=s.plan_id;
ALTER TABLE user_subscriptions
    ALTER COLUMN plan_code_snapshot SET NOT NULL,
    ALTER COLUMN display_name_snapshot SET NOT NULL,
    ALTER COLUMN period_snapshot SET NOT NULL;
ALTER TABLE recharge_orders ADD COLUMN subscription_snapshot JSONB;
ALTER TABLE redemption_codes ADD COLUMN subscription_snapshot JSONB;
UPDATE recharge_orders o SET subscription_snapshot=subscription_plan_snapshot(p)
    FROM plans p WHERE p.id=o.plan_id AND p.kind=1;
UPDATE redemption_codes r SET subscription_snapshot=subscription_plan_snapshot(p)
    FROM plans p WHERE p.id=r.plan_id AND p.kind=1;
