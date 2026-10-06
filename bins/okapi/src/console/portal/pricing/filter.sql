FROM jsonb_to_recordset($8::jsonb -> 'models') AS p(
    model_name text, pricing_mode text, model_ratio_scaled bigint,
    completion_ratio_scaled bigint, cache_ratio_scaled bigint, cache_write_ratio_scaled bigint,
    audio_ratio_scaled bigint, audio_completion_ratio_scaled bigint, image_ratio_scaled bigint,
    modality_ratios jsonb, server_tool_prices jsonb, per_call_price_micro bigint
)
JOIN models m ON m.model_name = p.model_name
WHERE m.status = 1
AND ($1::text IS NULL OR m.model_name ILIKE $1 ESCAPE E'\\'
    OR m.display_name ILIKE $1 ESCAPE E'\\' OR m.vendor ILIKE $1 ESCAPE E'\\')
AND ($2::text IS NULL OR COALESCE(lower(btrim(m.vendor)), '') = lower($2))
AND ($3::text IS NULL OR m.capabilities -> $3 = 'true'::jsonb)
AND ($4::text IS NULL OR m.model_name = $4)
AND ($9::text IS NULL OR p.pricing_mode = $9)
AND ($11::text[] IS NULL OR regexp_replace(lower(btrim(m.vendor)), '[[:space:]._-]+', '', 'g') = ANY($11))
AND (($5::text IS NULL AND $6::text IS NULL AND NOT $10::boolean) OR EXISTS (
    SELECT 1 FROM channels c JOIN pool_channels pc ON pc.channel_id = c.id
    WHERE c.status = 1 AND c.deleted_at IS NULL AND c.models ? m.model_name
    AND EXISTS (
        SELECT 1 FROM price_groups g LEFT JOIN channel_pools cp ON cp.pool_code = g.pool_code
        JOIN jsonb_to_recordset($8::jsonb -> 'groups') AS pg(group_code text)
            ON pg.group_code = g.group_code
        WHERE (COALESCE($5::text, CASE WHEN $10::boolean THEN $12::text END) IS NULL
            OR g.group_code = COALESCE($5::text, CASE WHEN $10::boolean THEN $12::text END))
        AND (g.self_select OR g.is_default OR EXISTS (
            SELECT 1 FROM user_groups ug WHERE ug.group_code = g.group_code AND ug.user_id = $7
        ))
        AND pc.pool_code IN (g.pool_code, cp.fallback_pool_code)
    )
    AND ($6::text IS NULL OR CASE $6
        WHEN '/v1/responses' THEN TRUE
        WHEN '/v1/responses/compact' THEN
            (c.provider = 'codex'
             OR (c.provider = 'openai' AND COALESCE((c.settings ->> 'responses_native')::boolean, TRUE))
             OR (c.provider = 'openai_compat' AND COALESCE((c.settings ->> 'responses_native')::boolean, FALSE)))
            AND (c.capabilities -> 'compact') IS DISTINCT FROM 'false'::jsonb
        WHEN '/v1/messages' THEN c.provider NOT IN ('codex', 'gemini')
            AND (c.provider <> 'vertex' OR lower(COALESCE(c.model_mapping ->> m.model_name, m.model_name)) LIKE 'claude%')
        ELSE c.provider <> 'codex'
    END)
))
