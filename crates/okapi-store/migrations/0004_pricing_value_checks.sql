-- 定价值域守卫：把会让整本价簿编译失败的脏值拦在写入这一步。
--
-- 控制面的写入口早就校验了这些值，但直写库 / 导入 / 手工 SQL 绕得过去：负的按次单价、
-- 非正的专属绝对价一旦进表，下一次发布被拒，或装载快照时整本价簿装不上。
-- NOT VALID：只约束之后的新写入和被更新的行，不因历史脏行让迁移本身失败（需要的话事后
-- 清掉脏行再 VALIDATE CONSTRAINT）。
ALTER TABLE model_pricing ADD CONSTRAINT model_pricing_per_call_nonnegative
    CHECK (per_call_price_micro IS NULL OR per_call_price_micro >= 0) NOT VALID;
ALTER TABLE user_pricing ADD CONSTRAINT user_pricing_absolute_prices_valid
    CHECK ((custom_input_per_1m_micro IS NULL OR custom_input_per_1m_micro > 0)
       AND (custom_output_per_1m_micro IS NULL OR custom_output_per_1m_micro >= 0)) NOT VALID;
