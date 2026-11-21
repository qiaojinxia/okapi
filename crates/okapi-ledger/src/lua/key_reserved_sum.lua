-- key_reserved_sum：在 Redis 侧汇总单把 api_key_id 的普通在途预扣，
-- 避免限额 key 准入把整张 bal:{uid} 哈希表拖过网络（O(K) 网络传输 → O(1) 标量返回）。
-- KEYS[1] bal:{uid}
-- ARGV[1] target_api_key_id（字符串比较，覆盖 > 2^53 的 PG bigint）
local target_key = ARGV[1] or ''
if not ledger_key(target_key) or target_key == '0' then
    return {0, 'INVALID_RESERVATION'}
end
local total = 0
local all = redis.call('HGETALL', KEYS[1])
for i = 1, #all, 2 do
    if string.sub(all[i], 1, 2) == 'r:' then
        local receipt = ledger_reservation(all[i + 1])
        if not receipt or receipt.key == '0' or receipt.amount < 0 then
            return {0, 'ADMISSION_STATE_INVALID'}
        end
        if receipt.key == target_key then
            total = ledger_add(total, receipt.amount)
            if not total then
                return {0, 'ADMISSION_STATE_INVALID'}
            end
        end
    end
end
return {1, ledger_decimal(total)}
