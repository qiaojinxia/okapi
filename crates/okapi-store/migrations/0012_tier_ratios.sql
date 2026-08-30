-- service_tier 价格轴（IMPLEMENTATION §11.3，Sub2API 0.1.179/180 对齐）：
-- 模型级档位倍率表，如 {"flex": "0.5", "priority": "2.0"}；NULL = 全档 1.0。
-- 语义：有效 model_ratio = model_ratio × tier_ratio(结算档)；
-- 结算档 = 请求声明档与上游响应报告档中倍率较低者（只降不升）；
-- 未配置的档位名按 1.0（default/auto 天然如此）。
ALTER TABLE model_pricing
    ADD COLUMN IF NOT EXISTS tier_ratios JSONB;
