-- 数据量上来后才显形的扫描：每条都是周期任务或页面的固定查询。

-- 对账增量轮按 created_at 取近期有事件的用户。追加写入、按时间单调，BRIN 只有几个页，几乎不加写入成本。
CREATE INDEX idx_be_created_brin ON billing_events USING brin (created_at);

-- 资金流入概要只看充值 / 调整 / 过期三类，在消费事件里是极少数；部分索引避免扫窗口内全部事件。
CREATE INDEX idx_be_cashflow ON billing_events (created_at)
    WHERE event_type IN ('recharge', 'adjust', 'expire');

-- chsink 每秒认领未入批的 outbox 行。谓词与 admit_outbox 两臂互证（status=0 待发布 / 已发布待入批），
-- 让计划器在任何统计下都走索引按 id 取前 N 条，而不是退化成全表扫。
CREATE INDEX idx_outbox_ch_unassigned ON billing_outbox (id)
    WHERE ch_batch_id IS NULL AND (status = 0 OR (status = 1 AND stats_protocol = 1));

-- 清理 CH 批次时外键要查 DLQ 有无引用；没有这条索引每删一批都全表扫一次 DLQ。
CREATE INDEX idx_dlq_ch_batch ON billing_dlq (ch_batch_id) WHERE ch_batch_id IS NOT NULL;
