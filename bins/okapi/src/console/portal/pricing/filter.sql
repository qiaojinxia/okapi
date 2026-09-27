FROM models m JOIN model_pricing p ON p.model_id = m.id
WHERE m.status = 1
AND ($1::text IS NULL OR m.model_name ILIKE $1 ESCAPE E'\\'
    OR m.display_name ILIKE $1 ESCAPE E'\\' OR m.vendor ILIKE $1 ESCAPE E'\\')
AND ($2::text IS NULL OR COALESCE(lower(btrim(m.vendor)), '') = lower($2))
AND ($3::text IS NULL OR m.capabilities -> $3 = 'true'::jsonb)
AND ($4::text IS NULL OR m.model_name = $4)
AND (($5::text IS NULL AND $6::text IS NULL) OR EXISTS (
    SELECT 1 FROM channels c JOIN pool_channels pc ON pc.channel_id = c.id
    WHERE c.status = 1 AND c.deleted_at IS NULL AND c.models ? m.model_name
    AND ($5::text IS NULL OR EXISTS (
        SELECT 1 FROM price_groups g LEFT JOIN channel_pools cp ON cp.pool_code = g.pool_code
        WHERE g.group_code = $5 AND pc.pool_code IN (g.pool_code, cp.fallback_pool_code)
    ))
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
