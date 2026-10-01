# 历史 speech 单位校准（进行中）

2026-09-30。主统计与迟到 CH 投递的历史字符校准已按下述固定源码验证；整个计费统计链路仍在核对，日志/MCP 等未完成项见末尾。原收费、财务记录和已存在的主聚合保持原值，统计读取通过独立长期校准证据排除可确认的字符数量。

最终接受副本为 `/tmp/okapi-historical-speech-primary-v5-snapshot-20260930`，1,175 个后端/构建文件。本机副本、容器运行前后 SHA-256 和最终工作区指纹一致。单次完整关联回归 `/tmp/okapi-historical-speech-primary-v5-related_retry.log` 实际退出 0、**211 passed**，11 个完整块，无 failed/ignored/filtered/软跳过或解析错误；其中统计 34 项全部通过，包含四个新增历史字符场景及此前两项超时回归。覆盖 lib、分析、日志、门户、统计、音频、透传、报价版本、价格规则、视频与 worker。现有日志回归通过并不表示尚未接入的旧载体归一化已经完成。

同一副本财务三包（含 parity） `/tmp/okapi-historical-speech-primary-v5-financial.log` 实际退出 0、**188 passed**、17 个完整块；store 单测 **38 passed** 单列。workspace/all-targets 严格 Clippy 实际退出 0，格式、浮点守卫、错误码/API 清单和差异空白通过，汇总器/API 清单 Python 单测 16 项通过。三组计数不相加为 API 端点覆盖；79 个 domain/pricing/ledger 文件与上一价格版本阶段一致，本阶段未修改收费公式或账本写入。没有前端修改/构建、提交、推送、部署或共享服务重启。

识别依赖旧网关的明确端点和快照契约，不能用模型名、金额或费率猜单位：精确 `/v1/audio/speech`，非流式消费记录，快照版本和分组一致，ratio/tiered/per_call 的旧快照结构可解释，输入单位字段缺失，上游用量/来源没有 Token 证据，除旧 prompt 数量外的所有 Token 轴为零。数量必须是合法整数；新显式单位和冲突证据保留未知，不归为字符零。保留识别依据和数量便于追溯。

CH 新建无 TTL 校准证据与分页进度表，worker 对仍保留的历史候选逐页读取、确认和幂等写入；进度只能在证据写入成功后前进。读取和后续重放使用相同识别规则。统计查询按原时间、用户/密钥、模型/渠道、分组及高级维度范围匹配排除数量，输入/总 Token、来源、单位与排行/流向保持一致；校准只覆盖有证据的记录，未保存且已过期的明细不能补造历史。

新增实际 HTTP 回归先对照 200 Token + 11 旧字符，要求 Token 总数 200、字符 11、请求 2、金额 1,022 micro；后续需要验证重跑/批次重放、原明细删除、升级缺口、异常证据、不同模型/用户/密钥/时间/维度隔离、Token 排行和 PG/CH 日志读取的一致性。全部实际退出码、固定源码、失败原因和最终验证范围在执行后记录。

已执行的中间证据（尚非最终验收）：主趋势最初实际失败，输入 111 应为 100（`/tmp/okapi-old-speech-baseline-2.log`，退出 101）；校准后单项通过（`/tmp/okapi-old-speech-query-first.log`，退出 0）。分页/断点/重复执行与 raw 删除后的证据保留单项通过，2 个证据身份、3 条有效请求、22 字符（`/tmp/okapi-old-speech-evidence-first.log`，退出 0）。扩展首页总览回归实际失败：Token 总数 211 应为 200，unknown 输入来源仍含 11 个字符（`/tmp/okapi-old-speech-primary-views-baseline.log`，退出 101）；相关读取正在修正。此前识别器 2 单测通过后又补充了嵌套报告/模态冲突规则，必须重新验证，不能沿用旧结果作为新源码证明。

本阶段第二轮进展：四项历史 API 回归在 `/tmp/okapi-old-speech-current-four.log` 实际退出 0、4 passed；共享 store 单元测试在固定源码副本中实际退出 0、38 passed。四项覆盖主趋势、首页/模型/客户端/分组/个人总量、来源、速度、原明细删除、分页与重复、部分升级缺口及 Token 排名/流向/特殊模型名筛选。首个跨接口修正运行的失败包含测试误查 entity-usage 不存在的 tokens 字段，随后按原接口核对金额、单位和来源；第二个运行实际暴露个人总计仍含 11 字符，已改为校准后求和。部分升级速度覆盖也曾实际失败（1 应为 2），随后保留独立字符证据的残差。

固定副本 `/tmp/okapi-historical-speech-primary-snapshot-20260930` 的严格 Clippy 实际退出 0，完整关联测试仍未通过：错误目标 `console_analysis` 导致首个命令退出 101，修正为真实目标 `console_analytics` 后前四个目标分别完成 98/17/15/9，但统计阶段的历史日缓存回归发生 HTTP 500。为先定位，已终止自己的统计测试子进程，关联命令实际退出 101、统计块不完整，不能记为 191 项通过。单独复现 `/tmp/okapi-historical-speech-cache-regression.log` 实际退出 101。自有 CH query_log 确認 exception_code=159，分析阶段耗时超过现有 15 秒护栏；清理测试库之后在其他未完成查询里出现的 UNKNOWN_TABLE 是后续现象。

查询优化进行中：以完整当前/上期窗口和基础归属范围的实时稀疏探针确认没有校准证据，才跳过校准查询和表达式；不缓存“没有证据”的判断，不放宽超时。第一轮只跳过 JOIN 仍实际退出 101；继续检查并消除无证据时的残差与汇总表达式展开。全部回归和最终源码指纹必须在修复后重新核对。

后续单项 `/tmp/okapi-historical-speech-cache-optimized-third.log` 实际退出 0，历史日缓存窗口 1 passed、33 filtered，测试用时 249.46 秒；保留原 15 秒单查询限制。迟到旧 outbox 的真实 worker 投递在 `/tmp/okapi-historical-speech-primary-worker.log` 实际退出 0、4 passed，核对字符单位、原载体、报价版本、四金额和重复 drain。不能以单项通过替代完整统计或生产负载性能验收。

第二固定副本的关联命令 `/tmp/okapi-historical-speech-primary-final-related.log` 实际退出 101：lib 98 passed，analytics 2 passed、15 failed，失败为独立 CH 连接拒绝，后续目标未执行。Docker 状态确认本轮自有 `okapi-character-ch-20260930` 退出 137、OOMKilled=true；仅恢复这一隔离服务继续验证，没有重启共享服务。第二副本严格 Clippy 也实际退出 101（`if_not_else`），已在新源码中调整分支次序，不添加抑制。新的第三固定副本正在重跑，前两副本和失败日志保留。

第三固定副本最终结果：关联命令 `/tmp/okapi-historical-speech-primary-v3-related.log` 实际退出 101，5 个完整块累计 171 passed、2 failed，后续报价/worker 目标没有执行；统计块 32 passed、2 failed、耗时 577.23 秒。四个新增历史字符场景均通过，但历史日缓存与旧 Token 明细缺口的趋势仍因 HTTP 500 失败，CH 捕获到 15 秒超时，不能归为服务断开或视为统计套件通过。第三副本严格 Clippy 实际退出 0，store 38 单测、静态验证分别通过。汇总报告保留实际退出码，不合并不同运行的 passed 数量。

下一轮只改当前/上期的覆盖判定隔离：原来在两期合并范围选择恢复模式，使一个窗口的缺口给另一个完整/空窗口也引入恢复查询。第四固定副本对两期分别做同粒度覆盖和 fresh 字符证据探针，缓存补偿也选择对应窗口判定；单查询 15 秒/2GB 护栏和原计数断言保持不变。尚在验证，不将代码调整当作已通过。

第四副本缓存专项实际退出 0（`/tmp/okapi-historical-speech-primary-v4-targeted.log`，1 passed、33 filtered、252.32 秒），但部分单查询达到 14 秒以上。诊断副本单独关闭谓词下推的对照实际退出 101，CH 仍报 15 秒超时（`/tmp/okapi-historical-speech-optimizer-probe-targeted.log`，0 passed、1 failed）；该诊断改动没有写回工作区，也不算作修复或通过。继续定位查询分析开销，保留所有失败证据。

旧分析器的诊断对照也实际退出 101（`/tmp/okapi-historical-speech-analyzer-probe-targeted.log`，2.39 秒），CH 报 Multiple USING statements are not supported，属于 SQL 能力不兼容；清库后的 UNKNOWN_DATABASE 是后续现象。两种诊断副本均未写回生产代码。第五副本保持原 CH 客户端、分析器与护栏，仅重构分析主来源：时间/归属在主聚合子查询中裁剪，各观察以显式 `m.key = source.key` 连接，主键/基础聚合列显式限定 `m`，保留原择一、人口和残差校验；正在使用原缓存断言验证。

第五副本缓存专项实际退出 0、1 passed、33 filtered、162.36 秒（同一原夹具第四副本为 252.32 秒）。严格 Clippy 实际退出 0、29.88 秒；store 单测实际退出 0、38 passed。首次完整关联命令在并行链接时 ld 被 signal 9 终止，实际退出 101、没有开始测试，报告无通过块；不算业务回归失败，也不算通过。当前以 `cargo test -j 1` 在同一固定副本重跑，原失败日志保留；财务验证排在关联命令之后，尚未结束。

后续链路核对除 PG/CH 日志与 MCP 校准外，还应覆盖现有 chsink 文档明确的幂等边界：CH 已写而 PG 标记失败后，若重试成员变化，min/max 组成的批次 dedup_token 会变化，统计明细可能重复。账本不二次扣费不等于 CH Token/金额统计已经完整幂等；当前四项 worker 回归不能替代这个故障窗口的专门注入测试。新显式字符元数据也应继续核对 TTL/模态等所有 Token 轴的矛盾证据与非消费事件的指标适用人口，本阶段未作全链路正确的结论。

静态验证边界：副本格式、浮点守卫、API 清单通过；错误码守卫首次缺少前端语言只读输入而退出 1，补全到独立验证目录（后端链接同一固定副本、脚本及两语言文件单列 SHA）后退出 0、62 个错误码匹配。没有修改或构建前端。PG/CH 历史日志与 MCP 用量/KPI 的旧载体读取仍待接入和专门验证，未识别历史保持未知覆盖，不能声称全链路校准完成。
