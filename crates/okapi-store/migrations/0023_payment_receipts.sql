-- Preserve the quoted payment contract; legacy orders cannot recover a session
-- identifier that was never stored. Only new orders use contract version 1.
ALTER TABLE recharge_orders
    ADD COLUMN payment_contract_version SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN merchant_id VARCHAR(128),
    ADD COLUMN checkout_session_id VARCHAR(128);
CREATE UNIQUE INDEX recharge_checkout_session_unique
    ON recharge_orders (gateway, checkout_session_id)
    WHERE checkout_session_id IS NOT NULL;
CREATE INDEX recharge_paid_transaction_lookup
    ON recharge_orders (gateway, gateway_trade_no)
    WHERE status IN (1, 3);

-- Claim the provider transaction in the same transaction as the order and
-- durable financial intent. A per-user lock alone cannot prevent cross-user reuse.
CREATE TABLE payment_receipts (
    gateway VARCHAR(32) NOT NULL,
    merchant_id VARCHAR(128) NOT NULL,
    trade_no VARCHAR(128) NOT NULL,
    order_id BIGINT NOT NULL UNIQUE REFERENCES recharge_orders(id),
    currency VARCHAR(8) NOT NULL,
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (gateway, merchant_id, trade_no)
);
