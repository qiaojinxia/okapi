# 编程客户端与上游适配架构

## 范围与兼容边界

第一批落实 Claude Code / Codex 的架构：编译期上游注册表、独立凭证管理、
能力执行计划。客户端继续使用 Okapi API key；上游允许官方 API key 与站长自己的
订阅 OAuth 账号。既有 `channels.provider`、`channel_keys` 和加密凭证格式继续使用，
不新增账本、存储表或 Redis 键，不调整定价、预扣、退款和历史资源归属。

协议、凭证、客户端请求配置与传输是独立类型；只有适配器明确注册的组合才可执行。
OAuth 不表示该账号拥有所有 API 能力。订阅适配保持实验性，真实账号行为须联调。

## 两类客户端如何接入

| 客户端入口 | 官方 API key 渠道 | 自用订阅 OAuth 渠道 |
|---|---|---|
| Claude Code：`/v1/messages` | `anthropic` | `anthropic_max` |
| Codex：`/v1/responses`、`/v1/responses/compact` | `openai`（原生 Responses） | `codex` |

两类客户端均持有 Okapi 用户 API key，上游凭证只存储在渠道中。模型映射、可见分组、
额度和权限仍由现有控制面配置，注册适配器不会自动授予用户模型访问权。

Claude Code 的网关地址为根地址，例如 `ANTHROPIC_BASE_URL=http://127.0.0.1:8080`，
客户端凭证使用 Okapi key；可通过 `ANTHROPIC_API_KEY` 接入（本轮 `--bare` 联调所用），
普通 gateway 模式也可配置 `ANTHROPIC_AUTH_TOKEN`。Codex 自定义 provider 的 `base_url` 为
`http://127.0.0.1:8080/v1`、`wire_api="responses"`，通过 `env_key` 读取 Okapi key，
并配置 `requires_openai_auth=false`。`supports_websockets` 仅在渠道支持 WS 且已验证时开启。
这两种配置保持各自原生协议，适配器与凭证选择发生在网关内部。

## 模块与依赖

```text
Messages / Responses / Chat / Gemini 入口
    -> 既有身份、模型、限额与预扣
    -> ExecutionPlan（注册表 + 入口 + 上游模型 + 渠道能力）
    -> 既有账号调度、缓存亲和与历史硬绑定
    -> CredentialManager（static / oauth / cloud identity）
    -> 注册的上游实现（原生体或显式方向转换）
       -> 适配器自己的请求扩展（配置校验、请求体/头处理）
    -> HTTP / SSE / WebSocket
    -> 既有用量采集、结算与诊断
```

- `okapi-providers::registry`：注册 ID、适配器类型、方言、凭证类型、默认地址、原生
  Responses 策略、可用入口和传输。只保存能力元数据，不依赖 store/ledger。
- `gateway::execution_plan`：把当前渠道与请求编译成可执行操作，集中校验入口、工具、
  图像、服务端工具、compact 与传输。保留“通用能力显式 false 才排除”的旧约定；
  原生协议和 WS 必须有已注册实现。未知适配器不回退到 OpenAI。
- `gateway::credentials`：统一解析静态 key / OAuth / 云身份；OAuth 流程位于子模块，
  复用进程内单飞、Redis 锁、重读、刷新与回写。`oauth_cred` 保留兼容导出供旧调用方迁移。
- `gateway::dialect` 与 `openai_dialect`：按已注册适配器选择现有客户端；协议转换继续
  按方向拆分。执行计划只保存操作元数据，不构造统一请求/响应 IR。

渠道能力、原生 Responses 设置和入口诊断共用同一套计划规则。请求体构造与发送也重新
核对计划，防止候选过滤与最终执行使用不同的协议判断。WS 每轮继续复查权限及渠道配置。

## 必须保持的不变量

1. 同方言保留原始请求字段、缓存标记、推理项与事件；转换只走现有方向模块。
2. Codex 订阅只接 Responses/compact；Messages 不改投 Gemini 方言；compact 不降级 chat。
3. `previous_response_id` 绑定用户、API key 与上游历史空间，不因缓存亲和/刷新而改绑账号。
4. OAuth 401 只在已确认未执行的认证失败路径强刷一次；WS 发出 create 后不重放。
5. 请求路径、后台预刷新与手动刷新共用凭证流程与分布式锁。
6. 用量、价簿快照、预扣和结算沿用既有链路；不把订阅剩余额度当成用户钱包。

## 扩展与验收

新增上游先注册描述与受支持的组合，再在集中适配分派中接入实现；共用入口、凭证接口、
执行计划与账本。新增 OAuth 提供刷新实现，新增协议转换提供独立方向模块。
注册表是编译期扩展点，新增实现需要编译发布；渠道配置驱动已有实现的选择，
不执行管理员提供的任意脚本。前端的渠道选项也应与新增注册项同步。

验收覆盖注册表唯一性、入口矩阵、显式能力拒绝、未知适配器、静态/OAuth/云凭证选择，
以及隔离 PG/Redis 的 OAuth 刷新、Messages、Responses、历史绑定、compact 与 WS/桥接
集成测试。真实账号、多轮 CLI 和连接缓存驱逐单独记录，mock 通过不代表供应商验收。

账号额度观测现已通过账号 hook 接入；真实供应商账号与多轮工具调用仍需单独验收。

## 第三批：客户端请求配置扩展

扩展放在 `okapi-providers::profiles`。`Outbound.context` 只携带渠道扩展配置、稳定身份种子
和白名单客户端头；HTTP 层不解释这些内容。store 只读取 JSON，gateway 的 `extensions`
模块做上下文装配；调度、限额、预扣、凭证刷新、结算和重试继续使用既有实现。
请求扩展在协议转换之后、真实发送之前执行，不能选择账号、刷新凭证、访问账本或自行重试。
注册表声明每个适配器支持的扩展，管理面创建/修改与执行计划都校验支持范围。

当前 `anthropic`（API Key）和 `anthropic_max`（OAuth）可选择同一种 Claude Code 请求风格。
profile 不改变凭证类型：API Key 仍发送 `x-api-key`，不添加 OAuth 专属 beta、不进入刷新流程；
OAuth 仍通过共享凭证管理器获取 access token，Bearer 与 OAuth beta 由订阅适配器负责。
Codex、OpenAI、Azure、Bedrock、Vertex 等尚未注册此扩展，配置时明确拒绝。

通过现有渠道管理 API 的 `settings` 配置（API 会替换整个 settings 对象，更新时应保留其他设置）：

```json
{
  "extensions": {
    "client_profile": {
      "name": "claude-code",
      "mode": "auto",
      "revision": "2.1.286",
      "entrypoint": "cli"
    }
  }
}
```

- `auto`：合法 metadata 身份格式配合 Claude CLI UA 或 billing block 识别原生客户端，
  保留其原始请求体、system、cache_control 和 thinking signature；普通客户端使用所选配置整形。
  该识别只决定兼容处理，不授予任何权限。
- `passthrough`：保留 body，转发白名单身份头。
- `mimic`：显式整形请求；上游账号身份由当前 key 的稳定种子和 OAuth 账号 UUID 构建，
  不沿用普通客户端任意提供的 user_id，其他 metadata 字段保留。
- `{"name":"native"}`：明确关闭此请求扩展，覆盖旧 `mimic_cc` 设置。
- 无 `extensions.client_profile`：保持既有渠道行为，旧 `mimic_cc` / `mimic_cc_version` 仍兼容。

revision 选择一整套 UA、SDK/runtime、beta、body 规则，目前注册 `2.1.258` 和本机抓包
对齐的 `2.1.286`。旧配置缺省仍为 `2.1.258`；管理页新建/导入模拟配置缺省选 `2.1.286`。
`entrypoint` 支持 `cli` / `sdk-cli`；`request_class` 支持 `main` / `auxiliary`（缺省 main）。
旧版配置拒绝 SDK/auxiliary 组合，避免版本和字段混搭。
不允许只把版本号改成尚未实现的新版本；增加新版须在 Provider 中增加整套配置并补请求样本。
`2.1.286` 模拟配置自动生成新版 cch；计算最终出站字节，调用方无需提供该字段。
原生 auto/passthrough 仍完整保留客户端的字节与 cch。旧版配置不会借用新版规则。
新版 cch 已与官方客户端样本一致；完整官方功能的兼容范围见下文验收记录。
指纹的文本索引改用 JavaScript UTF-16 code unit，含 emoji 的回归样本覆盖代理项编码。

### 通用 API 调用

在渠道里一次性选择 Claude Code 模拟、版本 `2.1.286`，API 调用方继续使用自己的
Okapi API Key、模型和对话。无需发送 Claude Code token、UA、SDK 版本、billing block、
metadata 身份或 cch；这些由渠道凭证与 Provider profile 分别提供。

```http
POST /v1/chat/completions
Authorization: Bearer <Okapi API Key>
Content-Type: application/json

{
  "model": "claude-sonnet-5-5",
  "messages": [{"role": "user", "content": "Hello."}],
  "stream": true
}
```

请求顺序为：既有鉴权/调度/预扣 → 协议转换 → 客户端 profile 补齐 → 最终 JSON 序列化
→ cch 计算与原位替换 → 凭证 adapter 添加认证头 → 既有 HTTP/SSE 与结算。
工具调用时按普通 API 传 tools、tool_calls 和 tool 消息；Provider 转换成对应上游字段，
工具执行仍由调用方负责。更换客户端版本只新增 profile，不在通用流程加入版本判断。

模拟模式支持有效 `x-claude-code-session-id`，body 与出站头共用此会话 ID；相同开场白的不同
会话可明确分开。不提供会话 ID 时仍采用旧的首条用户文本近似，无法区分完全相同的开场白。
设备身份种子为 channel_key_id，token 轮换不改变设备身份。

新增扩展的改动范围：Provider 内的配置解析、实现、注册声明和用例。通用请求阶段的顺序
保持固定，不向鉴权、调度、账本或 worker 主循环增加客户端专属分支。此处是编译期扩展，
不是运行任意脚本或动态加载代码的插件系统。

## 第四批：直接导入 Claude Code token

管理页新建渠道选择 `anthropic_max` →「直接导入 Token」，填写自己的 Claude Code
access token（`sk-ant-oat…`）、模型和池，再选择「模拟 Claude Code 客户端」并新建。
导入模式缺省选择 mimic；直接连接真实 Claude Code 时建议改用 auto / passthrough。
编辑渠道的接入页可替换 token，多 key 时必须明确选择目标。修改请求风格通过渠道保存提交，
替换 token 单独提交。两种写操作沿用既有权限、凭证封装和审计；token 不返回列表或进入审计 detail。

普通建渠道 API 也接受裸 access token：`POST /admin/channels` 的 `provider` 选
`anthropic_max`，`credential` 填裸 token，`settings.extensions.client_profile` 使用上面的
配置并将 `mode` 设为 `mimic`。导入归一化发生在管理面；网关仍使用同一个 CredentialManager。
不把 Claude OAuth token 当 Anthropic API key，不新增用户账本或特殊计费链路。

只有 access token 时不具备刷新权限，后台不会提前刷新，手动刷新也不会误停用仍可用的 token。
未知到期时间明确记为未知，不虚构有效期；若导入的 JSON 带 `expires_at`（Unix 秒），则按已知
到期时间停用。没有新凭证可接替时，上游明确返回 401 后不重发、不请求 token 端点，失效 key 等待人工
更换或重新授权；请求失败仍沿原链路退款。完整可刷新凭证 JSON 的格式如下（全部值为占位）：

```json
{
  "kind": "oauth",
  "access_token": "sk-ant-oat01-REPLACE",
  "refresh_token": "sk-ant-ort01-REPLACE",
  "expires_at": 2000000000,
  "account_id": null
}
```

提供 refresh token 时必须同时提供有效的到期时间，继续使用原有锁、CAS 与刷新流程。
这个导入入口不解析浏览器 cookie 或任意客户端配置文件；Codex 仍要求含 account_id 的完整
OAuth 凭证或浏览器授权。当前 mimic 配置仍为 2.1.258，实际供应商是否接受必须真实账号联调。

## 第二批：预刷新与重新授权

worker 独立运行 OAuth 预刷新任务，keyset 分页扫描启用渠道的 OAuth key；只处理
active / cooling / rate_limited，invalid / banned / quota_exhausted 不自动恢复。
默认提前 300 秒刷新，30 秒一轮，每页 100 把、并发 2、每个 worker 每秒最多启动 1 次刷新。
全局 `oauth_refresh_policy` 可调整 `enabled`、`interval_secs`（5–300）、
`refresh_margin_secs`（120–3600）、`batch_size`（1–1000）、`concurrency`（1–8）、
`requests_per_second`（1–5）。各轮轮转分页，避免第一页占住全部工作。

共享刷新流程使用带随机持有者的 Redis 租约（90 秒，释放时比较持有者）；Redis 故障不
无锁刷新。PG 回写按原密文字节比较，管理员重新授权或轮换后，旧刷新不能覆盖新凭证，
旧 invalid_grant 也不能把新凭证标成 invalid。跨进程遇到正在刷新的 key 时，未过期 token
可继续使用；已过期或强刷请求有界等待最多 45 秒并重读凭证，Redis 出错则立即失败回退。
瞬态失败保留未过期 token，并按
30 秒起步、最多 15 分钟退避；401 强刷一次和手动刷新可越过退避。后台停用不关闭请求时刷新。

Redis 保存每把 key 的脱敏刷新观测（最近尝试/成功、连续失败数、下一次重试时间、错误码），
TTL 30 天；不保存 token、账号标识、授权码、上游错误原文，也不把未公开的订阅剩余额度
推断成数值。列表附带观测及重新授权状态。管理页可手动刷新、对指定旧 key 重新授权；
重新授权恢复 key 状态，但不启用已停用渠道。已知账号不同则拒绝替换，应新增账号 key。
OAuth state 绑定发起管理员与目标，换码前复查属主和目标归属，并捕获原凭证版本；
延迟的换码响应也不能覆盖此期间完成的刷新或重新授权。

真实客户端联调先使用隔离配置与 mock 上游验证客户端协议；供应商账号联调需要
已授权且可用的对应渠道，未验证的账号能力不标记为通过。

### 第一批验证记录（2026-10-01）

- `SQLX_OFFLINE=true cargo check -p okapi` 通过。
- `cargo clippy -p okapi -p okapi-providers --lib --test gateway_oauth_channels -- -D warnings` 通过。
- Provider 单测 97 项、应用单测 159 项通过。
- 隔离 PG / Redis / ClickHouse / NATS，逐套件重置测试数据；16 个集成套件共 104 项通过：
  OAuth、Messages、Responses、能力过滤、历史绑定、原生 WS、HTTP 桥接、Azure、Bedrock、
  Vertex、Gemini 及其入口、模型降级、Anthropic、出向配置和跨页面账单一致性。
- 新增 Codex 404 回归用例，验证仅调用上游一次、不刷新凭证、不降级 Chat，并全额退回预扣。

全目标 Clippy 曾被同一工作区另一路改动中的 `gateway_usage_modalities::setup_inner`
超长函数告警阻断；上面的 Clippy 结果仅指列明的目标，不能替代全工作区 CI。
以上 360 项为第一批完成时的本地单测与 mock 集成验证，当时真实 CLI 和订阅账号尚未联调。

### 第二批验证记录（2026-10-01）

- OAuth 集成 22 项通过，覆盖后台提前刷新、停用、退避、手动恢复、保留调度冷却、
  失效后指定 key 重新授权、账号一致性、state 管理员/目标绑定、分页推进、跨进程慢刷新，
  以及延迟刷新/invalid_grant/换码响应不能覆盖新授权。钱包未因凭证维护发生变化。
- 应用单测 162 项、store 凭证单测 11 项通过；Messages、Responses、历史绑定、WS 与
  HTTP 桥接 5 个套件 58 项通过。管理面相关用例 10 项通过。
- 一个额外统计用例 `console_manage::stats_surface_exposes_clickhouse_views` 未通过：
  4 GiB 的隔离 ClickHouse 首次被 OOM 杀死，降低测试查询并发后复跑仍命中 Code 241：
  查询内存约 3.63 GiB、总内存护栏 3.60 GiB。最后管理面定向执行明确排除此项；
  没有扩大生产查询限制，以上结果不代表全工作区统计/容量验收通过。
- `SQLX_OFFLINE=true` 编译与指定目标严格 Clippy 通过，新增 SQLx 查询离线元数据已保存。
  前端构建、改动文件 lint、中英翻译闸通过；3 项维护界面交互 + 1 项既有 OAuth 登录卡回归通过。
- 本机 Claude Code **2.1.287** 与 Codex **0.159.0** 均通过网关到隔离 mock：
  使用临时 Okapi key，分别选择 `anthropic_max` / `codex`，最终文本与账单渠道/key 归属核对成功。
  独立 opt-in 测试位于 `gateway_oauth_channels::programming_clients`，默认忽略，需要显式提供
  `OKAPI_TEST_CLAUDE_BIN` / `OKAPI_TEST_CODEX_BIN`；不依赖开发者现有客户端配置、OAuth 账号或插件。
  Claude 使用 `--bare` / `--restricted`、禁用工具/MCP；Codex 使用 `--ignore-user-config` / `--ephemeral`
  及自定义 provider env key。实际 home 路径和用户配置均未改动。

此轮 CLI 验证仅覆盖基础文本、SSE 终态及路由/结算；真实供应商订阅、工具执行循环、
多轮会话和实际账号额度仍未验收。当前代码在工作区，未重启已有服务，也未提交或推送。

### 第三批验证记录（2026-10-01）

- Provider 单测 103 项通过；执行计划 7 项、调度 6 项、渠道扩展校验 1 项通过。
- 独立 PG / Redis 与本地 mock 上游的四个集成套件共 46 项通过：Messages、Responses、
  OAuth 渠道及出站配置。新增用例核对原生客户端 system/metadata 保留、API Key 不进入
  OAuth 流程、不受支持的组合在建渠道前拒绝，以及单次请求只有一笔结算且钱包差额一致。
- `SQLX_OFFLINE=true cargo check --workspace --all-targets` 与
  `cargo clippy -p okapi -p okapi-providers --lib -- -D warnings` 通过。
- 集成目标严格 Clippy 仍被既有 `support/programming_clients.rs` 的 `format_collect` 和
  `too_many_lines` 两项告警阻断；本轮没有修改该客户端 smoke 文件。
- 本批默认忽略 opt-in 本机 CLI smoke，未访问真实供应商账号。旧 2.1.258 配置的 mock
  通过不代表更新版本的 mimic、cch 或供应商兼容性已验收。隔离测试容器在验证结束后移除。

### 第四批验证记录（2026-10-01）

- 凭证封装与解析单测 12 项、导入校验 2 项、共享凭证管理单测 2 项通过。
- 独立 PG / Redis 与本地 mock 的 Messages、OAuth、Responses、出站配置四个套件共
  50 项通过。新增 4 项覆盖裸 token 到 Bearer/mimic 的真实管理与网关链路、单次结算、
  后台不刷新 token-only、列表不泄露 token 或虚构到期时间、401 后停用与退款、指定 key
  替换恢复、已知到期拒绝、手动刷新不误停用，以及坏凭证在开通前拒绝。
- 管理页 5 项 OAuth/导入交互与 2 项原有渠道写表单回归通过；前端构建、改动文件 lint、
  中英翻译与文案检查通过。多 key 替换明确提交 channel_key_id，编辑请求风格保留其他设置。
- 全工作区所有目标编译通过，`okapi` / `okapi-store` / `okapi-providers` 库严格 Clippy 通过。
- 本批未访问真实供应商、未运行 opt-in CLI 测试、未重启现有服务。测试容器已移除；
  没有修改正在使用的数据库。真实账号验收和新版本 mimic 签名仍未完成。

参考：[Sub2API 账号调度](https://github.com/Wei-Shaw/sub2api/blob/d6adebd22de00478cd021119ba755f37bcb94fb5/backend/internal/service/openai_account_scheduler.go)、
[Claude Code 协议](https://code.claude.com/docs/en/llm-gateway-protocol)、
[Codex 配置](https://learn.chatgpt.com/docs/config-file/config-reference)。

## 渠道账号控制

`gateway::account_control` 独立于客户端 profile，负责渠道额度准入和订阅额度调度。
刷新策略复用 CredentialManager；额度探测协议和窗口选择在 providers 插件中。
配置、软/硬限制边界及验收见 [channel-account-controls.md](channel-account-controls.md)。


## 账号行为 hook 与渠道公共限制

固定流程仍由通用层负责：候选筛选与限制 → 统一凭证管理 → 上游调用 → 用量结算。
`ProviderDescriptor.account` 注册可选 `AccountHooks`，提供 `quota` 与 `refresh` 行为。
提供商插件封装端点、请求头、额度周期和响应解析，只返回统一额度快照或 token 结果；
不接触 PG、Redis、调度或账本。锁、刷新协调、身份校验、冷却和观测时效继续由通用层管理。

账号维护扫描从注册表获取支持行为的提供商列表，不把提供商名或 OAuth 类型写进扫描 SQL。
API Key 提供商也可以注册仅有额度查询的 hook，不需要修改网关准入、worker 或通用表单。
控制面通过 `/admin/channels/providers` 下发能力描述；只有声明订阅能力的插件显示订阅额度和授权管理。

界面分成通用渠道控制和订阅额度控制：并发、限流暂停和连续失败暂停适用于所有渠道；
订阅 5 小时／周百分比上限、上游额度查询、本地 Token 上限和授权管理只在订阅渠道显示。
插件声明可配置的窗口时长，通用准入按上游实际时长分别匹配百分比限制，不识别提供商名。
窗口和重置时间来自上游；本地 Token 限制明确使用该渠道已记录的输入加输出历史，默认累计总量，
也可按机器时区自然日／周重置。累计值与账单同事务更新，历史清理不会归零。
旧的组合累计限制仍停用，不会自动复活旧配置；金额限制隐藏，也不会用本地账单推算官方订阅额度。
只导入访问 token 时不显示自动续期。
编译期插件需随二进制发布，配置层只选择已注册能力。
