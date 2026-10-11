-- 倍率列不收负数：RatioFp::from_scaled 对负值返回 None，读路径只能退回全价（用户倍率）或
-- 告警跳过该行（价簿），坏值从源头挡住。0 是合法的"免单"语义，所以是 >= 0 而不是 > 0。
-- tier_ratios / modality_ratios 是 jsonb，由 Rust 侧逐项 fail-closed 校验，不在这里约束。
ALTER TABLE users
    ADD CONSTRAINT users_price_multiplier_nonnegative CHECK (price_multiplier >= 0);
ALTER TABLE user_pricing
    ADD CONSTRAINT user_pricing_ratios_nonnegative CHECK (
        (custom_model_ratio IS NULL OR custom_model_ratio >= 0)
        AND (custom_completion_ratio IS NULL OR custom_completion_ratio >= 0)
        AND (custom_cache_ratio IS NULL OR custom_cache_ratio >= 0)
        AND (custom_cache_write_ratio IS NULL OR custom_cache_write_ratio >= 0)
    );
ALTER TABLE model_pricing
    ADD CONSTRAINT model_pricing_ratios_nonnegative CHECK (
        (model_ratio IS NULL OR model_ratio >= 0)
        AND completion_ratio >= 0
        AND cache_ratio >= 0
        AND cache_write_ratio >= 0
        AND audio_ratio >= 0
        AND audio_completion_ratio >= 0
        AND image_ratio >= 0
    );
ALTER TABLE price_groups
    ADD CONSTRAINT price_groups_group_ratio_nonnegative CHECK (group_ratio >= 0);
