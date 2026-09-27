-- refund：全额释放预扣（上游失败/空回复不计费路径）+ 释放并发
-- KEYS[1] bal:{uid}  KEYS[2] conc:{uid}:k:<kid>
-- ARGV[1] request_id  ARGV[2] api_key_id
-- 幂等：重复调用返回 {1,'0',avail,0}
-- 回到预扣所在池（字段第 4 段 pool；老格式缺省钱包）。

return close_reservation(KEYS[1], KEYS[2], ARGV[1], ARGV[2], nil)
