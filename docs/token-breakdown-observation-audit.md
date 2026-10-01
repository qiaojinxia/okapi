# Token 细分采集状态核对

2026-09-29。本阶段已修正长期模态汇总所依赖的采集基础，并通过下述专项回归；长期汇总接入仍待完成。旧用量结构的音频、图片、推理计数缺失默认零，不能把这些占位零当作上游明确上报；转换还丢失明确零，或补出不存在的推理零。

`TokenUsage.reported_details` 是可空的采集状态对象，分输入、输出、缓存读取、缓存写入的 audio/image 双轴及 reasoning。缺少外层对象表示旧适配器/旧数据状态未知；对象存在、轴 false 表示未采集。原计费数字、价格、金额和总 Token 不因状态变化而改变。按已校验的完整总数/子项能唯一确定的拆分可标记采集；不得因为默认数值为零或缺失模态计数而猜分配。

文本 JSON/SSE 保留字段是否出现：明确零参与采集，缺失字段不参与。转换 JSON 只输出已提供的细分，流式累计按每个字段替换，缺失沿用前值，明确零替换前值；非法用量持续阻止正常结算。Gemini 模态数组只有该模态出现或完整覆盖总量时才确定该轴，部分数组不能补出别的模态为零。

Realtime 与按图片累加的用量保留每段观察；合并完整性用 AND，旧数据未知不能被后来一段的已知信息覆盖。输入/输出的上游原始总数也需在有效媒体用量中保留。PG 用量、outbox、CH 明细及日志携带同一状态。旧记录保持未知，禁止回填推断。

本阶段不改缓存两种 TTL 的价格。长期独立模态聚合及其他看板接入同一细分观察口径、TTS 字符与 Token 的统计单位分离、独立工具费用及真实供应商账单仍需完成；不能以采集标记的存在证明这些完成。

## 实现与接口口径

- OpenAI Chat/Responses 探针分别保存 audio/image/reasoning 字段是否出现，协议转换保留明确零。累计流式更新即使同时提供输入/输出总量，也不会擦除缺失的旧细分；新缓存总量会使旧 TTL/缓存拆分失效，非法或歧义用量仍拒绝正常结算。
- Gemini 主模态数组允许部分覆盖，但只有显式项或完整覆盖总量时才能确定其他轴为零。没有采集 reasoning 不会补出 thoughts=0。Gemini 缓存数组要求完整组成；跨协议输入的缓存组成不完整时省略该数组，不制造其他模态的测量零，转换不能替代原始完整供应商账单。
- 有效 Realtime、直接图片用量保存上游输入/输出原数。图片输出未单独拆分时沿用该路由的图片单位契约，属于已校验总量的唯一分配，并非新增独立供应商字段。
- 独立用量合并统一到 `TokenUsage::checked_add`，保存全部输入/输出、缓存读写交集、reasoning、TTL 与来源。完整性取 AND，缺失信息不能由另一笔补齐；数字超出 PG/CH 存储上限拒绝合并。
- 图片原生批处理采用同一 Gemini 用量解析器，修复此前丢掉图片输出、缓存交集、TTL、来源等字段的问题。原生批处理按已封存成功图片数及入场单价结算，本阶段的用量字段修正不改变该单价或重复收费。已存在但非法/缺轴的 usage 不能默认变成零；完全未提供 usage 的历史兼容结果保留未知。
- PG `usage_details.tokens.reported_details` 和 outbox 保留对象；CH 九个 `Nullable(UInt8)` 观察列保存三态，旧记录 NULL，新记录 0 未采集、1 已采集。增加列不回填或推断历史观察。

`/api/me/logs` 与 `/admin/logs` 明细返回 `usage.reported_details`。两种日志的 `/stat` 新增 `token_detail_observations`，九个数字字段各含：

| 字段 | 含义 |
| --- | --- |
| `tokens` | 全范围所有结算记录均采集该字段时的合计；覆盖不足或空范围返回 null。 |
| `observed_tokens` | 只加总完整采集该字段的记录；没有样本返回 null，明确零保留 0。 |
| `observed_records` | 完整采集该字段且数字存在的结算记录数。多轮会话/批任务内部只部分采集时，该结算记录仍不计作完整样本。 |
| `coverage_bp` | 观察记录 / 同范围全部记录 × 10,000，整数向下取整；空范围返回 null。 |
| `complete` | 非空范围的所有记录均有该字段的完整采集。 |

原顶层细分数字仍为结算保存值，标记 `token_detail_basis=settled`；原 `*_samples` 兼容字段描述保存了数值的记录，标记 `token_detail_samples_basis=stored_values`，不能用它们宣称供应商采集完整。需要实际采集数和完整范围的读数时使用新的观察对象。汇总沿用原属主、密钥、请求、模型、日期过滤，忽略分页参数。

## 验证记录

完整核心包 `/tmp/okapi-detail-observation-core-2.log` 已观察实际退出 0：**366 passed、0 failed/ignored/filtered/软跳过**。API 9、domain 14、ledger 123、pricing 43、providers 177；其中 domain/pricing/ledger 的 180 项与 pricing 的 5 项对拍包含在总数中，不重复相加。运行前后 1,155 个后端文件指纹完全一致。严格 workspace/all-targets Clippy `/tmp/okapi-detail-observation-clippy-3.log` 已观察退出 0。

首次核心运行 `/tmp/okapi-detail-observation-core-1.log` 实际退出 101，363 passed、2 failed，证据保留：新增累计用例漏写合法的混合模态缓存写入拆分；旧 Gemini 断言预期丢弃明确 thinking=0。补全用例并修改零值保留断言后，整个五包回归重跑通过。Clippy 前两次失败为 DTO 字段同后缀、未使用导入及测试函数过长，已改内部字段名、移除导入、拆分辅助函数；没有放宽 lint 或删减业务/金额断言。

九个完整目标（后端 lib 与八个 HTTP/投递套件）`/tmp/okapi-detail-observation-related-1.log` 已观察退出 0：**277 passed、0 failed/ignored/filtered/软跳过**，包括 lib 89、日志 15、门户 9、图片基础 3、图片契约 82、原生批任务 55、Realtime 9、协议用量 12、CH 投递 3。覆盖不同协议的金额/账本/outbox/统计交叉、零值/未知/部分采集、批量字段、非法用量、重复流事件及重复扣费保护。该运行中 `images/batches/results.rs` 有一次并行改写，不能宣称整个后端源快照全程冻结。

已恢复该文件丢失的两项细分回归断言和单笔存储边界检查；格式化后其指纹与核心二轮开跑时相同，不覆盖其他功能变化。随后另一处并行更新优化了 `analysis_source.rs` 的恢复查询。最终文件下完整后端 lib `/tmp/okapi-detail-observation-lib-final.log` 实际退出 0：**90 passed、0 failed/ignored/filtered/软跳过**，不与前一次 lib 89 重复相加；新增的一项来自并行聚合查询结构验证。当前正在针对最新查询执行完整统计与分析接口回归，最终严格检查与源指纹仍需收口。

第一轮看板回归 `/tmp/okapi-detail-observation-dashboard-final.log` 实际退出 **101**：33 passed、1 failed、0 ignored/filtered/软跳过。个人统计 20 项全部通过，分析 14 项只有 Token 跨接口用例的 TPM 断言失败。该用例先核对多个完整日期窗口接口，耗时超过 60 秒后仍断言最初样本属于分钟窗口；实际 CH 原始记录三笔合计输入 1,100、输出 500，但诊断时最新样本已距当前时间 279 秒。完整范围的 1,600 Token 与 1,636 基点缓存命中率断言均通过，不能据此改动生产分钟窗口。修正用例保留全部范围断言，另写入唯一模型的新样本，精确断言同一条记录的 1,600 Token、1,636 基点及 TPM=1,600。该运行中聚合/缓存代码仍在并行变化，不能作为当前完整快照的通过证据。

并行性能优化接入短查询缓存及粒度覆盖探针后，最新严格检查曾因未使用变量、趋势函数长度及三个网关入口的 Future 大小退出 101（`/tmp/okapi-detail-observation-clippy-final.log` 至 `-final4.log`）。移除废弃引用、提取堆叠维度解析，并在图片/Realtime 异步入口使用 `Box::pin`，没有放宽 lint 或删减统计断言。`/tmp/okapi-detail-observation-clippy-final5.log` 已观察实际退出 **0**，完整 workspace/all-targets `-D warnings` 通过。随后对这一合并版本启动完整后端 lib、分析和统计回归，日志 `/tmp/okapi-detail-observation-dashboard-2.log`；尚未结束时不记为通过。

收尾合并版本的 `/tmp/okapi-detail-observation-dashboard-2.log` 已观察实际退出 **0**：**127 passed、0 failed/ignored/filtered/软跳过**，完整 lib 92、分析 15、统计 20 项，三个完整块，无解析错误或未结束块；报告 `/tmp/okapi-detail-observation-dashboard-2-report.json`。分钟窗口的新样本精确断言通过，原完整日期范围、加权耗时/速度、缓存率、不同拆分维度、历史恢复与保留期、短缓存属主隔离及手动刷新断言均保留并通过。此前 33 passed / 1 failed 的运行仍为失败记录，不以收尾结果改写。

收尾 Clippy 开跑时、HTTP 回归前后及静态检查后，1,157 个后端文件指纹一致；五个核心包相对已通过的 366 项运行没有源码变化，因而不重复计算或声称新的全工作区运行。格式、计费浮点/panic 守卫、错误码守卫、API 清单及 diff 空白检查通过；汇总器与 API 清单的 16 项 Python 回归单独通过。没有改价公式、部署或核销真实供应商账单。本记录的完成范围是细分采集及已列接口，长期模态聚合、字符单位、独立 TTL/工具计价仍按前述边界保留为未完成项。
