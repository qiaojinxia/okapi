# Anthropic Token 统计核对

2026-09-28。接续 Chat/Responses/Gemini 解析阶段，核对原生 Messages，以及 Chat、Responses、Gemini 转换。验证对象是当前工作树；其中有其他并行功能改动。本专项不修改价格公式。

## 口径与修复

[官方流式规则](https://platform.claude.com/docs/en/build-with-claude/streaming)将 `message_delta.usage` 定义为累计值。[Messages 字段参考](https://platform.claude.com/docs/en/api/messages)包含后续输入与缓存计数、缓存创建的 TTL 细分，以及 `output_tokens_details.thinking_tokens`。本地统一输入为普通输入 + 缓存读取 + 缓存写入；输出已经包含推理，不能再次相加。

此前原生和转换流各自解析用量，错误类型、负数或越界值落为零，输入/缓存只取起始事件。现在共享 `anthropic_usage`；完全缺失或 null 的 usage 保持未知。JSON 在本阶段要求完整输入和输出，后续 [来源阶段](token-usage-provenance.md) 已支持保留有效单轴、仅估算缺失轴；明确零仍保留，非法字段不能转入估算。流式按字段更新累计值，保留未更新的字段，重复相同 delta 不相加，后续输入与缓存修正生效。起始输出只是阶段值，没有收到后续输出计数时交给估算路径，不将起始计数当成最终输出。

非法类型、负数、小数、计数越界、缓存 TTL 细分总量矛盾、推理超出输出、累计输出回退、用量事件损坏或重复 message_start，都会留下持续到结算的非法标记。后续合法事件不能清掉该标记。原生与转换路径共用网关错误结算：退款并返回上游错误，不以异常计数生成正常账单，也不因此重放上游。

只按明确上报的 `thinking_tokens` 记录推理用量，不通过可见思考文本长度反推。缓存缺失与明确上报零的采集状态继续保留。

## 验证设计

合成最终用量：普通输入 100、缓存读取 800、缓存写入 100、输出 50（其中推理 20）。统一输入 **1,000**、总量 **1,050**、缓存命中率 **80%**。测试价格是输入 2、缓存读 1、缓存写 4、输出 4 micro-USD/Token，费用为 **1,600 micro-USD**；这不是供应商现行报价。

流式从较小起始值出发，再依次给出部分累计值、最终值、重复最终值和空更新。原生 Messages、Chat、Responses、Gemini 四入口，JSON/SSE 组合，需在 PG 账单、余额、outbox、CH 门户统计保持相同结果。

- `crates/okapi-providers/tests/anthropic_usage.rs`：7 项，覆盖协议规则、字段完整性、边界值、累计更新、重复/回退、缺失/零、非法状态持续。
- `bins/okapi/tests/support/anthropic_usage.rs`：3 项 HTTP 测试，随 `gateway_usage_modalities` 执行，核对有效用量、错误退款及缺失与零的不同路径。
- `cargo test -p okapi-api -p okapi-providers -p okapi-domain -p okapi-pricing -p okapi-ledger --locked --offline --no-fail-fast -- --test-threads=1 --nocapture` 实际退出 0：**336 passed、0 failed/ignored/filtered、0 软跳过**，28 个完整块，无解析错误或未结束块。日志 `/tmp/okapi-anthropic-usage-core-1.log`。金额 domain/pricing/ledger 的 172 项（含 parity 5 项）已包含在其中。
- workspace/all-targets 严格 Clippy 实际退出 0（`/tmp/okapi-anthropic-usage-clippy-5.log`）。首次几轮检查暴露测试 Bytes 参数与样式错误，修正后通过；未将编译失败记为测试失败或通过。
- 首次 11 个关联网关套件实际退出 101：**97 passed、3 failed**，日志 `/tmp/okapi-anthropic-usage-http-1.log`。一个实际失败暴露 Anthropic→Chat→Responses JSON 二次转换丢弃内部非法标记、重新读取 null usage 后错误进入估算；修正为保留已有探针，Gemini 与 Anthropic 回向转换同样处理，并增加 Gemini HTTP 覆盖。另一个失败来自测试直接比较门户不公开的上游成本字段，现改查 PG 成本与 outbox 的 known 标记，门户白名单保持不变。
- 第三个失败发生在跨周期测试的 `rolled.window_start <= Utc::now()`。滚窗实现的契约按传入时间计算，测试原先在等待、调用和断言时分别读取墙上时钟，原日志没有记录偏差，不能证明具体时钟跳变原因。现捕获越过到期点的同一个时间并按该入参验证覆盖，保留后续新周期额度实际可用、退款不串窗的断言，并补入失败诊断时间。本阶段未修改生产滚窗算法。
- 修正后 13 个关联网关套件实际退出 **0**：**109 passed、0 failed/ignored/filtered、0 软跳过**，13 个完整块，无解析错误或未结束块，日志 `/tmp/okapi-anthropic-usage-http-2.log`。覆盖上述四入口与 Bedrock、Vertex、原生/转换 Messages、Gemini、Responses HTTP/WS、流中断、非法用量及 33 项预扣原子性/跨窗测试。11 个本专项实现和测试文件在运行前后不变；期间另有基础价格和目录功能并行更新，不能声称整个工作树冻结。
- 在新的基础价格实现下重跑 core 套件实际退出 **0**：**336 passed**（其中金额 172 项），28 个完整块，0 failed/ignored/filtered/软跳过，无解析错误或未结束块，日志 `/tmp/okapi-anthropic-usage-core-2.log`；1,188 个后端相关文件在运行前后指纹一致。随后只做相关文件格式整理，并出现并行新增测试。
- 最新价格规则、分层价格与跨协议用量三个 HTTP 套件实际退出 **0**：**23 passed**，无失败或跳过，日志 `/tmp/okapi-anthropic-usage-pricing-integration-1.log`。此时的生产价格代码已包含并行增加的基础价格配置，运行期间既有文件不变。之后新增基础价格单元套件另测 **4 passed**（`/tmp/okapi-anthropic-usage-base-price-1.log`），不加到先前 336 项中。
- 上述检查点和上一阶段 1,142 项完整工作区通过分别记录。本阶段未再做全工作区测试，不拼接局部套件与重叠用例形成新的“完整通过”数字。测试使用模拟上游与隔离的真实 PG、Redis、NATS、CH 服务，没有进行真实供应商账单核销。

## 收尾时工作区状态

严格 workspace/all-targets Clippy 在 `/tmp/okapi-anthropic-usage-clippy-11.log` 实际退出 **0**。此前新增基础价格测试触发长度和命名告警，已提取重复请求与夹具并保留全部断言；另对基础价格单元测试做格式和显式类型整理。这些属于并行功能的测试维护，不代表本专项实现了基础价格功能。

该检查通过后，工作区开始并行增加密钥限额功能。额外 `pricing_base` HTTP 运行在编译阶段退出 **101**（`/tmp/okapi-anthropic-usage-base-http-1.log`）：`console/admin.rs` 构造 `ApiKeyPatch` 时缺少新字段 `quota_micro`，**没有执行测试，不能记为通过或业务断言失败**。最终源码核对新增 `okapi-ledger/src/key_budget.rs`，并发现 gateway、ledger、权限等文件继续变化，包括本专项修改过的 `gateway/chat.rs`；记录为 `/tmp/okapi-anthropic-usage-final-source-check.json`。因此上述通过结果只对各运行时的源码有效，不覆盖正在写入的新限额实现。

最后一次格式检查退出 **1**（`/tmp/okapi-anthropic-usage-final-fmt.log`），新密钥限额相关代码尚未格式化；错误码守卫发现 `key_limits_session_required` 缺 zh-CN/en 文案。金额浮点守卫及 API 清单在新增限额后续变动前曾通过，不能据此声称最终工作区全绿。未为追赶这批并行变更修改前端、重跑无关功能或覆盖他人实现，待其稳定后需要重新验证。

后续平均首字阶段已通过 `pricing_base` 实际 HTTP 测试，并完成新的全目标严格 Clippy、格式与错误码检查；上述失败属于当时检查点。平均 TTFT 修正及实际证据见 [平均首字核对](ttft-average-accounting.md)。

## 2026-09-29 展示与缓存 TTL 补充

已增加可空的 `cache_write_5m_tokens`、`cache_write_1h_tokens`，从 Anthropic 原生／转换探针传至归一化用量、PG 快照、outbox 和 CH 原始日志。两项必须同时存在且合计等于缓存写入总量；后续只更新输出保留拆分，更新写入总量但没有拆分则清除旧拆分。历史缺失保持 null，明确零保持零。

门户与管理日志详情和可展开汇总展示 TTL、多模态缓存交集、图片输出及用量来源；汇总为当前筛选全范围，并标明已采集条数，不随分页变化。CSV 同步增加字段。费用展示使用请求当时保存的有效模态单价，文本输出扣除音频及图片输出，缓存文本扣除缓存音频及图片，避免明细重复计算。缺单价不读取当前价格补造费用。

本补充不修改扣费公式：TTL 只是写入总量的统计拆分，仍使用原缓存写入价格。纯生成阶段速度与包含首字等待的总览吞吐量分别说明。CH 新字段仅在原始日志保留期内可按原字段统计，尚未建立 TTL／模态专用长期聚合。

验证包括域/API/Anthropic 累计用量单元测试、PG 日志全范围汇总测试、CH 新旧日志混合查询测试及浏览器布局/账单回归。首次 PG 测试因本地 Docker 数据盘无空间失败，空间短暂恢复后的复跑通过；未删除业务数据，也不能因此视为磁盘问题已根治。没有执行真实供应商账单核销。

最终浏览器日志与语义回归 **51 passed**，全目标编译、格式和中英键检查通过。严格 Clippy 被已有 `duration_suboptimal_units` 告警阻止；仅在命令中允许该告警后的 lib/bins 检查通过，不称为无例外的严格全绿。四入口 JSON/SSE 完整复验未通过：先遇数据库连接失败，随后出现异步投递尚未可见的断言，最终满盘阶段请求返回 500。新版网关与控制台健康检查为 200，但 worker 启动被 CH 的明确 `errno: 28 / No space left on device` 阻止；需解决 Colima 数据盘空间后再复验，不将此前重点接口通过替代完整端到端验收。

## 仍未完成

- 缓存写入已有一个可配置价格；两种 TTL 计数现已独立持久化，但尚未按两种 TTL 分别定价，不能声称 1 小时缓存费用已与供应商对齐。
- 服务端工具、advisor/fallback 多模型子调用的独立费用仍需核对。本次只读取顶层 Token 用量，不能由此证明所有供应商收费项已覆盖。
- 文本请求已在后续 [来源阶段](token-usage-provenance.md) 补上按轴的估算来源、已知计数与估算计数混合保存，贯穿结算明细、回执、outbox、CH 单笔日志。聚合来源覆盖、图片/音频/Realtime 等专用路径仍需补齐，估算准确度仍不能由模拟用量测试证明。
- 上游不提供推理或缓存细分时不能补造历史值。平均首字时间口径已在后续阶段修正；平均总耗时、配对输出速度及有效分位数也已完成专项修正，见 [总耗时与速度核对](latency-statistics-audit.md)。真实供应商账单联调与 CH 模态长期分析列仍待完成。
