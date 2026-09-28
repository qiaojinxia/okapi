-- commit：结算（多退少补）+ 释放并发（docs/database.md §2.2）
-- KEYS[1] bal:{uid}  KEYS[2] conc:{uid}:k:<kid>
-- ARGV[1] request_id  ARGV[2] actual_micro  ARGV[3] api_key_id
-- 幂等：预扣字段不存在 → {0,'NO_RESERVATION'}（调用方转对账路径，不直接改余额）
-- 回到预扣所在池（字段第 4 段 pool；老格式缺省钱包）。

return close_reservation(KEYS[1], KEYS[2], ARGV[1], ARGV[3], ARGV[2], ARGV[4], ARGV[5])
