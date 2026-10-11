-- 每请求 / 每个维护 tick 都要跑的查询，补上能被计划器用上的索引。

-- quota_limited 的 key 每次预扣都要汇总这把 key 的在途持久预扣（ledger/key_budget.rs）。
-- 现有部分索引按 user_id 打头，按 api_key_id 过滤只能扫全部未关闭的预扣。
CREATE INDEX balance_holds_key_open ON balance_holds (api_key_id)
    INCLUDE (maximum_micro) WHERE state IN ('pending', 'held');

-- 建图任务入队时核算全局与单用户容量（store/image_tasks.rs）：覆盖列让它走仅索引扫描。
CREATE INDEX image_tasks_capacity ON image_tasks (user_id) INCLUDE (status, storage_budget);

-- 订阅维护按「窗口到期 OR 订阅到期」扫活跃订阅（store/subscriptions.rs）。只有 window_end
-- 有部分索引时 OR 会退化成全表扫（含只增不减的历史订阅）；两边都有索引才能 BitmapOr。
CREATE INDEX idx_user_sub_expiry ON user_subscriptions (expires_at) WHERE status = 1;

-- 订阅历史页按用户倒序翻页。
CREATE INDEX idx_user_sub_history ON user_subscriptions (user_id, id DESC);

-- billing_outbox 写入后很快删除，死元组翻新快：更积极地 autovacuum，并给 HOT 更新留页内空间，
-- 避免表膨胀拖慢每秒一次的认领查询。
ALTER TABLE billing_outbox SET (autovacuum_vacuum_scale_factor = 0.05, fillfactor = 70);
