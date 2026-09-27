-- A persistent per-user Redis high-water mark prevents a timed-out EVAL from
-- applying again after its per-operation receipt has been cleaned up.
ALTER TABLE fund_transfers ADD COLUMN sequence BIGSERIAL UNIQUE CHECK (sequence > 0);
CREATE INDEX fund_transfers_user_sequence ON fund_transfers(user_id,sequence);
