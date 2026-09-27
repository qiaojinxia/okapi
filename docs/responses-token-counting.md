# Responses 输入 Token 计数

`POST /v1/responses/input_tokens` 是生成前的独立预检。它不调用生成接口、不预扣或结算余额，不新增消费账单、计费事件或计费 outbox；零余额账户仍可在权限和限流允许时使用。

## 请求与计数模式

使用正常的数据面 API key。网关需要非空 `model` 进行授权和选路；支持模型别名及渠道模型映射。`input`、`instructions`、`tools`、`text`、`reasoning`、`previous_response_id`、`conversation` 等原生字段交给计数上游处理。`conversation` 与 `previous_response_id` 不能同时设置。计数不支持流式；`stream: false` 会被移除。

```sh
curl "$OKAPI_BASE_URL/v1/responses/input_tokens" \
  -H "Authorization: Bearer $OKAPI_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-model","input":"请统计这段输入的 Token 数。"}'
```

`OKAPI_BASE_URL` 是网关根地址，不包含 `/v1`。可选请求头 `x-okapi-token-count-mode`：

| 值 | 行为 |
| --- | --- |
| `native`（默认） | 只请求上游 `/responses/input_tokens`；不支持时返回错误，不自动估算。 |
| `auto` | 优先原生计数。没有支持计数的原生候选，或尝试的候选都返回 404/405/501 时，可对完整文本上下文估算。权限、限流、超时、5xx、畸形响应不触发本地估算。`provider.allow_fallbacks=false` 时首个上游错误直接返回。 |
| `estimate` | 使用本地分词估算，不请求计数上游。仍检查模型权限、可见渠道、留存偏好以及请求准入。 |

原生候选要求 `responses_native=true` 且未配置 `capabilities.input_tokens=false`。普通 OpenAI、显式开启原生 Responses 的兼容渠道以及 Codex OAuth 使用各自凭证和请求头；上游计数能力须由实际供应商支持。最多尝试三个实际上游请求，渠道 RPM/并发不足的候选不会被发送请求。

## 响应与精度

上游计数的典型成功响应：

```json
{"object":"response.input_tokens","input_tokens":42}
```

本地估算响应：

```json
{"object":"response.input_tokens","input_tokens":42,"estimated":true}
```

- `x-okapi-token-count-source: upstream` 表示结果来自当前上游；`local_estimate` 表示网关本地估算。来源为 `upstream` 不保证中转上游使用了供应商精确 tokenizer。
- 上游 JSON 的 `estimated: true` 或响应头 `x-okapi-token-count-source: local_estimate` 会被保留为响应中的 `estimated: true`，不会经二次转发变成未标记的计数。
- 成功响应设置 `cache-control: no-store`；成功和错误均包含请求 ID。
- 计数必须是非负整数且不超过 `u32::MAX`；合法零值可返回，缺字段、负数、小数、字符串、溢出或畸形对象会失败，不伪造零值。

本地估算支持文本消息、function call / output、instructions 与工具/输出格式配置。使用本地 BPE 和协议开销近似；超长片段会按采样比例外推，非 OpenAI 模型也不是其官方分词器。因此即使文本看似简单，也始终标注估算。

本地估算拒绝图片、文件、加密推理、未知输入项，以及 `previous_response_id`、`conversation`、保存的 `prompt` 引用：网关没有这些对象的完整内容。原生模式保留这些字段，由上游判断是否可计数。`previous_response_id` 已接入 [历史账号绑定](responses-history-routing.md)：须为同一用户、同一 API key 经网关获得的原生响应 ID，固定原渠道凭证，不切换账号或估算；未知/过期返回 404，不可用返回 503。`conversation` 的归属和生命周期尚未实现；对该字段的既有测试只证明原样转发。

## 权限、限流与失败

检查 API key 状态、模型白名单、IP 白名单、可见渠道池和 `provider.zdr` / `data_collection: deny`。共享现有分组和用户×模型 RPM；原生请求还共享渠道 key 的 RPM 与并发槽。

计数自身使用独立的 per-key RPM/RPD/并发租约，不消耗生成请求的余额预扣或 Token 额度。RPM 取 key 的正数配置，否则每分钟 60；RPD 取正数配置，否则不限制；并发取正数配置，否则为 4。准入 Lua 无法执行时返回 503。

| 状态 | 含义 |
| --- | --- |
| 400 | 本地输入校验失败、无法本地估算，或上游拒绝参数。`error.param` 标出字段或上游状态。 |
| 401 / 403 | 无效凭证、模型/IP 等权限拒绝。 |
| 404 | 网关没有指定模型。 |
| 429 | key、分组、模型、渠道限流/并发，或上游返回 429。 |
| 501 | 没有原生计数能力，或上游返回 404/405/501；`error.param=input_tokens_unsupported`。 |
| 502 | 上游认证失败、重定向、服务错误或不可用的计数响应。上游私密错误原文不会回显。 |
| 503 | 无可见候选、留存条件不满足、计数准入不可用等。 |
| 504 | 上游或整个计数请求超时。 |

单次上游超时取渠道首响应超时与 20 秒的较小值；整个 handler 最长 60 秒。禁止跟随上游重定向，响应体最多 32 KiB。成功、失败、超时和已取得租约后的 handler 取消都会归还占用；进程崩溃或 Redis 清理失败依靠租约/信号量 TTL 兜底，不能把正常取消测试当成全故障释放证明。

验证入口：`bins/okapi/tests/gateway_token_count.rs`。运行记录及尚未完成的对标项目见 [核心验证记录](core-api-verification.md)。
