-- Request attempts are durable; Redis restart must not reset a configured cap.
CREATE TABLE channel_usage_windows (
    channel_id BIGINT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    period VARCHAR(8) NOT NULL,
    window_start TIMESTAMPTZ NOT NULL,
    window_end TIMESTAMPTZ NOT NULL,
    requests BIGINT NOT NULL DEFAULT 0 CHECK (requests >= 0),
    PRIMARY KEY (channel_id, period, window_start),
    CHECK (period IN ('hour','day','week','month')),
    CHECK (window_end > window_start)
);
CREATE INDEX channel_usage_windows_expiry ON channel_usage_windows(window_end);
