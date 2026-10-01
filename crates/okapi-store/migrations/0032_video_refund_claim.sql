-- Claim terminal outcomes durably before calling the ledger; retry interrupted refunds.
ALTER TABLE video_tasks DROP CONSTRAINT video_tasks_state_check;
ALTER TABLE video_tasks ADD CONSTRAINT video_tasks_state_check
    CHECK (state IN ('pending','refund_pending','completed','refunded'));
DROP INDEX idx_video_tasks_poll;
CREATE INDEX idx_video_tasks_poll ON video_tasks(next_poll_at)
    WHERE state IN ('pending','refund_pending');
