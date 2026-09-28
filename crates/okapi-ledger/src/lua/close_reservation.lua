-- Redis cannot roll back a script after an error. Preflight every value and
-- arithmetic result before changing money, deleting the receipt or releasing a slot.
local function close_reservation(balance_key, concurrency_key, id, key, actual_raw, expected_pool, expected_epoch)
    local field = 'r:' .. id
    local raw = redis.call('HGET', balance_key, field)
    if not raw then
        if actual_raw then return {0, 'NO_RESERVATION'} end
        local balance = redis.call('HGET', balance_key, 'avail') or '0'
        if not ledger_integer(balance, true) then return {0, 'SETTLEMENT_STATE_INVALID'} end
        return {1, '0', balance, 0, 0}
    end
    local receipt = ledger_reservation(raw)
    if not receipt then return {0, 'SETTLEMENT_STATE_INVALID'} end
    if receipt.key ~= key then return {0, 'RESERVATION_CONFLICT'} end
    if expected_pool and expected_pool ~= ledger_decimal(receipt.pool) then return {0, 'RESERVATION_CONFLICT'} end
    local actual = 0
    if actual_raw then
        actual = ledger_integer(actual_raw, false)
        if not actual then return {0, 'INVALID_SETTLEMENT'} end
    end
    local concurrency = ledger_integer(redis.pcall('GET', concurrency_key) or '0', false)
    local balance = ledger_integer(redis.call('HGET', balance_key, receipt.field) or '0', true)
    if not concurrency or not balance then return {0, 'SETTLEMENT_STATE_INVALID'} end
    local epoch = redis.call('HGET', balance_key, 'sub_epoch') or ''
    if epoch ~= '' and not ledger_epoch(epoch) then return {0, 'SETTLEMENT_STATE_INVALID'} end
    if expected_epoch then
        if receipt.epoch and receipt.epoch ~= expected_epoch then return {0, 'RESERVATION_CONFLICT'} end
        if not receipt.epoch then
            if expected_epoch ~= epoch then return {0, 'RESERVATION_CONFLICT'} end
            receipt.epoch = expected_epoch
        end
    end
    local delta = receipt.amount - actual
    if receipt.epoch and receipt.epoch ~= epoch then delta = 0 end
    if not ledger_add(balance, delta) then return {0, 'SETTLEMENT_STATE_INVALID'} end
    local delta_raw = ledger_decimal(delta)
    redis.call('HINCRBY', balance_key, receipt.field, delta_raw)
    redis.call('HDEL', balance_key, field)
    if concurrency > 0 then redis.call('DECR', concurrency_key) end
    return {1, delta_raw, redis.call('HGET', balance_key, receipt.field), receipt.pool, 1}
end
