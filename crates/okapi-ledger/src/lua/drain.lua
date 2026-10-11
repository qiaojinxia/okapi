-- drain：余额有效期到期清零，原子取出全部可用余额（在途预扣不动）。
-- KEYS[1] bal:{uid}
-- 返回取出的金额（字符串；avail ≤ 0 时不动返回 '0'）。
-- 余额先按规范十进制校验：坏值报错而不是按 double 取整后记一笔失真的过期事件。
local balance = ledger_integer(redis.call('HGET', KEYS[1], 'avail') or '0', true)
if not balance then return redis.error_reply('invalid_balance') end
if balance <= 0 then return '0' end
redis.call('HSET', KEYS[1], 'avail', '0')
return ledger_decimal(balance)
