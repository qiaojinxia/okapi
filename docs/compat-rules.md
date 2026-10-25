# 适配规则（compat）：上游与终端差异的数据化设计

> 状态：**设计稿，尚未实现、尚未立项**（2026-10-24）。第一阶段只预留可扩展性：本文定约定与接缝，不要求立刻写引擎。
> 已定的方向见 §0，仍待拍板的见 §15。代码引用均为 2026-10-24 的 `main`；行号随代码漂移，所以只写文件与函数名。

## 0. 摘要与已定的方向

**做什么。** 把"不同上游厂商 / 模型 / 下游终端之间的形状差异"从散落的 Rust 特例，变成**带版本、可回放、可审计的数据**，由一个无 IO 的纯引擎在流水线的固定挂载点执行。

**不做什么。**
- 不在进程内嵌 Lua / JS：与 `docs/provider-architecture.md`「渠道配置驱动已有实现的选择，不执行管理员提供的任意脚本」冲突；脚本会和所有上游凭证、账本连接跑在同一进程里。
- 不碰金额：计费规则（`pricing_rules`、价簿快照与 epoch）保持独立；本引擎只产出"用量事实"。
- 不取代协议转换器：方言互转（`crates/okapi-providers/src/convert/`）仍是 Rust，只把它们**读取的表**数据化。

**已定的方向（2026-10-24 讨论）。** 标 ✓ 的是明确拍过板的，其余是讨论中形成、尚未逐条复核。
1. 规则 = 带类型的闭合规则代数（JSON Pointer 路径 + 选择器），条件与取值用 **CEL**；不嵌脚本。
2. 绑定三层叠加、按候选渠道解析：**厂商预设 → 渠道覆盖 → 全局护栏**。可复用的"适配档案"（命名规则包，等于管理员自定义厂商）**放 P2 ✓**。
3. 存储用**独立版本表 ✓**，设计成通用的 `rule_sets`（带 `kind`），路由策略将来复用。
4. 蓝图只是规则 JSON 的视图，不是第二份配置；MCP 让 AI 只起草草稿，人批准发布。
5. 模型路由是**另一个阶段**（定价与预扣之前），复用本引擎的版本表、CEL、选择器、回放，见 §13。

## 1. 为什么需要：现状的四个缺口

| 缺口 | 代码 | 后果 |
|---|---|---|
| 模型契约是编译期精确 ID 表 | `crates/okapi-providers/src/model_parameters.rs::profile()`（`gpt-6-*`、`gpt-5.x`、`claude-*`、`kimi-k3`…逐个写死） | 新模型的 effort、预算、温度范围要改代码、发版（服务器构建约 14 分钟） |
| 渠道级只有顶层、无条件的剥与注 | `bins/okapi/src/gateway/chat.rs::adjust_request_fields`（先剥后注） | 不分模型、改不了嵌套路径、不能改名与限幅；一个渠道里多个模型怪癖不同，只能拆渠道 |
| 响应、流、错误侧没有声明式入口 | 思考字段只认 `reasoning_content`（字符估算处明确忽略别名 `reasoning`）；用量字段变体、错误映射都是代码；`try_model` 重试循环里写死 `dropped_reasoning`、Responses 404/405 降级 | 每个新变体都要改 Rust |
| 厂商身份是代码 | 余额探针按写死的主机名选厂商（DeepSeek / SiliconFlow / OpenRouter / Moonshot 各一个解析函数，`bins/okapi/src/console/channel_balance.rs`）；`openai_compat` 的 `default_base` 是 `https://api.openai.com/v1`（`crates/okapi-providers/src/registry.rs`） | 空地址的第三方渠道会把第三方 key 发给 OpenAI；`openai` 适配器的认证头是写死的 `Authorization: Bearer`（Azure 另用 `api-key`），换认证方式要改代码 |

另有一处小债：保护键清单有两份（`console/admin.rs::inject_request_fields_ok` 与 `chat.rs` 的 `STRIP_PROTECTED` / `INJECT_PROTECTED`），靠注释约定"同一份"。

## 2. 竞品对照

对照的两份源码：new-api `d04c118`（2026-09-23）、sub2api `8f2cbb5`（2026-10-19）。

| | new-api | sub2api | 本设计 |
|---|---|---|---|
| 厂商适配 | 40 个 Go 包，各自实现 14 个方法的 `Adaptor` 接口，大半是样板（`relay/channel/moonshot/adaptor.go` 135 行，真正的怪癖只有 URL 布局和一个模型的温度） | `domain/platforms.go` 单一平台清单 + `service/provider_profile.go`（"协议→端点"表，`by_inbound` / `by_model` 两种选路），但行为怪癖是 `if account.Platform == PlatformDeepseek` 散在至少 6 个文件，`openai_chat_roles.go` 把厂商域名写死在代码里 | 厂商 = 数据；翻译器留 Rust；怪癖 = 规则 |
| 参数覆盖 | `relay/common/override.go`：23 种操作、条件 AND/OR、敏感路径审计；字符串 `mode`、gjson 路径、无静态检查、只挂渠道 | 账号级头覆盖 + 禁止覆盖清单；`model/error_passthrough_rule.go`（库里的错误规则：状态码 / 关键词 → 透传或自定义） | 带类型的闭合原语、JSON Pointer、分层、版本、回放 |
| 扩展 | JS 插件（sobek，5 秒超时）只用于异步任务（10 个插件、6.3k 行 JS），治理很重：schema、d.ts、夹具、版本、dry-run、代际切换 | 签名的外部二进制插件，只用于 OAuth 传输 | 不嵌脚本：CEL → Rust 适配器 → 外挂转换服务 |
| 配置形态 | `relaykit/dto/channel_settings.go` 里 20 多个字段（多为布尔开关）的杂物袋；`advanced_custom` 路由表与 40 个适配包是两套并行机制 | 平台特例散在代码 | 同一份规则表达；默认拒绝的白名单规则取代一堆开关 |

值得借鉴的两点：new-api 插件"只做转换、副作用归宿主、只报用量事实不报金额"；sub2api 的"协议→端点"表和按模型选协议。

## 3. 设计原则

1. **数据，不是脚本。** 规则是 JSON 文档，不是程序。
2. **原语优先于特例。** 遇到表达不了的怪癖，加一个**通用**原语（操作、CEL 函数、变量），不写厂商特例；原语集合只增不减。"不改通用代码"是目标，不是承诺（见 §14 的边界样本）。
3. **纯函数、幂等、可回放。** 无网络、无时间、无随机；应用两次等于应用一次；同一输入同一输出。
4. **事实，不是金额。** 响应规则只能把字段映射到用量槽位（整数运算），价格由价簿算。
5. **先预过滤，再求值。** 选择器可索引；没有规则的渠道零开销（沿用价簿 `has_volume_rules()` 那种门控）。
6. **失败开放。** 规则运行失败（CEL 求值出错、路径缺失）→ 跳过并记录，不让合法请求失败；标记 `required` 的护栏与 `reject` 失败关闭。
7. **来源与依据。** 内置规则必须带 `doc.why / url / verified_at`；管理员规则带作者与备注。
8. **单一真源。** 规则 JSON 是唯一真源；蓝图、表单、API、AI 都只是它的客户端。

## 4. 流水线与挂载点

下面是 `gateway/chat.rs` 里 chat 请求的真实流程，以及规则在哪几处接入。

| 阶段 | 现有位置 | 规则挂载 | 说明 |
|---|---|---|---|
| S0 入口解析 | `chat_completions` / `messages` / `responses` / `gemini_generate` → `ProbeInfo` | — | 提供 `features`、`stream`、`client` |
| S1 解析模型 | `resolve_with_directive` → `(ResolvedModel, ReasoningDirective, 计价变体)` | 路由（预留，§13） | 返回形状就是路由器该返回的形状 |
| S2 定价与预扣 | `prepare_chat`：`CalcContext → quote → reserve_for_key`（含 `fallback_models` 冻结） | 不挂 | 计费域 |
| S3 选候选 | `eligible_candidates` / `try_model` 逐候选（渠道 × key，`upstream_model` 为映射后） | `capability` | 能力事实合并进 `ExecutionPlan::compile_target` 的 `caps: &Value` 参数，不改 `ExecutionPlan` |
| S4 构造上游体 | `build_upstream_body`：`ExecutionPlan::compile` → 方言转换 → `model_parameters` 契约 → reasoning 注入 | `request_pre`（预留） | `bound_default_output` 已有先例：有些默认值必须在转换前写入口方言 |
| S5 请求整形 | `dispatch_chat` 内 `shape_upstream_body`：剥 okapi 指令 → 渠道 strip / inject → 后置校验 | **`request`** | 现有 strip / inject 就在这里 |
| S6 传输 | `outbound_with_client` + 各 `*_via` 适配器 | `wire` | 头、端点、认证声明。客户端伪装（`extensions`、`profiles`）由适配器自己做，在规则之后，规则不得改它拥有的字段 |
| S7 响应 | 适配器返回 `ChatResponse` 之后、`wrap_*` 方言回转之前 | **`response`** | 仿 `wrap_thinking_to_content` 的包装做法 |
| S8 失败 | `try_model` 的重试循环 | **`error`** | 现有写死的反应式规则迁入 |

### 4.1 阶段（phase）枚举

`request_pre`、`request`、`wire`、`response`、`error`、`capability`。**schema 现在就写全，P1 只实现 `request`**，这样以后加阶段不用改规则格式。

### 4.2 执行语义

- **请求规则作用于"发给这个上游的那份 JSON"**：在 `shape_upstream_body` 的位置，对已转成上游方言的体执行。Claude Code 的 Anthropic 请求去 OpenAI 兼容厂商，会先转成 OpenAI chat，再套该厂商的规则。
- **每次尝试都重新应用**：`dispatch_chat` 每次尝试都对已构造的体整形；failover 到下一个候选时从原始请求重新构造。所以规则必须幂等，且不留状态。
- **后置校验沿用**：现有 `previous_response_id` 不得被改的校验就是这个模式；规则应用后重跑受保护路径断言，违反则回滚该规则本次改动并记录。
- **响应规则对流式事件逐帧执行**：只有存在匹配规则才解析；无规则零开销。流式事件是原文 `raw` 加元数据（用量、`has_output`），规则改了字段后要重算元数据，否则会漏收。
- **错误规则只在"首字前"生效**：流已开始输出后不能重试。
- **规则不改计费行为**：`include_usage` 等计费关键字段见 §6.4。

## 5. 绑定与解析

### 5.1 三层叠加，按候选渠道解析

| 层 | 绑定到 | 谁写 | 存放 | 典型内容 | 能否被下层改 |
|---|---|---|---|---|---|
| 厂商预设 | `channels.vendor`（可空） | 我们，随二进制发布，带夹具 | 内嵌 JSON | 严格 role、推理模型去采样参数、用量字段映射、错误映射 | 渠道可按 id 禁用或改写 |
| 适配档案（P2） | `channels.vendor = custom:<id>` | 管理员 | 版本表 | 自定义长尾厂商；一批渠道共用的规则 | 渠道覆盖 |
| 渠道覆盖 | 渠道本身 | 管理员，或 AI 起草后人批 | 版本表 | 本渠道特例：转售商的怪癖、隐私剥字段、注入头 | 不能改护栏 |
| 全局护栏 | 整个网关 | 管理员（高权限） | 版本表 | 合规：永不转发某些字段，最后执行 | 谁也不能改 |

### 5.2 不绑定的对象

| 对象 | 为什么不绑 |
|---|---|
| 渠道池 | 池里可能混着多个厂商，怪癖是每个候选自己的；规则按候选解析 |
| 分组 / 用户 / API key | 产品策略，不是上游适配；需要时作为 CEL 的只读变量（P3 再开放） |
| 价簿里的模型 | 那是对客户端的名字、定价和权限，同一个客户端模型可经不同渠道走到不同的上游模型；规则里用**映射后的上游模型**写选择器 |
| 客户端伪装（`extensions.client_profile`） | 客户端身份由适配器拥有，规则只能读 |

### 5.3 选择器

全部字段 AND；缺省 = 不限；模型与主机支持 `*` 通配。选择器是**结构化**的，所以能建索引、能画成表单，也能在规划阶段先预过滤。

| 字段 | 含义 |
|---|---|
| `vendor` | 厂商 id 列表 |
| `host` | 渠道 `api_base` 的主机名（覆盖"同一厂商经通用兼容渠道接入"的情形） |
| `model` | **映射后的上游模型** |
| `client_model` | 客户端请求的模型名（少用） |
| `dialect` | 上游方言：`openai.chat` / `openai.responses` / `anthropic.messages` / `gemini.generate` |
| `operation` | `chat` / `responses` / `embeddings` / `images` / `rerank` / `audio`… |
| `stream` | 布尔 |
| `client` | 入口客户端类别（`claude-code`、`codex`、`cursor`…，来自 UA 与头） |

### 5.4 合并与顺序

- 有效规则 = 厂商 ⊕ 档案 ⊕ 渠道 ⊕ 护栏。规则有 `id`；**更靠后的层里同 id 的规则覆盖前面的**；`disabled: true` 把前面的同 id 规则关掉。
- 执行顺序：阶段 → 层级（厂商 100、档案 200、渠道 300、护栏 900）→ `order` → 文档顺序。护栏最后执行，并且不能被下层覆盖。
- 编译：按（渠道 id，快照版本）编译成 `CompiledRules { by_phase }`，带模型通配索引，缓存；发布时由 NATS 通知各副本失效。

### 5.5 厂商自动识别只做建议

`channels.vendor` 为空时，按 `api_base` 主机在厂商表里查，**只用于**：① UI 提示"检测到 api.deepseek.com，应用 DeepSeek 预设？"；② 规则的 `select.host` 匹配。**不静默改变现有渠道的行为。** 教训来自 sub2api 的 `targetsDeepSeekAPIHost`：同一厂商经常通过通用兼容渠道接入。

## 6. 规则文档格式（schema v1）

### 6.1 结构

```jsonc
{
  "schema_version": 1,
  "kind": "compat",                       // compat | route（预留）
  "rules": [
    {
      "id": "strict-role.developer-to-system",   // 必填；同一规则集内唯一；跨层同 id = 覆盖
      "phase": "request",
      "select": { "host": ["api.deepseek.com"], "dialect": ["openai.chat"] },
      "if": "request.messages.exists(m, m.role == 'developer')",   // CEL，可选；P1 保留字段，校验器返回 cel_not_enabled
      "do": [ { "op": "role_map", "from": "developer", "to": "system" } ],
      "order": 100,                       // 可选，层内微调
      "disabled": false,                  // 在下层关掉上层同 id 的规则
      "required": false,                  // true = 失败关闭（护栏与 reject 用）
      "billing_critical_ack": false,      // 允许移除计费关键字段，见 §6.4
      "doc": { "why": "…", "url": "https://…", "verified_at": "2026-10-24" }
    }
  ]
}
```

### 6.2 三个示例

请求阶段（new-api 里 `kimi-k2.6` 的温度固定，sub2api 里的严格 role 厂商）：

```jsonc
{ "id": "moonshot.k2-6-temperature-one", "phase": "request",
  "select": { "vendor": ["moonshot"], "model": ["kimi-k2.6"] },
  "do": [ { "op": "set", "path": "/temperature", "value": 1.0 } ],
  "doc": { "why": "该模型只接受 temperature=1（new-api relay/channel/moonshot/adaptor.go isTemperatureOneOnlyModel）", "verified_at": "2026-10-24" } }

{ "id": "strict-role.developer-to-system", "phase": "request",
  "select": { "host": ["api.deepseek.com", "api.kimi.com", "api.moonshot.cn", "api.moonshot.ai", "open.bigmodel.cn", "api.z.ai"],
              "dialect": ["openai.chat"] },
  "if": "request.messages.exists(m, m.role == 'developer')",
  "do": [ { "op": "role_map", "from": "developer", "to": "system" } ],
  "doc": { "why": "这些厂商拒收 developer 角色（sub2api backend/internal/service/openai_chat_roles.go requiresSystemChatRole，主机清单写死在代码里）", "verified_at": "2026-10-24" } }
```

响应阶段（用量映射，整数运算、饱和、非负）：

```jsonc
{ "id": "xai.completion-from-total", "phase": "response",
  "select": { "vendor": ["xai"], "dialect": ["openai.chat"] },
  "do": [ { "op": "usage_map", "slots": { "completion": "usage.total_tokens - usage.prompt_tokens" } } ],
  "doc": { "why": "xAI 的 completion_tokens 不含推理 token（new-api relay/channel/xai/text.go）", "verified_at": "2026-10-24" } }
```

错误阶段（把现有写死在重试循环里的 Responses 404/405 降级数据化）：

```jsonc
{ "id": "responses.no-native-endpoint", "phase": "error",
  "select": { "dialect": ["openai.responses"] },
  "when": { "status": [404, 405] },
  "do": [ { "action": "retry_same", "plan": { "responses_native": false }, "max": 1 } ],
  "doc": { "why": "上游没有 /responses：同一候选改走 chat 降级链，不计 failover、不标 key 失败（chat.rs try_model；仍受渠道 tool_loss_policy 约束）", "verified_at": "2026-10-24" } }
```

### 6.3 原语清单

路径用 RFC 6901 JSON Pointer，`*` 表示数组的任一元素或对象的任一成员。

**请求 / 响应的 JSON 操作**

| 操作 | 阶段 | 作用 | 来源怪癖 |
|---|---|---|---|
| `remove {paths}` | request, response | 删除路径 | #18 |
| `keep_only {paths}` | request | 白名单：只保留列出的路径 | #8 |
| `set {path, value 或 expr, mode}` | request, response | 写值；`mode` = `always`（缺省）或 `if_absent` | #7 #12 #19 |
| `rename {from, to}` / `copy {from, to}` | request, response | 移动或复制，支持通配 | #10 #13 #14 |
| `clamp {path, min, max}` | request | 数值夹取；整数路径保持整数 | #15 |
| `map_value {path, table}` | request, response | 查表替换 | |
| `role_map {from, to}` | request | `messages[*].role` 映射（`map_value` 的语法糖） | #11 |
| `each {path, do}` | request, response | 对通配元素逐个执行子操作，变量 `item` | #9 #10 |
| `coerce {path, to}` | request | 形状转换：`string→message_list`、`string→array` | #19 |
| `inject_message {position, role, content}` | request | 在 `messages` 头或尾插入一条（敏感路径） | #30 |
| `reject {status, code, message}` | request | 拒绝请求，不发往上游（合规） | |
| `apply {builtin, params}` | request, response | 调用 Rust 内置的命名变换，作逃生口 | #25 #B5 |

**线路（wire）**：`set_header {name, value, mode}`、`remove_header {name}`，受保护头见 §6.4。端点、认证放在厂商记录里（§8），不是规则。

**仅响应**

| 操作 | 作用 |
|---|---|
| `alias {from, to}` | 目标缺失才写 |
| `usage_map {slots}` | 槽位：`prompt / completion / cache_read / cache_write / reasoning / audio_prompt / image_prompt`；每个槽位是整数表达式（取字段、加减、`max(0, …)`），饱和、非负 |
| `finish_reason_map {table}` | 查表 |

**错误阶段**：条件 `when {status, code, body_contains, match}`（`match` = `any` / `all`），动作如下。

| 动作 | 作用 |
|---|---|
| `retry_same {mutate, plan, max}` | 在同一候选上改请求或改计划后重发，计次，不计 failover、不标 key 失败 |
| `fail {status, code, message, passthrough}` | 改写返回给客户端的错误 |
| `failover` | 显式换候选 |
| `cooldown {scope, secs}` | 与现有健康状态机对接 |
| `skip_monitoring` | 不计运维监控 |

**能力（capability）**：`capability {tools, vision, json_schema, stream_usage, parallel_tool_calls, verbosity, …}`，按选择器给出事实，合并进 `caps`；**渠道里显式的 `false` 永远优先**（保留现有"显式 false 才排除"的约定）。

### 6.4 受保护、计费关键与敏感路径

- **永不可写**：`/model`（只能经模型映射改）、`/stream`、`/previous_response_id`、`/conversation`、okapi 自己的路由指令（`provider` 等）、客户端伪装拥有的字段（系统前缀、缓存断点、`metadata.user_id`…）、认证头与连接头（`authorization`、`x-api-key`、`x-goog-api-key`、`host`、`content-length`、`content-type`、`accept-encoding`、逐跳头、`sec-websocket-*`）。这份清单只能有一份注册表，控制台写入校验与网关运行期共用，取代现在的两份。
- **计费关键**：流式时的 `/stream_options/include_usage`。移除它会让结算落到字符估算（`chat.rs` 注释实测"漏收约七成"），所以规则要移除它，必须带 `billing_critical_ack: true`，UI 给出警告并写审计。
- **敏感路径**（写入需要更高权限并写审计，仿 new-api 的敏感路径清单）：`/messages`、`/input`、`/system`、`/instructions`、`/contents`、`/reasoning*`、`/service_tier`。

### 6.5 限额

每个规则集 ≤ 256 条规则；每条规则 ≤ 32 个操作；路径深度 ≤ 16；`each` 嵌套 ≤ 2；CEL 表达式 ≤ 1 KiB、语法树深度 ≤ 32；规则文档 ≤ 256 KiB。

## 7. CEL profile v1

CEL 只用于 `if` 条件和取值。**P1 不启用**：`if` 字段在 schema 里预留，校验器对它返回 `cel_not_enabled`，存量规则以后不用迁移。

### 7.1 变量

| 变量 | 类型 | 可用阶段 | 说明 |
|---|---|---|---|
| `request` | 动态 JSON | request, error | 上游方言的请求体（只读视图，写入用操作） |
| `response` / `usage` | 动态 JSON | response | 非流式整体或流式单帧；`usage` 是 `response.usage` 的简写 |
| `error` | `{status, code, message, body, retry_after}` | error | |
| `vendor` `host` `model` `client_model` `dialect` `operation` | 字符串 | 全部 | `model` 是映射后的上游模型 |
| `stream` | 布尔 | 全部 | |
| `client` | `{family, ua}` | 全部 | |
| `channel` | `{id, provider}` | 全部 | |
| `features` | `{has_tools, has_images, has_audio, message_count, prompt_tokens_estimate}` | 全部 | 来自 S0 的 `ProbeInfo` |
| `directive` | `{effort, budget}`，可空 | request, capability | S1 归一后的 reasoning 意图（`@effort`、`-high`、请求参数合并后） |
| `capability` | `map<string, bool>` | request, response | 能力事实 |
| 预留 `user` `conversation` `classifier` | | | 路由与分组条件用；分类器概率用千分整数，避免浮点 |

### 7.2 函数

标准 CEL：`size`、`has`、`exists`、`all`、`map`、`filter`、`contains`、`startsWith`、`endsWith`、`matches`（RE2，线性时间）、`int`、`string`。

宿主函数（每增加一个都是一次小改动，之后所有厂商可用）：`glob(s, pattern)`、`hash_alnum(s, n)`（确定性：同输入同输出，用来做跨字段一致的 ID 重写，不需要状态）、`coalesce(a, b)`、整数 `max` / `min`、`lower` / `upper` / `trim`。

不提供：时间、随机、网络、浮点运算（`usage_map` 的表达式必须通过"结果为整数"的类型检查）。

### 7.3 检查与成本限制

Rust 的 `cel` crate（0.15.0）文档里没写静态类型检查器与资源上限，所以写入时要自己做：① 每个阶段固定变量表与函数表，未声明的名字直接拒绝；② 用夹具上下文**试跑**一遍，求值出错或结果类型不对就拒绝；③ 表达式长度与语法树深度上限；④ 输入体积上限与求值超时。运行期求值出错 → 跳过该规则并记录（失败开放）。

### 7.4 版本

固定一份带版本号的"Okapi CEL profile v1"（变量表加函数集合加语义），随规则集记录。升级只增加变量与函数，不改已有语义；破坏性变更走 v2，并提供迁移校验器。

## 8. 厂商层（Vendor）

厂商是编译进二进制的静态数据（做法同内置的 LiteLLM 目录：内嵌 JSON、带夹具、`verified_at`）。

```jsonc
{
  "id": "deepseek", "display_name": "DeepSeek", "group": "cn",       // cn | western | self_hosted
  "hosts": ["api.deepseek.com"],                                      // 自动识别建议与 select.host
  "endpoints": [                                                      // "协议→基址"表；先匹配先用
    { "protocol": "openai.chat",         "base": "https://api.deepseek.com" },
    { "protocol": "openai.responses",    "base": "https://api.deepseek.com", "path": "/responses" },
    { "protocol": "anthropic.messages",  "base": "https://api.deepseek.com/anthropic" }
  ],
  "auth": { "style": "bearer" },                                      // bearer | header{name} | query{name}
  "default_headers": {},                                              // 建议头，前端预填，不静默合并
  "links": { "console": "…", "docs": "…" },
  "probes": { "balance": null, "models": { "path": "/models", "ids": "/data/*/id" } },   // 声明式 ProbeSpec（P2）
  "capabilities": { "tools": true },
  "rules": [ /* 适配规则，格式同 §6 */ ]
}
```

- **端点是表，不是单个 `default_base`**：同一厂商常有多个协议端点（Moonshot 的 Anthropic 端点在 `/anthropic/v1/messages`，DeepSeek 的 Responses 在 `/responses`，Volcengine 的 `bot*` 模型走 `/api/v3/bots/chat/completions`）。端点可带 `when`（选择器），先匹配先用。
- **`channels.provider` 在 P1 仍是渠道的唯一线路协议**（单协议渠道，调度与计费保持简单）。"按入口方言选同协议端点以避免转换损失"（sub2api `by_inbound`）留到 P3。
- **命名空间**：`builtin:<id>` / `custom:<id>`（P2 档案），`channels.vendor` 存带命名空间的 id。
- **`openai_compat` 的默认地址**：建议改为"没有缺省，必须填 `api_base` 或选厂商"，修掉空地址把 key 发给 OpenAI 的隐患（这是对现有行为的改动，要单独评估）。

## 9. 存储、版本与发布

通用版本表（迁移序号待定，当前最新是 `0005`）：

```sql
CREATE TABLE rule_sets (
    id             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind           varchar(16)  NOT NULL,        -- 'compat' | 'route'（预留）
    scope_kind     varchar(16)  NOT NULL,        -- 'channel' | 'global' | 'profile'（P2）
    scope_id       bigint,                       -- 渠道 id / 档案 id；global 为 NULL
    version        integer      NOT NULL,        -- 同 (kind, scope_kind, scope_id) 内自增
    status         varchar(16)  NOT NULL,        -- 'draft' | 'published' | 'archived'
    schema_version integer      NOT NULL DEFAULT 1,
    rules          jsonb        NOT NULL,
    note           varchar(255),
    created_by     bigint,
    created_at     timestamptz  NOT NULL DEFAULT now(),
    published_by   bigint,
    published_at   timestamptz,
    UNIQUE (kind, scope_kind, scope_id, version)
);
-- 每个 (kind, scope_kind, scope_id) 至多一条已发布
CREATE UNIQUE INDEX uq_rule_sets_published
    ON rule_sets (kind, scope_kind, COALESCE(scope_id, 0)) WHERE status = 'published';
```

- **发布**：一个事务里把原已发布版本置 `archived`、新版本置 `published`，然后经 NATS 发 `reload`，消息带 `kind / scope_kind / scope_id / version`；各副本重新编译并缓存。沿用价簿换期（`PriceBook 已装载 epoch=…`）的做法。
- **回滚**：把旧版本重新发布即可。
- **草稿**：AI 起草与管理员编辑都先落草稿，不影响线上。
- **诊断**：每个请求的诊断里记 `compat: { versions: { vendor: "deepseek@3", channel: 7, guard: 2 }, fired: [规则 id…], skipped: [{id, reason}] }`，沿用现有 `bill.trace.set(…)`。这样"回放真实请求"几个月后仍能复现。
- **权限**：渠道覆盖草稿用 `channel.write`；发布用新增的 `compat.publish`；全局护栏用 `compat.guard.write`；触及敏感路径用 `compat.sensitive`。AI（MCP）只能写草稿。
- **审计**：草稿创建、发布、回滚都写审计；敏感路径与计费关键的改动单独标记。

## 10. 与现有机制的融合

融合原则三分：**数据化的进引擎；带逻辑的翻译器留 Rust，但把表数据化；其他领域只通过明确的接口交互。**

| 现有机制 | 去向 | 说明 |
|---|---|---|
| 渠道 `strip_request_fields` / `inject_request_fields`（`adjust_request_fields`） | 渠道覆盖层的 `remove` / `set` 规则 | 等价替换，用夹具回归；保护键清单收成唯一一份 |
| `model_parameters`（`profile` / `completion_cap` / `apply_effort`） | 表数据化，翻译器留 Rust | effort 如何落到各方言字段是翻译逻辑，留着；efforts、预算范围、温度上限这些表进厂商 / 模型契约数据，管理员能补新模型 |
| 修饰符 `@effort`、`-high` 与 reasoning 归一（`modifiers`、`reasoning`） | 不动，留在 S1 | 将来路由返回同样的三元组 |
| `tool_loss_policy`、`thinking_to_content`、`responses_native`、`image_stream_usage` | 变成"有效设置"（厂商默认 ⊕ 渠道覆盖），用同一套分层 | 它们改的是转换计划，不只是 JSON |
| 重试循环里写死的反应式规则（`dropped_reasoning`、Responses 404/405 降级） | 迁到错误阶段的数据规则 | OAuth 刷新、同 key 重试、failover 留 Rust（牵涉凭证与健康状态机） |
| 余额探针按主机 | 厂商里的声明式 `ProbeSpec` | P2 之后 |
| 计费规则 `pricing_rules`（价簿快照 + epoch） | **不并入** | 金额永远走整数引擎。只对齐三点：选择器词汇（模型、分组）、发布快照语义、"没规则零开销"门控。将来若用 CEL 做计费规则的条件，也只产出布尔或整数倍率 |
| 客户端伪装（`client_profile`、`profiles/`）、账号控制（`account_control`）、`routing_prefs`、凭证与健康状态机 | 不并入 | 适配器与凭证域；规则只读，不写它们拥有的路径 |

**命名**：计费规则 / 适配 / 路由，三个名字分开。现有的"规则"页（`features/rules`）是计费规则，新功能叫"适配"，避免混淆。

## 11. 预留的接缝（现在几乎零成本，不写引擎）

| 将来功能 | 现在预留什么 |
|---|---|
| CEL | schema 的 `if` 字段、阶段变量表（§7.1）；P1 校验器对 `if` 返回 `cel_not_enabled` |
| 响应、错误、能力规则 | `phase` 枚举现在写全（§4.1），P1 只实现 `request` |
| 蓝图、MCP、AI | 引擎做成**无 IO 的纯 crate**（输入 JSON 加上下文，输出改动与命中轨迹）；线上、dry-run、回放、夹具用同一份代码；管理端只读 API：**有效规则（含来源）**、**dry-run**、**catalog** |
| 模型路由 | 把 `resolve_with_directive` 的返回抽成 `ModelResolver` 接口（别名 + 修饰符是第一个实现） |
| 档案层（P2） | `vendor` 命名空间、"协议→基址"端点表 |
| 版本、发布、回放 | 通用 `rule_sets`；诊断记 `compat_version` 与命中规则 id；NATS 消息带 `kind` |
| 计费关键保护 | 受保护路径唯一注册表里就带"计费关键"标记 |

## 12. 蓝图与 MCP

### 12.1 蓝图是视图

- 节点与操作一一对应：选择器、条件（CEL）、依次执行、各种操作；一条执行线加几条数据线，不做循环、变量、自定义函数。
- 数据不变：存的是规则 JSON，蓝图只负责渲染与编辑；表单、API、AI 写的规则都能自动排版成图。**位置信息另存，不进规则。**
- P1 只读自动排版加"回放真实请求"（每个节点标出命中与改动，右侧实时显示编译出的 JSON 与 CEL）；P2 拖拽编辑（`@xyflow/react`，MIT，只在这个页面懒加载）。
- **列表 / 表单视图必须保留**：节点图做不好键盘无障碍与手机端。
- 节点库由后端 `catalog` 生成，Rust 新增一个操作，前端自动多一个节点。

### 12.2 MCP 工具（草案）

| 工具 | 作用 |
|---|---|
| `compat_catalog` | 规则 schema、原语、变量表、配方、示例 |
| `compat_effective(channel, model, dialect)` | 有效规则与来源 |
| `compat_validate(draft)` | schema、CEL 检查与 lint |
| `compat_dry_run(draft, sample 或 request_id)` | 对样本或一条真实请求回放，给出前后差异 |
| `compat_explain(request_id)` | 这个请求命中了哪些规则、改了什么 |
| `compat_propose(scope, rules, rationale)` | AI 提交草稿；发布要人批 |

核心闭环：**上游 400 → AI 读诊断 → 提议规则 → 对那条失败请求回放通过 → 人批准发布。**

## 13. 模型路由（预留，不在本期）

- **位置**：S1 解析模型之后、S2 定价之前。路由必须在预扣之前，因为价簿按模型定价。输出形状复用 `resolve_with_directive` 的三元组：`{canonical, directive, 计价变体}`，另加 `reason`。
- **虚拟模型**：客户端请求 `auto` 之类的名字，路由策略（`kind='route'` 的规则集，同样有版本与发布）把它解析成具体模型。
- **决策由便宜到贵三档**：① 确定性规则（CEL）：显式指定了具体模型就直接用，带图选视觉模型，超长上下文选长上下文模型，有工具选支持工具的，`#fast` 之类标记；② 本地启发式；③ 分类器或小模型。Jev（TypeSafe 的分类决策模型）一类的分类器通过 Okapi 渠道调用，复用凭证、出口代理和防内网检查。
- **护栏**（取自 Jev 路由实践）：分类器答案只当建议；确定性前置规则先于 AI；护栏只能升档不能降档；会话内只升不降（提示缓存绑定模型）；空闲约 10 分钟或压缩后才重判；每档置信度门槛；超时 fail-open 到默认档。
- **候选先过滤**：按 key 的允许模型、分组可见性、已发布价格、渠道是否健康，路由不越权。
- **计费**：按选中的具体模型计价，不加路由附加费（OpenRouter Auto 同此）；路由器自己的调用成本记为平台成本；诊断记 `requested_model`、`resolved_model`、原因、置信度、路由版本。响应体的 `model` 写真实模型，另加响应头标出路由结果。
- **隐私**：分类器会看到 prompt，可限定只用自己的渠道、截断长度、只记决策不记内容。
- **边界**：做"事前选一次"，不做"先用便宜模型、不行再升"的级联（要看输出，成本翻倍）。
- **与现有机制的分工**：路由选模型；池与调度选渠道；`models.fallback_models` 与重试处理失败。

## 14. 怪癖语料库（前 30 条）

用途：**验收这套原语是否够用**。每条都要有"规则表达 + 夹具"，新增厂商只提交数据与夹具；表达不了的记为缺口，决定加通用原语还是写适配器。`Y` = 用本文已有原语即可；`Y+` = 需要新增一个通用原语 / 函数 / 变量（已在 §6–§7 列出）；`B` = 计费关键，有保护规则；`N` = 内置命名变换。来源路径的仓库：new-api、sub2api 为对照仓库，其余是 Okapi 自己。

### 14.1 端点、认证、头

| # | 来源 | 怪癖 | 阶段 | 表达 | 评估 |
|---|---|---|---|---|---|
| 1 | new-api `relay/channel/moonshot/adaptor.go` | Claude 格式走 `/anthropic/v1/messages`，chat 走 `/v1/chat/completions`，rerank 走 `/v1/rerank`；另有"特殊基址"表 | 厂商 | `endpoints` 表 | Y |
| 2 | new-api `relay/channel/volcengine/adaptor.go` | `bot*` 模型走 `/api/v3/bots/chat/completions`，其余走 `/api/v3/chat/completions` | 厂商 | `endpoints[].when.model` | Y+（端点带选择器） |
| 3 | new-api `relay/channel/cloudflare/adaptor.go` | URL 含账号 id：`/client/v4/accounts/{account}/ai/v1/chat/completions`，默认 `/ai/run/{model}`；账号 id 借用了 `api_version` 字段 | 厂商 | 端点路径模板变量 `{account}`、`{model}` | Y+（模板变量） |
| 4 | sub2api `service/provider_profile.go` | DeepSeek 的 Responses 端点在 `/responses`（没有 `/v1` 前缀） | 厂商 | `endpoints[].path` | Y |
| 5 | new-api `relay/channel/openai/adaptor.go` | OpenRouter 缺省补 `HTTP-Referer`、`X-OpenRouter-Title` 头 | 厂商 | `default_headers` | Y |
| 6 | new-api `relay/channel/ali/adaptor.go` | 流式请求加 `X-DashScope-SSE: enable` 头 | wire | `set_header`，`if stream` | Y |

### 14.2 请求字段

| # | 来源 | 怪癖 | 阶段 | 表达 | 评估 |
|---|---|---|---|---|---|
| 7 | new-api `moonshot/adaptor.go` | `kimi-k2.6` 只接受 `temperature=1` | request | `set /temperature 1.0`，选择器 `model` | Y |
| 8 | new-api `perplexity/relay-perplexity.go`、`mistral/text.go` | 输出只含固定字段，其余一律丢弃 | request | `keep_only` | Y+（白名单原语） |
| 9 | new-api `mistral/text.go` | tool_call id 必须恰好 9 位字母数字，`tool_calls[].id` 与 `tool_call_id` 要映射一致 | request | `each` + `hash_alnum(id, 9)`（确定性，无状态） | Y+（宿主函数） |
| 10 | new-api `mistral/text.go` | `image_url` 对象改成字符串；assistant 带 tool_calls 且 content 为空时清掉 content | request | `each` + `rename` + 条件 `remove` | Y+（通配逐元素） |
| 11 | sub2api `service/openai_chat_roles.go` | DeepSeek / Kimi / 智谱拒收 `developer` 角色，要改 `system`（按平台或主机名判断） | request | `role_map`，选择器 `host` | Y |
| 12 | new-api `siliconflow/adaptor.go` | FIM 请求（有 prefix / suffix）要求 `messages` 非空，缺则补一条空 user 消息 | request | `set /messages`，`if` 条件（CEL） | Y（依赖 CEL） |
| 13 | new-api `siliconflow/adaptor.go` | 图片请求字段映射：`size→image_size`、`n→batch_size` | request | `rename`，选择器 `operation=images` | Y+（`operation` 变量） |
| 14 | new-api `xai/adaptor.go` | `grok-3-mini*` 要把 `max_tokens` 改成 `max_completion_tokens` | request | `rename`，选择器 `model` | Y |
| 15 | sub2api `pkg/apicompat/types.go`、`service/apicompat_config.go` | Responses 的 `max_output_tokens` 下限 128（配置不得小于 16），太小上游会报错 | request | `clamp {min: 128}` | Y |
| 16 | sub2api `service/openai_gateway_forward.go` | Responses 请求的 `max_output_tokens`：OpenAI / DeepSeek 保留（被上游明确拒收时才删，有界重试）；Anthropic 改成 `max_tokens`；其余（含 Gemini）直接删；OpenAI 平台反向把 `max_tokens` 归一成 `max_output_tokens`（兼容 Responses 的上游会拒 `max_tokens`） | request + error | 按厂商 `rename` / `remove`，加错误阶段 `retry_same{mutate: remove}` | Y+（`retry_same`） |
| 17 | sub2api `SupportsVerbosity(upstreamModel)` | 模型不支持 `text.verbosity` 就删掉这个字段 | request | `remove /text/verbosity`，条件"能力里没有 verbosity" | Y+（`capability` 变量） |
| 18 | Okapi `crates/okapi-providers/src/oauth/codex.rs` `UNSUPPORTED_FIELDS` | Codex 后端不接受 14 个官方 Responses 字段（`stream_options`、`max_output_tokens`、`temperature`、`top_p`…） | request | `remove` 路径列表 | Y |
| 19 | Okapi `oauth/codex.rs` `prepare_body` | `store=false`、`stream=true`、`instructions` 缺省补空串、字符串 `input` 包成消息列表、`input[].role=system` 改 `developer` | request | `set` + `set{if_absent}` + `coerce` + `role_map` | Y+（`coerce`） |
| 20 | Okapi `model_parameters.rs`（`completion_cap`、`sampling_requires_none`、Kimi K3 固定采样） | 推理模型 `max_tokens` 改名 `max_completion_tokens`；GPT 推理模型仅 effort=none 时可设采样参数；Kimi K3 的温度与 top_p 不可设 | request | `rename` / `remove`，`if directive.effort != 'none'`；契约表进数据，翻译器留 Rust | Y+（`directive` 变量） |

### 14.3 流式、用量、响应

| # | 来源 | 怪癖 | 阶段 | 表达 | 评估 |
|---|---|---|---|---|---|
| 21 | Okapi `crates/okapi-providers/src/openai.rs::ensure_stream_usage` 与 new-api `openai/adaptor.go` | Okapi 对流式请求强制 `stream_options.include_usage=true`（否则结算落字符估算，实测漏收约七成）；new-api 对非 OpenAI / Azure 渠道清掉 `stream_options`。两者冲突 | request | 允许 `remove /stream_options`，但必须带 `billing_critical_ack` 并告警 | B |
| 22 | new-api `relay/channel/xai/text.go` | xAI 的 `completion_tokens` 不含推理 token，要用 `total_tokens − prompt_tokens` 算 | response | `usage_map` 整数减法 | Y+（整数加减） |
| 23 | sub2api `service/gemini_messages_compat_service.go` | Gemini 的 `promptTokenCount` 含缓存命中数，要减去，且下限为 0；输出 = candidates + thoughts | response | `usage_map`：`max(prompt − cached, 0)`，`cand + thoughts` | Y+（饱和减法）。Okapi 的 Gemini 用量转换是 Rust 实现，这里只演示原语 |
| 24 | Okapi `bins/okapi/src/gateway/chat.rs`（测试 `visible_reasoning_is_counted_once_and_opaque_context_is_not`） | 思考内容的字段名各家不一，Okapi 现在只认 `reasoning_content`，字符估算处明确忽略别名 `reasoning` | response | `alias /…/reasoning → /…/reasoning_content` | Y |
| 25 | Okapi `crates/okapi-providers/src/convert/thinking.rs` | `reasoning_content` 在流里转成带 `<think>` 标签的正文，要跨帧记住"已开 / 已关" | response | `apply: thinking_to_content`（Rust 内置命名变换） | N |

### 14.4 错误与重试

| # | 来源 | 怪癖 | 阶段 | 表达 | 评估 |
|---|---|---|---|---|---|
| 26 | Okapi `chat.rs::try_model` 与 `okapi_providers::responses::rejects_foreign_encrypted_content` | 会话换了账号，上个账号签发的加密推理项验不过：去掉推理项在同一候选上重发一次，不计 failover | error | `retry_same{mutate: apply without_reasoning_items, max: 1}` | Y+（`retry_same` + 内置变换） |
| 27 | Okapi `chat.rs::try_model` | Responses 直转撞上 404 / 405（上游没有 `/responses`）：同一候选改走 chat 降级链，仍受 `tool_loss_policy` 约束 | error | `retry_same{plan: {responses_native: false}}` | Y+（`plan` 动作） |
| 28 | sub2api `backend/internal/model/error_passthrough_rule.go` | 按状态码 / 关键词（any / all）+ 平台范围 + 优先级，透传或自定义状态码与错误信息，可跳过运维记录 | error | `when` + `fail{…}` + `skip_monitoring` | Y |

### 14.5 策略与护栏

| # | 来源 | 怪癖 | 阶段 | 表达 | 评估 |
|---|---|---|---|---|---|
| 29 | new-api `relaykit/dto/channel_settings.go` `ChannelOtherSettings` | `AllowServiceTier`、`AllowInferenceGeo`、`AllowSpeed`、`AllowSafetyIdentifier`、`DisableStore`、`AllowIncludeObfuscation` 六个布尔开关，默认过滤敏感字段 | 护栏 | 一条默认拒绝的 `remove` 规则 + 放行白名单，取代六个开关 | Y |
| 30 | new-api `ChannelSettings.SystemPrompt` / `SystemPromptOverride` | 渠道级注入或覆盖系统提示词 | request | `inject_message`（敏感路径，需 `compat.sensitive`） | Y+（敏感路径权限） |

### 14.6 边界样本：这些要写代码，不属于规则

| # | 来源 | 为什么规则表达不了 | 怎么办 |
|---|---|---|---|
| B1 | new-api `relay/channel/ollama/`（1053 行） | 原生 `/api/chat` 是另一套线协议与换行 JSON 流 | 写适配器；但 Ollama 有 OpenAI 兼容端点，new-api 用 `OllamaOpenAIChat` 开关切过去，选兼容端点就回到规则 |
| B2 | new-api `relay/channel/baidu/`（用 key 换 `access_token`）、`tencent/`（三段式密钥走 TC3 签名）、`xunfei/`（HMAC 签名的 WebSocket 地址）、`jimeng/sign.go`（HMAC 签名） | 换 token 与签名类认证，属凭证域 | 凭证域代码（Okapi 已有 SigV4、Vertex 服务账号、OAuth） |
| B3 | new-api `relay/channel/task/`（约 1880 行）+ `plugins/tasks/*`（10 个 JS 插件），`replicate/`（`/v1/models/{model}/predictions` 预测任务接口）同类 | 提交 → 轮询 → 取结果的有状态异步任务 | 专门的任务框架，不在本引擎范围 |
| B4 | new-api 插件的 `usageSchema`（credits、duration、resolution）+ 计费表达式 | 新的计费维度（按秒、积分） | 价簿加新轴（代码 + 迁移）；规则只产出"事实槽位" |
| B5 | sub2api `service/openai_gateway_forward.go`（`adaptOpenAIResponsesClientTools`、`setOpenAIResponsesClientToolMapping`） | 请求侧改工具名，响应侧要按映射还原：需要请求级暂存的"可逆改写" | `apply: responses_tool_names`（Rust 内置命名变换：请求登记映射，响应自动还原） |
| B6 | Okapi `crates/okapi-providers/src/profiles/*`、`client_tls.rs` | 客户端指纹伪装（TLS、线序、签名校验） | 适配器域，永远不进规则 |

## 15. 分期、验收与待拍板

**P0（只预留，不写引擎）**：落实 §11 的接缝与约定：schema 与 `phase` 枚举、`if` 字段保留、保护键唯一注册表、`ModelResolver` 接口抽取、诊断里预留 `compat` 字段、`rule_sets` 的表结构定稿。

**P1（最小一刀）**：新建纯 crate（暂名 `okapi-compat`）；`rule_sets` 表与迁移；厂商内置数据（内嵌 JSON + 夹具）；厂商 / 渠道 / 护栏三层解析；在 S5 等价替换 strip / inject；诊断记版本与命中；管理端只读接口：有效规则、dry-run。**不做**：CEL、响应 / 错误 / 能力阶段、蓝图、MCP、路由、档案。

**P2**：适配档案；响应、错误、能力阶段；声明式 `ProbeSpec`；只读蓝图；MCP 只读与 `propose`；`openai_compat` 默认地址的修正。

**P3**：启用 CEL；蓝图编辑；路由阶段；"上游报不支持某参数 → 丢掉并重试一次 → 给管理员一条可固化成规则的建议"的自愈建议；按入口方言选同协议端点。

**验收**：
- 怪癖语料库（§14）的每一条有规则表达加夹具，覆盖率进 CI 报告；
- 每条内置规则有夹具，且"应用两次等于应用一次"；
- 属性测试：规则永远写不进受保护路径；
- `okapi-compat` 的用量模块纳入 `scripts/guard-no-float.sh` 的扫描范围；
- dry-run 与线上走同一份代码路径。

**待拍板**：
1. strip / inject 直接替换（建议，符合"不做历史兼容"，数据一次性迁移）还是并存一段时间；
2. 功能名是否叫"适配（compat）"；
3. 厂商自动识别只做建议（建议）还是静默应用；
4. 内置规则的更新节奏：每条带 `verified_at`，按月复核；
5. `usage_map` 允许整数加减（建议允许，否则 xAI、Gemini 这类要写代码）。

**风险**：
- `cel` crate 没有文档化的类型检查与资源上限，要自建（§7.3）；
- 流式响应规则的逐帧开销，只在存在匹配规则时才解析；
- 规则数量膨胀与顺序冲突，靠夹具、dry-run、命中轨迹控制；
- 内置厂商数据会随厂商变化过期，靠 `verified_at` 与监控（按规则统计错误率）发现；
- `ensure_stream_usage`（强制 `include_usage`）与厂商"拒收 `stream_options`"之间的优先级，见 #21，由 `billing_critical_ack` 兜住。
