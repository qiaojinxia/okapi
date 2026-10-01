-- Persist task ownership and the original charge alongside video settlement.
CREATE TABLE video_tasks (
    user_id BIGINT NOT NULL REFERENCES users(id),
    task_id TEXT NOT NULL,
    request_id UUID NOT NULL UNIQUE,
    channel_key_id BIGINT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','completed','refunded')),
    next_poll_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, task_id)
);
CREATE INDEX idx_video_tasks_poll ON video_tasks(next_poll_at) WHERE state='pending';
