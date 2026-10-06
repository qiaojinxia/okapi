-- 出口代理二期（IMPLEMENTATION §11.41）：单代理并发上限、出口 IP 变化记录。

ALTER TABLE proxies
    ADD COLUMN max_concurrency    INT,
    ADD COLUMN previous_exit_ip   VARCHAR(64),
    ADD COLUMN exit_ip_changed_at TIMESTAMPTZ,
    ADD CONSTRAINT proxies_max_concurrency_chk CHECK (max_concurrency IS NULL OR max_concurrency > 0);
COMMENT ON COLUMN proxies.max_concurrency IS
    '经该代理同时在途的上游请求数上限（跨 key、跨副本，Redis 租约 conc:px:*）；null = 不限';
COMMENT ON COLUMN proxies.exit_ip_changed_at IS
    '最近一次测试 / 后台探测发现出口 IP 与上次不同的时刻；previous_exit_ip 是变化前的 IP';

-- 选代理时一并带出并发上限，候选拿到就能在准入时占租约（返回列变了，只能先删再建）
DROP FUNCTION egress_pick(BIGINT, BIGINT, BOOLEAN);
CREATE FUNCTION egress_pick(p_channel BIGINT, p_key BIGINT, healthy_only BOOLEAN)
RETURNS TABLE (mode TEXT, proxy_id BIGINT, url_ciphertext BYTEA, max_concurrency INT)
LANGUAGE sql VOLATILE AS $$
    SELECT e.mode, px.id, px.url_ciphertext, px.max_concurrency
    FROM channel_egress e
    LEFT JOIN proxy_groups g ON e.mode = 'group' AND g.code = e.group_code
    LEFT JOIN channel_keys ck ON ck.id = p_key AND ck.channel_id = e.channel_id
    LEFT JOIN LATERAL (
        SELECT p.id, p.url_ciphertext, p.max_concurrency
        FROM proxies p
        LEFT JOIN proxy_group_members m ON m.group_code = g.code AND m.proxy_id = p.id
        WHERE p.status = 1
          AND (NOT healthy_only OR p.cooldown_until IS NULL OR p.cooldown_until <= now())
          AND ((e.mode = 'proxy' AND p.id = e.proxy_id)
            OR (g.mode = 'pinned' AND p.id = ck.egress_proxy_id AND m.proxy_id IS NOT NULL)
            OR (g.mode = 'rotate' AND m.proxy_id IS NOT NULL))
        ORDER BY (p.cooldown_until IS NOT NULL AND p.cooldown_until > now()),
                 m.priority DESC NULLS LAST,
                 -ln(1 - random()) / GREATEST(COALESCE(m.weight, 1), 1)
        LIMIT 1
    ) px ON true
    WHERE e.channel_id = p_channel
$$;
