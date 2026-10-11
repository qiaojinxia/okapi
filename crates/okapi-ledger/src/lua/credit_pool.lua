-- credit_pool：指定池入账（钱包 avail / 订阅 sub），守住全账本的 |值| ≤ 2^53−1 不变式。
-- KEYS[1] bal:{uid}
-- ARGV[1] pool_field —— 'avail' 或 'sub'
-- ARGV[2] delta_micro —— 有符号规范十进制
-- 返回入账后的余额（字符串，避免 Lua double 回传取整）。
-- 此前是裸 HINCRBY：余额一旦被推过 2^53，reserve / commit 全部 fail-closed，只能对账修复。
if ARGV[1] ~= 'avail' and ARGV[1] ~= 'sub' then return redis.error_reply('invalid_pool') end
local delta = ledger_integer(ARGV[2], true)
local balance = ledger_integer(redis.call('HGET', KEYS[1], ARGV[1]) or '0', true)
if not delta or not balance then return redis.error_reply('invalid_balance') end
local after = ledger_add(balance, delta)
if not after then return redis.error_reply('balance_out_of_range') end
redis.call('HSET', KEYS[1], ARGV[1], ledger_decimal(after))
return ledger_decimal(after)
