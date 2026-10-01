# 字符与 Token 单位核对

后续 Token 速度分母已在 [输出速度单位核对](output-rate-unit-audit.md) 接入并验证；本页保留字符计费阶段的独立证据。旧主 Token 校准与界面统计消费仍待收尾。

日期：2026-09-30。本阶段完成新 TTS 字符计费与 Token 单位分离、长期字符数量/覆盖和相关接口核对。未修改前端布局、历史财务金额或生产服务，不能据此认定所有指标正确。

## 已复现的问题与修正

实际 HTTP 复现：`hello world` 11 个字符收费 22 micro 正确，但旧账单 `prompt_tokens=11`，错误参与 TPM 预估、实时计数和 Token 累计。基线 `/tmp/okapi-character-unit-baseline.log` 实际退出 **101**（1 failed、2 filtered），始终保留原收费断言。

独立 `calculate_characters` 复用既有定点价格链，保持 ratio/tiered/per_call、基准价、service tier、分组、个人倍率、规则及四金额取整结果。字符只在报价域作为计价数量；快照明确 `input_unit=characters`、`input_characters=N`。离开报价域后，成功 TTS 的 TokenUsage 为零，预估 Token 为零，字符仍计请求、并发、花费和余额，不占 TPM 或 Token 累计规则。数量按既有 Rust `chars()` 的 Unicode scalar 口径；没有改成字节数或视觉字形数。

PG 使用详情、快照和 outbox 保存同一单位/数量，CH 新增独立字段；管理和个人日志与汇总提供相同语义。账本拒绝缺失/负数/非法/越界数量，以及字符与非零 Token 并存。没有新元数据的旧数据保持未知；显式上游 Token 零、本地估算或缓存/细分采集证据可以确认 Token 单位，空的来源对象不能确认。

严格检查还发现既有网关失败上下文达到 160 字节。将仅失败时保存的上游模型/端点二元组装箱，缩小错误返回结构；失败回复、退款、重试分类与记录内容保持原语义。用既有 HTTP/WS 错误、重试与结算回归验证，不能通过关闭 `result_large_err` 警告规避。

## 长期统计与覆盖

独立 `mv_input_units_5min` 无 raw TTL，沿完整分析维度保存请求数、有效字符数量/请求和明确 Token 单位请求。按粒度和请求覆盖择一读取 MV 或 raw，数量大于基础请求数的来源无效；禁止相加、回填旧 MV 或 POPULATE。单位覆盖与 Token 来源覆盖独立，已知 Token 单位不等于供应商实报。

`input_units` 返回 `observed_characters`、字符/Token/未知请求数与 `coverage_bp`；全部请求单位已知时才返回全范围 `characters`。明确字符零、只有明确 Token 请求的字符零和未知历史分别处理。升级时 raw 可恢复旧字符数量且不重复；raw 过期后保留新聚合子集，缺失历史保持未知，不把新记录的零解释为历史总零。管理趋势、分析拆分/实体/端点过滤、个人图表/活动及 PG 日志汇总共用语义。

## 验证证据与隔离范围

- `okapi-pricing/tests/characters.rs`：3 项，固定 ASCII/零/tier/per_call 数值、完整修饰链四金额，以及 0..u32::MAX 的金额等价性质；普通 Token 快照不添加单位字段。
- `gateway_audio.rs`：7 项，ASCII、中文/emoji/组合字符、明确空输入、TPM=1 允许 11 字符、Token 规则累计为零、二进制透传、PG/outbox/CH 四金额一致、去重重放、管理/个人日志与趋势；转写/翻译原按次收费保留，未记录 Token 单位保持未知。
- `support/input_unit_statistics.rs`：2 项，通过实际隔离 HTTP/CH 验证字符零/未知/坏元数据、跨主体和端点/特殊模型名过滤、聚合升级、明细过期、重复 schema 与重放。坏元数据不凭空改基础请求、Token 或金额。
- 账本和 CH sink 的纯函数边界覆盖缺字段、null、字符串、负数、数量溢出、显式零与遗留 speech 记录；遗留记录不根据模型名或正数猜为字符。

冻结副本 1：`/tmp/okapi-character-unit-snapshot-20260930`，1,168 个后端/构建文件，复制前后相同；完整财务三包 `/tmp/okapi-character-unit-snapshot-financial.log` 实际退出 **0**，**188 passed、0 failed/ignored/filtered/软跳过**，包含 5 项既有对拍。报告 `...-financial-report.json` 没有解析错误或未结束块。包含并行聊天新增的 3 项缓存 TTL 价格回归，只代表这些夹具通过，不代表 TTL 全链路或真实供应商验收。

冻结副本 2：`/tmp/okapi-character-unit-style-snapshot-20260930`，仍为 1,168 文件；与副本 1 仅三项等价样式/借用修正不同：TTL 测试按引用传值、SQL 字符串移除多余定界符、整数分隔符。`/tmp/okapi-character-unit-style-snapshot-related.log` 实际退出 **0**：单次 **186 passed、0 failed/ignored/filtered/软跳过**，8 个完整块（lib 95、分析 17、日志 15、门户 9、统计 28、音频 7、价格规则 12、CH 投递 3）。报告 `...-related-report.json` 无解析错误或未结束块，包括全部三项此前受资源限制失败的原用例。

冻结副本 3：`/tmp/okapi-character-unit-final-snapshot-20260930`，1,169 文件，复制前后相同，编译容器 `sha256sum --check` 一致。包含网关错误上下文装箱及并行聊天的模型校验拆分、价格/元数据校验单测和 capability 白名单复用；后两者不作为字符统计实现。严格检查消除了原 8 项警告，但新测试辅助函数按值传参仍退出 101。

最终副本 4：`/tmp/okapi-character-unit-verified-snapshot-20260930`，1,169 文件，仅将副本 3 的新模型测试辅助函数改为实际消费 JSON 对象，断言和生产代码不变。四份副本收尾 SHA-256 均未变化，字符阶段收尾时的后端源码/构建文件与副本 4 完全相同，编译容器校验通过。

- 严格 workspace/all-targets Clippy：`/tmp/okapi-character-unit-verified-snapshot-clippy.log`，实际退出 **0**。
- 模型目录/写入、网关 HTTP/WS 失败/退款/重试与 lib：`...-errors.log`，实际退出 **0**，单次 **158 passed、0 failed/ignored/filtered/软跳过**，8 个完整块（lib 97、目录 15、价格写入 5、M1 6、流中断 1、原生 WS 14、WS 桥接 15、重试 5），报告无解析错误或未结束块。
- 配置原子保存/清空/保留/回滚：`...-store.log` 实际退出 **0**，2 passed；SQLx 自建自清隔离测试库。
- 当前格式、金额浮点守卫、错误码守卫、API 源清单、差异空白检查通过；汇总器/API 清单 Python 单测 16 项通过。

188、186、158、2 及 16 分别是独立运行，不能相加为“完整工作区测试通过”。本轮字符报价、账本、音频采集、CH 投递及统计实现与对应 188/186 运行文件指纹一致；后加的模型校验/目录变化和失败上下文由 158/2 运行补验。本轮没有再跑整个工作区所有测试或供应商真实账单。

隔离 PG 数据库 `okapi_character_units_20260930` 从空库执行正式迁移（含 0027）；独立 Redis 和 CH 实例，不重置共享开发数据。最终测试 CH 资源为 4GiB、`max_threads=2`、`max_insert_threads=1`。这些是小规模正确性夹具，未验证大规模查询性能。

## 保留的失败与限制

早期编译两次退出 101：新增字段使大 JSON 宏递归超限，改用等价辅助写入解决，未增加递归限额。早期严格检查的函数行数、局部导入与借用警告按等价提取/格式修正，未关闭警告。

同目录另一聊天新增并修改 0027，造成运行中的旧测试二进制 `VersionMissing(27)` / `VersionMismatch(27)`。失败保留为 `/tmp/okapi-character-unit-related-final.log`、`...-media-evidence-final.log`；另一次财务编译因其新测试误用 `Quote.cost` 退出 101，已协调改用 `amount`。没有改迁移历史、删除版本记录或重置共享数据库。

副本 1 的关联回归先受 768MiB 配额 OOM，之后 Docker 调高额度但 CH 未重启仍沿用 691MiB 内部限额；两次分别实际退出 101。2GiB 复验 `/tmp/okapi-character-unit-snapshot-related-third.log` 实际退出 **101**，**161 passed、3 failed**；三项接口 500 对应 CH `Code 241`，预算 1.80GiB，申请约 1.82GiB，其余目标未执行。错误后的测试库删除还造成一个次生 UNKNOWN_TABLE。保留原断言，在自己创建的测试服务上配置 4GiB/两线程后重跑。

旧 TTS 可能已在主 Token 聚合中混入字符，本阶段没有重写或宣称修复这段历史。历史识别需要精确 speech 端点及旧计价契约证据，不能猜模型名/费率；缺失维度或已过期记录保持未知。历史主 Token 校准、界面速度合桶/未知显示、独立 TTL/工具费用、估算偏差、真实供应商账单与持续写入/大规模性能仍需继续核对。
