-- 站点公告从「一条」变成「一个列表」：settings.site_notice（单个对象）换成 settings.site_notices（数组，
-- 每条 { id, title, body, level, popup, enabled, updated_at }，见 crates/okapi-console/src/notices.rs）。
-- 原来那条有正文的话搬成列表里的第一条；没有正文的（从没发过公告）直接丢掉。
--   · updated_at 原样保留（客户端按它记「看过哪一版」，保留下来已经关掉旧横幅的人不会再被提醒一次）；
--     不是 ISO 时间的换成这一行的写入时间。
--   · 原来的公告是每页顶上的横幅，现在不弹窗的公告只在入口上亮一个红点——搬过来的这条设成弹窗，
--     还没看过的人照样会看到一次。
INSERT INTO settings(key, value, updated_by, updated_at)
SELECT 'site_notices',
       jsonb_build_array(jsonb_build_object(
           'id', 'n-' || substr(md5(value::text), 1, 12),
           'title', left(btrim(COALESCE(value->>'title', '')), 80),
           'body', left(btrim(value->>'body'), 4000),
           'level', CASE WHEN value->>'level' IN ('warning', 'critical') THEN value->>'level' ELSE 'info' END,
           'popup', true,
           'enabled', COALESCE(value->'enabled' = 'true'::jsonb, false),
           'updated_at', CASE
               WHEN value->>'updated_at' ~ '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?Z$' THEN value->>'updated_at'
               ELSE to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
           END)),
       updated_by, updated_at
  FROM settings
 WHERE key = 'site_notice' AND jsonb_typeof(value) = 'object' AND btrim(COALESCE(value->>'body', '')) <> ''
ON CONFLICT (key) DO NOTHING;

DELETE FROM settings WHERE key = 'site_notice';
