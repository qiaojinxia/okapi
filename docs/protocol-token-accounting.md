# Chat / Responses / Gemini 用量核对

2026-09-28，后端统计专项的协议解析阶段。只改用量解释、校验和传递，不修改价格公式；以下金额使用测试配置，不是供应商现行报价。当前工作树还存在其他并行改动。

## 官方口径与实现范围

- [OpenAI Chat 用量](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)：输入、输出、总量，以及输入缓存/音频和输出推理/音频细分。推理是输出子集。
- [OpenAI Responses 用量](https://developers.openai.com/api/reference/resources/responses/methods/create)：input/output 总量、input 缓存细分、output 推理细分。音频/图片及缓存交叉细分在本实现中属于兼容扩展，不能声称官方顶层 Responses usage 均提供它们；嵌套图像工具的独立 usage 也不自动等于主模型用量。
- [Gemini UsageMetadata](https://ai.google.dev/api/generate-content#UsageMetadata)：prompt 包含 cachedContent；本地 completion = candidates + thoughts；total = prompt + candidates + thoughts。promptTokensDetails、cacheTokensDetails、candidatesTokensDetails 分别描述输入、缓存和输出的模态。缓存创建 API、独立工具服务不属于本次生成用量统计。

## 修复

此前公共探针按缓存、缓存写入、音频、图片依次截取剩余额度。因为模态和缓存实际相交，这种做法可能重复计价或挤掉模态用量。现在先确认缓存与音频/图片的交集，扣除交集后再构造互斥计价段，保留缓存模态与图片输出字段。混合模态的正缓存缺少必要交叉细分时，不猜测缓存属于文本还是音频；完整缓存、单一模态等可以唯一确定的情况允许推导。完全没有模态计数的旧用量继续沿用基础价格，交叉组成保留未知。

Chat/Responses 非法数值、超出支持范围的整数、总量矛盾、细分越界、无法确定的交集不再截成合法账单。探针保留内部非法标记，避免反序列化失败后被当成 usage 缺失而估算。JSON 返回上游错误并退款；SSE 已发送内容时在收尾发送错误、退款，不重放上游。

Gemini 读取三个模态数组，检查重复模态、总量/细分冲突和整数溢出；输出音频、图片与 thoughts 都包含在 completion 总量内。TEXT 之外暂未有独立价轴的 VIDEO/DOCUMENT 等仍使用基础价，不宣称实现其独立长期维度。

OpenAI 上游转换为 Responses/Gemini/Anthropic 输出时，缺失 usage 不再制造全零计费探针。缺失或 null 留给现有估算路径；明确的零保留零；非法值阻止正常结算。转换输出也保留可表达的模态细分和显式缓存零值。

## 交叉核对数据

测试输入总量 1,000：文本 200、音频 500、图片 300。缓存 300：文本 50、音频 150、图片 100。因此非缓存文本/音频/图片分别为 150/350/200。输出总量 400：音频 100、图片 200，其余文本 100，其中包含推理 20。总量为 **1,400**，缓存命中率为 **30%**，不会再加缓存或推理。

测试价格为基础输入 4 micro-USD/Token，音频输入倍率 8、图片输入 3、文本输出 4、音频输出相对音频输入 2、图片输出 5，缓存文本/音频/图片倍率为 0.5/2/1。预期费用 **27,900 micro-USD**。JSON/SSE、Chat/Responses/Gemini 上游、Gemini 原生入口，需在 PG 账单、余额、outbox 与 CH 门户统计中保持一致。

- `okapi-providers/tests/usage_modalities.rs`：三协议相等、缓存写入交集、错误字段、缺失状态、原生和转换后的 JSON/SSE。
- `bins/okapi/tests/gateway_usage_modalities.rs`：4 项实际 HTTP 集成测试，包含协议/流式组合，以及缺失/明确零/非法用量的不同结算路径。
- 专项 HTTP 4 项通过（`/tmp/okapi-protocol-usage-http-2.log`，实际退出 0），并在完整运行中再次通过。

## 最终验证

`cargo test --workspace --locked --offline --no-fail-fast -- --test-threads=1 --nocapture` 实际退出 **0**。完整日志 `/tmp/okapi-protocol-usage-full-1.log`、报告 `/tmp/okapi-protocol-usage-full-1-report.json`：**1,142 passed、0 failed/ignored/filtered、0 软跳过**，138 个完整块，无解析错误或未结束块。金额套件包含 domain **13**、pricing **39**（含 parity **5**）、ledger **120**，合计 **172**；这些是完整运行的子集，不另加到总数。另有 434 条权限、公开契约和协议探针，不等同于 434 个完整业务通过。

1,184 个后端源码、配置、SQL、Lua、测试 fixture 和 SQLx 文件在本次完整运行前后指纹一致，记录在 `/tmp/okapi-protocol-usage-manifest-3.json` 与 `/tmp/okapi-protocol-usage-source-check.json`。结束后只调整 `console/mod.rs` 中一个模块声明的排序以通过格式检查，并同步 API 清单源指纹，没有改动执行逻辑。工作区同时包含其他人的功能改动，不把全部变更归为本专项。

第一次适配器专项执行有一个旧 Gemini 用例仍要求省略已上报缓存零值、截断非法推理数；已按新契约更新，最终完整运行通过。第一次 HTTP 专项编译发现测试构造参数位置错误，修正后取得上述专项及完整结果。失败日志保留，不用多次运行拼出完整通过。

最终严格 Clippy 和静态守卫结果见 [核心验证记录](core-api-verification.md)。完整回归证明的是现有用例实际断言的范围，不是所有供应商费用和所有 API 已核销。

## 尚未覆盖

后续已补齐 Anthropic 原生及转换的严格解析，并修复 JSON 二次转换丢失内部非法标记的问题，见 [Anthropic 用量核对](anthropic-token-accounting.md)。供应商真实账单联调、独立音频转写与图像工具费用、Anthropic 两种缓存写入 TTL 的分别持久化与定价、估算来源的全链路标记、CH 模态长期分析列仍需补齐。上游不提供细分时无法补造历史数据。没有把本地模拟上游通过当作各供应商能力或费用已经全部核销。
