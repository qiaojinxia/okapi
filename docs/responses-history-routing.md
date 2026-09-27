# Responses 历史响应路由

`previous_response_id` 引用的是上游账号里的历史。网关收到这一字段时，需要找回产生该响应的渠道凭证，而不能只提高某个候选的优先级。

## 入口与隔离

- `/v1/responses` 的原生 JSON/SSE 响应会建立 ID 绑定；转换自 Chat Completions 的合成 ID 不代表上游保存了 Responses 历史，不建立绑定。
- `/v1/responses`、`/v1/responses/compact`、`/v1/responses/input_tokens` 共用历史查询。调用方必须使用生成历史时的同一用户及同一 API key。不同 key 即便属于同一用户，也不能直接共享历史 ID。
- 未知、过期、其他用户或其他 key 的 ID 统一返回 `404 not_found`，`param=previous_response_id`，不探测上游。空 ID、非字符串、超过 512 字节、含空白/控制字符，或与 `conversation` 同时提供，返回 400。
- 映射只固定上游账号，不放宽当前模型授权、渠道池、能力、留存策略、渠道/key 状态、冷却、限流或并发限制。续聊读取当前数据库候选，不使用 5 秒进程候选缓存；候选不再可用时拒绝请求。
- 续聊禁止跨渠道/key、跨模型降级，以及 404/405 后改走 Chat Completions。同账号的既有瞬态重试策略仍生效。

## 存储与变更

Redis 键为 `stick:resp:{<user_id>}:v2:<api_key_id>:<sha256(response_id)>`。值包含渠道 ID、渠道凭证 ID 和身份摘要，不保存原始凭证或对话内容。固定有效期为创建后 30 天，读取不延长；这是网关路由记录的有效期，不承诺供应商仍然保留历史。

身份摘要覆盖 provider、入口地址、凭证身份和渠道额外请求头。普通 API key 的凭证变化会使旧映射失效；Codex OAuth 有 account_id 时以该账号身份为准，正常 access/refresh token 更新不改绑，账号变化则拒绝旧历史。刷新期间重读到另一账号时也终止当前请求。没有稳定 account_id 的 OAuth 凭证按完整凭证摘要处理，刷新后可能需要重新提供完整上下文。

映射使用 Lua 原子建立。同一 ID 的重复写入只有身份完全一致才成功，不能被另一渠道覆盖。Redis 读写都有 2 秒超时；读失败或损坏记录返回 503，不回退到任意账号。

## 响应和费用边界

- JSON：返回响应 ID 前写入映射；写入失败时返回错误、按现有失败链释放预扣，不再调用另一账号。
- SSE：首字缓冲期间先保存上游已发出的 response 对象 ID，再将事件交给客户端；通常在 `response.created` 时建立。这允许客户端断开后仍把该 ID 路由回原账号，但不保证上游响应已经完成或可续聊。
- 如果上游直到首字后才给出 ID，此时写入失败会发送 SSE `error` 并结束，不改投；已经生成的内容仍按现有流式结算规则计费。
- Token 计数不扣费；带历史引用时不允许本地估算，也不允许原账号不支持原生计数后切换账号。
- 渠道字段剥除/注入不能删除、替换或凭空注入 `previous_response_id`、`conversation`。Codex 请求整形也保留历史 ID；上游是否接受由实际协议响应决定，不把字段保留当作供应商联调通过。

## 当前边界

此实现针对 `previous_response_id`。`conversation` 对象的创建/归属与生命周期、item/file 引用的账号归属、Responses WebSocket 仍需分别实现和验证。网关不代存完整历史，也不能把启用前、直接调用供应商或已过期的未知响应 ID 安全导入另一账号。

测试记录与完成状态见 [核心 API 验证](core-api-verification.md)。
