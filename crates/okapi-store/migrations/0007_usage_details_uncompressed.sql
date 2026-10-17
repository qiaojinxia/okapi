-- usage_details 每行约 1.3KB，默认 EXTENDED 存储会被 pglz 压到 ~800 字节留在行内。
-- 用户日志汇总（portal/usage_logs.rs 的 stat）每行要引用它上百次，压缩值每引用一次就解压一次：
-- 4 千行的汇总单核要 5 秒多，改成不压缩后约 2 秒（与查询侧的路径预提取叠加约 1.3 秒），结果逐字节一致。
-- EXTERNAL = 行内不压缩，超过 TOAST 阈值（~2KB）才外置；多占的几百字节相对计费行本身可以忽略。
-- 分区父表上改会递归到现有分区，之后 PARTITION OF 建的新分区也继承这一设置。
ALTER TABLE billing_records ALTER COLUMN usage_details SET STORAGE EXTERNAL;

-- 存储方式只影响新写入的值：已有的压缩值重写一遍（|| '{}' 内容不变，但会生成新值、按新方式存）。
UPDATE billing_records SET usage_details = usage_details || '{}'::jsonb
WHERE pg_column_compression(usage_details) IS NOT NULL;
