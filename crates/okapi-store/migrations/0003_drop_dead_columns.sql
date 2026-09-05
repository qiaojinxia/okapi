-- 删两列建了从没接线的字段。
--
-- channel_keys.quota_snapshot：原意是"被动采集上游 rate-limit 响应头"，采集侧
--   一行代码都没写过，全仓库零引用、库内零非空行。真要做这件事时，形状也未必
--   还是当初设想的 JSONB——留着只会让读 schema 的人以为已经有了。
-- model_pricing.media_prices：媒体计费最终走了 per_call 与 tier_expr 两条路，
--   这一列被绕开，同样零引用零数据。
--
-- 两列都无索引、无外键、无 CHECK，DROP 只改 catalog，不重写表。
-- 复活的防线在 bins/okapi/tests/schema_shape.rs 的 DROPPED_COLUMNS。

ALTER TABLE channel_keys  DROP COLUMN IF EXISTS quota_snapshot;
ALTER TABLE model_pricing DROP COLUMN IF EXISTS media_prices;
