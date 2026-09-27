# Responses WebSocket 接入与验收

## 当前状态

已注册 `GET /v1/responses` upgrade，与原有 POST 并存；原生 WS 与 HTTP/SSE 桥接共享逐轮准入、选路和结算链。
当前支持 `response.create`，渠道类型为 `openai`（含此类型的兼容上游）与 `codex`，要求
`responses_native=true`，所选传输的能力开关不能为 false。
已实现 native/http/auto 协议选择及 HTTP/SSE 桥接；尚未实现模型级降级及中途干预事件。

协议参考：[OpenAI WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode)。
本地模拟上游测试与真实供应商联调分开记录，不能据此宣称已完整对齐 Sub2API / New API。

## 网关契约

- 升级前验证 API key；每一轮再次执行统一的身份、模型白名单、历史归属、分组/成员/模型限额、余额及预扣检查。
  使用既有鉴权缓存失效机制。网关错误帧携带独立 `okapi_request_id`，有效命名流还携带 `stream_id`；
  上游事件保留原有字段并添加 `okapi_request_id`，不覆盖供应商的 request ID。
- 每 key 默认最多 4 个连接，可用 `responses_ws_max_conns_per_key` 配置；使用独立的 Redis 租约，
  存储故障时拒绝升级。租约 60 秒、每 20 秒续租并复查身份；连接关闭后释放，活动结算仍持有它直到排空完成。
- 每轮从数据库获取当前候选，检查可见池、渠道/key 状态、能力、保留策略、负毛利、渠道 RPM/消费/并发限额。
  首次选定传输后固定上游账号；后续轮次不能改投。凭证、账号、地址、额外头或代理变更后不能继续复用旧连接。
  握手失败可在发送 create 前按策略尝试其他候选；发送 create 或 HTTP POST 后绝不自动重放或换账号。
- 同一 stream 在结算完成后再开始下一轮；不同 stream 最多 16 轮并行。待处理帧尚未鉴权预扣，关闭后直接丢弃。
  队列、消息与输出缓冲有数量/字节上限；慢客户端不能无限占用内存，也不能阻塞账单消费者。
- 原生 WS 的 `generate:false` 仍走准入和报价，预扣不估算生成 Token。按上游明确提供的 usage 结算；
  缺 usage 的预热返回 `usage_missing` 并退款，不凭空估算生成用量，也不把预热一概视为免费。
  渠道 strip/inject 不能改变 generate、stream、background、stream_id、store 或历史关联字段。
- 无产出且无可计费用量的请求级失败退款。已生成或带有效 usage 的失败/不完整终态仍按实际用量结算，
  账单同时记录错误状态，避免将其统计为正常完成；没有 usage 但已输出时沿用既有估算政策。
- 单轮硬时限缺省 480 秒，可用 `responses_ws_turn_timeout_secs` 缩短，最大仍为 480 秒；
  持续收到事件不会延长硬时限。预扣在 10 分钟后可能被对账退款，因此为排空和结算留出余量；
  一小时是连接寿命，不是单轮生成额度。首事件或单轮超时后通知客户端，并继续排空最多 30 秒；客户端断开后同样最多排空 30 秒，以便取得终态 usage。
  到期关闭原生上游连接或停止排空该轮 HTTP 流，不重放；不能保证取得截止时间之后产生的用量。每轮任务进入停机结算等待集合。
- 原生历史 ID 在暴露前按用户与 API key 绑定账号，复用 HTTP 的 Redis 元数据；只保存 ID 摘要和账号摘要，
  不保存对话内容。30 天映射 TTL 不是供应商历史留存保证，`store:false` 的实际历史仍由原连接缓存决定。
  不自动展开历史，也不将失效 ID 改成无历史的新请求。
- Codex OAuth 握手复用凭证刷新、账号头和客户端身份头；请求保留不透明输入，执行既有 Codex 规范化，
  固定 `store:false` 并移除 HTTP 专用的 `stream` 字段。真实账号的缓存/过期行为尚未专项联调。

## HTTP/SSE 桥接

- 全局 `settings.responses_ws_transport` 可选 `native`、`http`、`auto`，缺省 `auto`；渠道
  `channels.settings.responses_ws_transport` 非 null 时优先。每轮读取渠道设置；配置变更若不允许已固定的传输，
  本轮明确拒绝，须建立新连接，不在缺失历史快照时迁移。非字符串或未知值拒绝。
- `native` 只用原生 WS，`http` 直接发 SSE POST；`auto` 优先 WS，在渠道明确关闭
  `capabilities.responses_websocket`，或首次握手返回 404/405/426/501 时可改用同账号 HTTP。
  `capabilities.responses_http=false` 禁止 HTTP。鉴权失败、限流及其余 5xx 不触发协议切换；
  create 前的账号候选回退仍遵守原有路由偏好。协议选择与账号回退分开处理。
- HTTP 请求只发送一次，`stream=true`，移除 WS 专用 type/stream_id/generate；不跟随重定向，要求 SSE。
  请求级 HTTP 错误尽量保留供应商 JSON 错误体及状态。SSE 事件添加入口 stream_id，拒绝错误的流归属、
  不一致的 response ID、非法 JSON、矛盾的 output item；不把已发出的请求自动重试。
- 每个连接的每个 stream 保存最新完整快照（输入 + 原样 output items）；工具调用、工具结果、加密 reasoning
  与其他不透明字段保留。只在显式 previous_response_id 指向已知快照时展开；新一轮 instructions 以本轮为准。
  终态缺 output 时可由 output_item.done 重建，终态与已收集 item 矛盾则拒绝，不能静默丢失上下文。
- 未知的真实供应商历史 ID 仍须通过用户/API key 的元数据归属校验，然后原样交给同账号上游；不删除未知 ID。
  如快照始于一个外部已存储父响应，后续展开仍保留该外部父 ID。元数据 TTL 不保证供应商还能恢复内容。
  正在生成的响应不能用作桥接父快照。同 stream 的失败淘汰所引用的缓存父快照；其他 stream 的失败不删源快照。
- 连接内快照序列化字节预算 `responses_ws_context_bytes` 缺省/最大 128 MiB，可缩小；每个完整上下文最多
  4096 个 item、64 MiB，展开后的请求也最多 64 MiB。活动 HTTP 请求序列化字节另有 128 MiB 预算；
  每轮 SSE 原始字节总上限 128 MiB，在解析器缓冲之前检查；错误体最多 64 KiB。
  这些是序列化字节限额，实际 JSON 对象和短期拷贝还占内存。超限明确拒绝，不裁剪历史。
- HTTP 模式 `generate:false` 仅在网关准备本地快照，不 POST 上游；事件显式带 `okapi_warmup:"local"`，
  响应 ID 使用 `resp_okapi_warmup_` 前缀，仅当前 WS 连接可继续。不是供应商缓存预热，也不保证延迟改善。
  仍经过普通身份、限额、报价、余额及预扣准入，但完成后按零金额结算，按次计费模型也不收调用费。
  原生 WS 的预热继续使用上游真实 usage，两者不能混淆。
- 首事件超时可以发生在 HTTP 响应头到达前；同一 POST 会继续被排空，以收取最终 usage。
  超时、下游断线及缓存错误后的终态用量仍按共享结算政策入账，保留失败状态；不复发 POST。
  本地快照仅存在于连接内存，断开后不恢复；不写入 Redis，不进行对话压缩或摘要。

## 已实现的原生传输契约

- 每个实例只属于一个下游会话和一个上游账号；克隆共享该连接，不在不同用户间共用。
- 保留原始 JSON 帧、工具结果、加密 reasoning、`previous_response_id`、`generate` 和 `store`。
  本层不展开历史，不根据 stream 名自动补续聊 ID，不重放可能已经执行的请求。
- 同 stream 串行，跨 stream 最多 16 轮活动请求；最多 32 个命名 stream，另有默认 stream。
  本地最多容纳 64 轮待处理/活动请求，待发送消息总量及缓冲事件总量分别限制为 128 MiB，单消息上限 64 MiB。
- 按 stream 分发事件并检查响应 ID；终态保留 usage。异常关闭通过 `UpstreamError::Session` 返回，
  不伪造 `Done`，也不给网关提供自动重试的信号。
- 丢弃单轮消费者后继续排空其上游事件，等待终态才释放该 stream；这不等于取消模型执行。
  `close()` 或最后一个连接持有者被释放时关闭整个连接。
- 握手、写入、消费者背压、空闲和连接寿命分别有限制；默认连接寿命最多一小时。
  显式关闭和寿命到期可以中断受阻的发送/交付。

## TLS 与代理

普通转发、管理探针和 WS 使用独立 client 家族，共享一个轻量的 `HttpPool` 引用。
WS client 设置 `http1_only()` 且禁止重定向，直连和代理缓存均采用相同策略。
仅设置请求的 HTTP/1.1 version 不足以限制 TLS ALPN：真实 TLS 对照测试验证了默认 client 仍会协商 h2。

WS 继续执行证书链与域名校验。测试 CA 只加入单个测试 client，不安装到系统，也不关闭证书校验。
`http_tls_tests.rs` 用真实 TLS ClientHello 验证生产构造器及代理缓存未命中路径只提供 HTTP/1.1；
独立用例验证 WSS 帧与 usage、认证 HTTP CONNECT 代理、不受信任证书和域名不匹配。
普通转发与探针仍能协商 h2。HTTPS 代理本身的 TLS、SOCKS 和真实供应商尚未做本轮专项联调。

握手仅接受有效 upgrade、accept key；未协商的扩展/子协议被拒绝。
受保护的握手头不能被渠道额外头覆盖；账号凭证覆盖普通额外头。
非 101 状态保留状态码及 Retry-After，错误体最多读取 64 KiB。

## 验证与剩余边界

`bins/okapi/tests/gateway_responses_ws.rs` 使用真实 gateway upgrade、客户端/上游 WebSocket 帧及隔离 PG/Redis，
验证逐轮账单金额、余额、预扣释放、权限变更、账号固定、FIFO/并行、超时不重放、断开后终态结算等。
`gateway_responses_ws_bridge.rs` 的 15 项测试覆盖真实 WS → HTTP/SSE 转发、策略选择、快照、OAuth、按次定价预热、权限、失败隔离与迟到 usage。
原有 `responses_ws_transport.rs` 的 16 项传输测试与 5 项 TLS 测试继续保留；这些不能代替网关验收。
执行次数和实际退出码见 [核心 API 验证记录](core-api-verification.md)。

仍需补齐或专项验证：模型级降级；中途干预；真实供应商的缓存驱逐/跨连接恢复与存储；
会话/item/file 生命周期；长达一小时的真实连接运行；租约跨进程故障注入与关闭时的长期用量核对。
本地 fixture 证明协议与账本行为，不证明供应商兼容性或相对竞品的性能优势。
