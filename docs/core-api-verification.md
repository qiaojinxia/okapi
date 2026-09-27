# 核心功能与 API 验证（进行中）

目标：验证 Okapi 核心功能，补齐与 New API、Sub2API 的核心能力差距，逐个验证 API 的正确性。本文不把历史记录、静态路由数量或错误壳检查等同于业务验收通过。

## 对标基准与证据

- 本地起点：`31d733aab61caffee08465074730a36507c1cced`，含开始验证前已存在的未提交变更；验证对象是当前工作树，不是该提交的干净版本。
- New API：[`c2b7a9a9e0b548c2051a949fceabb59029adcb49`](https://github.com/QuantumNous/new-api/tree/c2b7a9a9e0b548c2051a949fceabb59029adcb49)，2026-09-25。
- Sub2API：[`a3eb7ef302961cba716dc78b39b93b60c467db0e`](https://github.com/Wei-Shaw/sub2api/tree/a3eb7ef302961cba716dc78b39b93b60c467db0e)，2026-09-23。这里的 subapi 指此前图表对标使用的 Wei-Shaw/Sub2API。
- 主要证据：[New API 路由](https://github.com/QuantumNous/new-api/blob/c2b7a9a9e0b548c2051a949fceabb59029adcb49/router/relay-router.go)、[README](https://github.com/QuantumNous/new-api/blob/c2b7a9a9e0b548c2051a949fceabb59029adcb49/README.en.md)；[Sub2API 路由](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/server/routes/gateway.go)、[README](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/README.md)。外部项目目前只做源码核对，尚未实际运行，因此不能据此比较可靠性或性能。

证据分级：**路由/实现存在**仅说明静态实现；**业务测试通过**需要本轮实际执行且断言覆盖指定行为；**错误壳通过**只验证响应形式；**缺口**需要实现和行为测试；**未验证**不能当作通过。模拟上游不等于真实供应商联调。

## 当前核对结果

原生 `GET /v1/responses`、HTTP/SSE 桥接和 native/http/auto 协议选择已接入，持久图片异步队列已增加 URL 转存与私有 S3 后端。长期冻结、确认前取消、幂等结算及 worker 恢复已具备；后续已接入默认关闭的原生 Batch API、逐项结果解析、按成功图片结算、未知提交查找和删除/到期回收。任务与 pending 资金意图原子创建，支持原 API key 归属、默认 20 条后端分页、组合筛选及结算后单图/整批 ZIP 私有下载。统计补记已接入成员消费、月 Token/金额、渠道日消费和实时 KPI，真实耗时取封存时间；创建和发送前复查现有软限额。批任务现已与普通调用共用密钥并发上限，满额保持排队且不冻结资金；取消、未知提交和重启期间保留占用，真正结算关闭才释放。

最新完整离线工作区回归 `/tmp/okapi-sub-lifecycle-full.log` 已观察实际退出 **0**：**1,010 passed、0 failed、0 ignored、0 filtered、0 软跳过**，133 个完整套件/文档块，无解析错误或未结束块。订阅 HTTP 36、domain 12、pricing 31（含对拍/性质）、ledger 111 均包含在这一次运行；433 条权限/公开契约/错误探针另计。严格离线 workspace/all-targets Clippy、静态守卫通过，1,129 个后端文件全程未变。本阶段修复续期与过期竞态、失败用户阻塞恢复队列、套餐可兑现性与支付换算边界。支付回调金额/币种/状态校验和普通预扣跨窗语义仍未完成，不能据此宣称完整对标完成。未修改/构建前端、未部署。

此前完整离线工作区回归 `/tmp/okapi-subscription-full-r2.log` 已观察实际退出 **0**：**1,000 passed、0 failed、0 ignored、0 filtered、0 软跳过**，133 个完整套件/文档块，无解析错误或未结束块。包括订阅 HTTP 26 项、domain 12、pricing 31（5 对拍、6 性质）、ledger 111；433 条权限/公开契约/错误探针另计。严格 workspace/all-targets 离线 Clippy 与静态守卫通过，1,116 个后端文件全程未变。订阅权益快照、同源幂等、受理后补发和财务恢复已具备本地业务证据，迁移旧条款、普通预扣跨窗口语义等仍有边界，见 [订阅持久发放契约](durable-subscriptions.md)。这不是完整竞品对齐或生产性能验收；未修改/构建前端、未部署。

此前完整工作区回归 `/tmp/okapi-retention-full.log` 已观察实际退出 **0**：**978 passed、0 failed、0 ignored、0 软跳过**，133 个完整套件/文档块，无解析错误或未结束块。新增 14 项历史留存验证，覆盖两池余额结转、历史退款、防重复扣费、迁移导入防重、并发清理、事务回滚和大额累计边界；domain 12、pricing 31（含 5 项对拍与 6 项性质测试）、ledger 109 全部通过。433 条权限/公开契约/错误探针另计。严格 workspace/all-targets Clippy、离线 SQLx 及静态守卫通过，1,074 个后端文件全程未变。订阅权益发放的 source 幂等、权益快照与失败补发仍是已知缺口，不能据测试总数宣称整体对标目标完成。本轮未修改/构建前端，未部署。

此前完整工作区回归 `/tmp/okapi-fund-sequence-full.log` 已观察实际退出 **0**：**964 passed、0 failed、0 ignored、0 软跳过**，132 个完整套件/文档块，无解析错误或未结束块。包含钱包支付/兑换、管理员退款的持久恢复、PG 失败原子回滚、迟到 Redis 命令防重，以及此前后端分页与普通结算修复。domain 12、pricing 31、ledger 109 均在该轮通过；433 条权限/公开契约/错误探针另计。严格 workspace/all-targets Clippy 与静态守卫通过，1,049 个后端文件全程未变。订阅权益发放、历史事件清理后的余额结转等实质缺口继续保留，不能据测试总数宣称整体对标目标完成。本轮未修改/构建前端，未部署。

此前管理分页阶段完整工作区回归 `/tmp/okapi-management-pages-full.log` 已观察实际退出 **0**：**950 passed、0 failed、0 ignored、0 软跳过**，132 个完整套件/文档块，无解析错误或未结束块。包含资金用户锁修复、未知用户充值拒绝，以及 12 类管理/门户资源默认 20 条后端分页。domain 12、pricing 31、ledger 101 均在本轮内通过；433 条权限/公开契约/错误探针单列。严格 Clippy 和静态守卫通过，1,029 个后端文件全程无变化。跨库崩溃补偿、其他核心能力与真实供应商联调仍未完成；本轮未改前端、未部署。

上一完整工作区回归（普通持久结算阶段）`/tmp/okapi-durable-sync-full-r2.log` 已观察实际退出 **0**：**941 passed、0 failed、0 ignored、0 软跳过**，132 个完整套件/文档块，无解析错误或未结束块。domain 12、pricing 单元/对拍/性质 31、ledger 96 均包含在这一轮内；433 条权限/公开契约/错误探针单列，不等于 433 个成功业务 API。严格 Clippy 和静态守卫通过；1,022 个后端验证文件全程无新增、删除或内容变化。旧积压夹具的等待冲突和 SSE 金额预期失败证据均保留在文末。此次全量未验收生产吞吐、实际云供应商或整个前端，核心对标目标仍未完成。

上一轮完整工作区回归（共享并发阶段、下述目录分页、普通预扣/结算/退款校验和持久结算变更之前）`/tmp/okapi-batch-concurrency-full.log` 实际退出 **0**，**895 passed、0 failed、0 ignored、0 软跳过**，131 个完整套件/文档测试块，无未结束块或解析错误。Batch HTTP 55 项、长期冻结/存储 52 项、原生协议 31 项、PG 记账 7 项以及日志/门户/统计/分析均通过；431 条权限/错误探针单列。严格全工作区/all-targets Clippy、格式及静态守卫通过。这是单次完整运行的实际结果，不是合并历史定向结果；此前两轮的缓存样本失败与修正记录仍保留在文末。

源码指纹覆盖 1,220 文件，完整运行期间后端源码、Lua、测试、配置及迁移均无新增、删除或内容变化。共享工作区的前端构建产物期间更新了 index 和 58 个资源（旧资源被替换）；本任务未修改或构建前端，不能宣称整个文件快照完全一致。验证范围是当前后端和各用例实际断言，不是整个前端包的固定版本验收。

当前目录变更已将 `/api/pricing` 与 `/api/pricing/models` 统一为默认 20、最多 100 条，分组和厂商列表独立分页，新增 `/api/pricing/groups`。后端倍率同步会逐页读取并在任何一页失败时拒绝部分价格表。目录、诊断、门户、同步和路由 34 项关联通过，库单元及同步 81 项通过，最后新增容量/页数边界后的完整同步 6 项通过；各次结果不相加。本阶段未重跑整个工作区。前端调用方仍需接入新分页契约，不能将新后端与旧全量读取前端视为已完成的整站交付。持久结算凭据兼容和共享并发的证据保留在前一阶段。

随后修复普通预扣的数据异常部分写入与负数预估问题：不限额的计数器也在扣款前验证。当前 `/tmp/okapi-reserve-atomicity-isolated.log` 在新建专用 PG 测试库、Redis DB15 上单次 **162 passed、0 failed、0 ignored、0 软跳过**，12 个关联套件全部完成；严格 Clippy 和静态守卫通过。旧积累库关联运行的 125 通过/11 容量拒绝记录保留，核实为默认 32 GiB 预算即将耗尽；没有清空旧库、调整生产上限或放宽断言。详细证据见文末，此次结果不是完整工作区回归。

随后完成普通结算/退款异常原子性与订阅失败账单归属修复：17 套件关联运行单次 **245 passed**；之后新增重复退款池归属边界修复，最终八套件 **74 passed、0 failed、0 ignored、0 软跳过**。两次结果不相加，也不是全工作区复测。当时发现普通成功请求缺少跨进程持久结算恢复；后续实现与验证见文末，整体目标未完成。

此前的请求次数阶段修复了批任务创建准入：日窗口与普通请求一致，key RPM/RPD、用户×模型 RPM、分组 RPM/RPH 按展开子请求数原子检查，并发及跨网关幂等重放只占一次。请求次数阶段关联回归为 **164 passed、2 failed**，10 个完整套件；失败来自并行账单明细测试中缓存写入样本为 10、旧断言为 0，当前断言同步后完整 PG 记账 7 项复测通过。新准入 9 项包含在 Batch HTTP 51 项内，持久资金 48 项、协议 31 项及其他关联套件通过，431 条路由探针另计。该阶段严格 Clippy 和静态守卫通过，未重跑全工作区；之后共享并发阶段的 895 项完整结果见上，不拼接不同运行的计数。

图片 URL 转存与私有 S3 已有受控上游及故障恢复测试，独立 S3 实服/真实云联调仍未完成。原生批处理的 Gemini 与 Vertex/GCS 受控业务、未知创建查找、产物回收、ZIP 和组合筛选已覆盖；完整限流、多模态用量、下载上限性能与实际云端验证仍有缺口。Responses WS 的中途干预、conversation/item/file 引用生命周期及下述未验证项仍需继续处理。权限/错误探针不等于所有 API 的成功业务场景已覆盖，也不能据当前测试声称已全面超越竞品。

| 核心域 | 当前本地证据入口 | 必须验收的行为 | 本轮状态 |
| --- | --- | --- | --- |
| Chat / Messages / Gemini | `gateway_m1`, `gateway_messages`, `gateway_gemini_ingress`, `gateway_compat` | JSON/SSE、工具、模型映射、usage、鉴权、方言错误壳 | 最新全量复测通过；证明范围以各套件实际断言为限 |
| Responses HTTP | `gateway_responses`, `gateway_oauth_channels` | 原生/转换、续聊字段、工具和 reasoning、流事件、计费、降级边界 | `gateway_responses` 13 项、`gateway_oauth_channels` 6 项及新增 `gateway_response_affinity` 10 项定向通过；HTTP 账号绑定已实现，跨实例、故障、归属和费用边界见下文。conversation/item/file 生命周期仍有缺口 |
| Responses WebSocket | `gateway_responses_ws`、`gateway_responses_ws_bridge`、`responses_ws_transport`、`http_tls_tests` | 多轮、增量输入、连接/请求并发、异常关闭、断线结算、HTTP 桥接与原生上游 | 原生 GET 入口 14 项测试已通过；HTTP/SSE 桥接及 native/http/auto 选择已接入，15 项桥接测试通过；中途干预和实际供应商联调仍未完成，不能计为完整协议对齐 |
| Responses 子接口 | `chat::responses_compact`、providers `send_compact_at`、`gateway_responses`、`gateway_token_count` | compact 的原生响应、usage、定价、权限与失败退款；input_tokens 的独立准入、来源标记及无扣费 | compact 4 项集成测试随 13 项 Responses 套件通过；input_tokens 已实现，10 项定向集成测试通过，未做实际供应商联调 |
| Embeddings / Rerank | `gateway_embeddings` | 数组/编码、模型能力匹配、两种 rerank 形状、usage 与结算 | 最新全量复测通过；证明范围以各套件实际断言为限 |
| 图片、音频、视频、Realtime | `gateway_images`, `gateway_images_contract`, `gateway_audio`, `gateway_videos`, `gateway_realtime` | 多图/时长定价、上传校验、任务归属、轮询/内容、WS 连接租约、失败退款 | 图片原有 3 项与新增契约 16 项随最新全量通过；JSON 编辑、多图上传、实际张数结算与重定向/容量边界见 [图片契约](images-contract.md)。流式图片与图片 Token 定价仍有缺口；其他证明范围以实际断言为限 |
| 异步图片与批量任务 | `gateway::images::tasks`、`image_tasks` / `image_batches` store、`worker`、`providers::batch`、`ledger::holds` | 创建、幂等、持久任务、重启恢复、轮询、取消、归属、下载、失败退款、产物回收 | 原生协议 31 项、长期冻结/批任务存储 52 项、Batch HTTP 业务 55 项及已有图片契约、worker 恢复随最新全量通过。未知提交查找、Vertex 结果收集、删除/到期回收、ZIP 和筛选已具备受控业务证据；完整准入、最大下载容量和独立 S3/真实云端联调仍未完成。见 [图片契约](images-contract.md)、[原生批处理边界](native-image-batches.md)、[批任务存储契约](native-image-batch-jobs.md) |
| 路由、调度、凭证池 | `gateway_m2_sched`, `gateway_routing_prefs`, `gateway_capabilities`, `gateway_retry_policy`, `gateway_midstream`, `channel_key_lifecycle` | 权重/优先级、粘性、并发/限流、重试边界、冷却、流中断不重复收费 | 最新全量复测通过；证明范围以各套件实际断言为限 |
| 价格与账本 | pricing/ledger crate tests、`billing_surface_parity`, `gateway_pricing_rules`, `gateway_tier`, `gateway_upstream_cost`, `gateway_untrusted_usage` | 微美元精度、缓存读写、tier、模型实际身份、幂等、并发、原额退款、未知 usage 不伪造零费用 | 最新全量复测通过；价格规则现为 12 项，包含新增 5 项负载生命周期回归，实际证明范围见下文 |
| 用户/权限/会话 | `console_auth_web`, `console_oauth`, `console_users`, `console_visibility`, `portal_ownership` | 身份验证、TOTP、会话撤销、角色/资源归属、密钥约束、禁止跨用户读取写入 | 现有业务套件随全量通过；New API 的 passkey 等身份能力仍需继续核对 |
| 渠道/模型/分组/规则管理 | `console_manage`, `console_channel_writes`, `console_pricing_write`, `list_pagination`, `console_write_receipts` | 完整 CRUD、搜索/后端分页、校验、重复提交、缓存失效、改价发布 | 最新全量复测通过；证明范围以各套件实际断言为限 |
| 充值/兑换/套餐/团队 | `console_pay`, `console_redemption`, `console_subscriptions`, `console_teams` | 签名校验、幂等支付回调、并发核销、周期配额、团队限额与归属 | 最新全量复测通过；证明范围以各套件实际断言为限 |
| 模型目录 | `console_model_catalog`, `console_portal_pages`, `console_diagnose` | 服务端分页、先筛选后分页、厂商统计、分组/入口能力、字段白名单、旧导出兼容 | 当前目录 13 项关联通过；模型、分组、厂商都默认 20/最大 100，两个目录入口均有界，支持独立分组查询。倍率同步后端已逐页适配；前端一次性读取调用方尚未迁移，不能当作整站功能完成 |
| 日志/统计/分析 | `console_logs`, `console_stats`, `console_analytics`, `console_portal` | 时间窗、维度名称、金额/Token 口径、可见性、数据缺失、分页/筛选 | 当前全量中统计 9、分析 13、日志 13、门户 1+8 项通过；此前缓存样本失败的修正也包含在本次完整运行。前端模拟接口测试另计 |
| 运维/可靠性/迁移 | `worker_*`, `gateway_multipod`, `gateway_backlog`, `gateway_shutdown`, `migrate_*` | PG→outbox→NATS→CH、重复消费、DLQ、对账修复、背压、重启/停机、迁移幂等 | 最新全量复测通过；证明范围以各套件实际断言为限 |
| MCP / 安全边界 | `console_mcp*`, `console_ssrf`, `gateway_outbound`, `gateway_ip_allowlist`, `console_audit` | 工具权限、写开关/确认、审计、SSRF、可信代理、敏感凭证不回显 | 最新全量复测通过；证明范围以各套件实际断言为限 |

补充核对规则：New API 的 `messages/count_tokens` 注册在该提交中已注释；`images/variations`、files、fine-tunes 指向 `RelayNotImplemented`。这些不能当作竞品已完成的能力。Sub2API README 的赞助广告也不属于功能证据。对 Grok 专属媒体操作、Antigravity、任务插件等扩展仍需继续判定核心场景和实际实现，未因当前已有测试较少而从目标删除。

## 验证环境与运行记录

本轮新建独立 PG16 / Redis7 / NATS2 / ClickHouse24.8 容器（标签 `okapi.task=core-api-20260926`），分别使用宿主机 35432 / 36379 / 34222 / 38123 端口，仅绑定回环地址。未启动、清空或迁移既有开发容器，未使用会删除开发数据的 `dev-deps.sh up` / `dev-reset.sh`。

- 四个服务均启动，PG、Redis、CH 的读探针通过。业务测试使用真实本地服务，供应商/邮件/支付上游使用各测试自己的模拟服务器。
- 全量基线命令：`cargo test --workspace --locked --offline --no-fail-fast -- --test-threads=1 --nocapture`。
- 显式设置四个服务 URL、`SQLX_OFFLINE=true`、`OKAPI_PG_POOL=4`、`OKAPI_SINGLE_USER_MODE=false`；不把 `.env` 中业务凭证写入报告。
- 输出：`/tmp/okapi-core-baseline.log`。首次运行编译失败：`console/admin.rs` 的模型发现查询新增 `channel_key_id` 后缺少离线缓存，未执行业务测试。已在隔离库应用全部 5 个迁移，完成 `cargo sqlx prepare --workspace -- --all-targets --locked --offline`，新增 `query-c53b953d…json`，其余原有缓存变更保留。
- 首次定向运行 `/tmp/okapi-core-targeted.log`：`gateway_responses` **9/9 通过**；路由探针 **1 通过、1 失败**。失败揭示了公开模型目录清单遗漏、WS 的 HEAD 被提取器拒绝，以及 HTTP/1 CONNECT 的 authority-form 不携带路由路径。原普通用户检查还有 34 项只返回 400，不能计为权限验证完成。
- 已修正测试契约并补充有效 JSON、必需查询参数及 multipart 结构。普通用户访问管理端点现在必须是 `403 permission_denied`，受保护端点的匿名请求必须是 401/403。探针仅在自己的 AppState 缓存关闭无效密钥限流，避免批量探测后半程都撞到 429；生产行为不变，限流另由 `gateway_invalid_key_rate` 实测。
- 定向复测 `/tmp/okapi-core-targeted-r2.log` 在编译期间修改了测试载荷并新增 compact 路由，旧编译无法验证当前源码，因此主动中止（退出 130，未运行测试）。随后启动原生完整运行 `/tmp/okapi-core-full-r3.log`：编译完成，78 项通过、0 项失败后，为验证 Clippy 修正后的最新源码切换至 Linux，主动中止（退出 130）。这次运行未完成，不能当作全量通过。
- CI 原先没有配置 NATS，`worker_nats` 会软跳过。现在独立启动带 JetStream 的 NATS，并配置 `OKAPI_NATS_URL`；已配置服务但连接失败改为硬失败。本地完整链路与 CI 实际运行结果仍须分别记录，不将工作流编辑当成 CI 已通过。
- 已用同一个本地缓存官方镜像独立验证新 CI 启动参数及 `/healthz` 命令，返回 `{"status":"ok"}`；验证容器随后移除，没有重启四个业务测试依赖。
- 编译停留时检查了原进程和采样：rustc 在 macOS 动态库加载校验处等待，之后继续产出新的 crate 编译记录。保持原运行，不因为观察超时重复启动。
- Linux 备用环境已准备：临时容器 `okapi-core-rust-20260926`，源码只读挂载，独立缓存与输出在忽略的 `target/core-linux-*` 下；CMake 3.31.6、Rust 1.98.1，锁文件离线解析 400 个 package 通过。原生编译完成并进入测试后，在独立 Linux 缓存执行 Clippy；原生测试现已终止，无并行业务测试。
- Linux Clippy `--workspace --all-targets --locked --offline -- -D warnings` 已通过（`/tmp/okapi-core-linux-clippy-r4.log`）。修正模型发现函数过长、compact 分派重复分支、权限探针的重复条件/载荷分支及测试中容易混淆的变量名；`cargo fmt --all --check` 通过。
- 最新 Linux 全量运行输出 `/tmp/okapi-core-linux-full.log`，四个隔离服务参数与原生运行相同；关闭增量和调试信息以控制临时产物大小，不改变测试断言。启动前对 870 个源码和配置文件记录 SHA-256（`/tmp/okapi-core-linux-source-manifest.json`）。首轮已结束（退出 101）：589 通过、16 失败、0 ignored，另有 1 项软跳过；不能算全量通过。
- 新增 `scripts/summarize-core-tests.py`，分别汇总套件、失败/忽略、软跳过信号和逐 API 权限探针；没有实际观察到进程退出时一律标记 incomplete。6 项 Python 回归通过，并用先前失败日志核对到 10 通过、1 失败和 34 项权限未证明的记录；该报告不声称接口业务覆盖或竞品功能已对齐。
- 完成后必须汇总逐套件通过/失败/忽略，并单独检索软跳过（例如 worker 未配置依赖时直接 return）。退出码 0 不能掩盖这些情况。

## 首轮全量失败整改与复测

- 首轮 Linux 日志 `/tmp/okapi-core-linux-full.log`，结构化结果 `/tmp/okapi-core-linux-report.json`。119 个套件/文档测试块，589 passed、16 failed、0 ignored。SPA 导航用例实际软跳过，已在报告中单列，不能计为业务已验证。NATS 链路 2 项、PG 结算 7 项、分析 11 项、统计 8 项通过。
- 14 项失败集中于空 `OKAPI_MASTER_KEY`：示例环境文件中的空值被当成已配置密钥，创建渠道返回 500，OAuth 凭证刷新无法持久化。Config 与 gateway 现共用空白值归一逻辑；未配置维持既有语义，非空错误值仍交加密层拒绝。新增单测覆盖空白、有效密钥继续加密、错误密钥不降级、密文不能无密钥读取。
- 2 项 compact 测试捕获 `stream=false` 仍被转发：通用字段剥除器刻意保护 stream，不能拿来处理 compact 协议。已用独立请求体处理移除该字段，其他字段原样保留，未放开普通渠道的字段保护。
- 新增规则删除业务测试，覆盖 401/403、删除回执、数据库结果、重复 404、发布前仍按旧折扣、发布后真实账单恢复原价及仅一次成功审计；删除回执补 `requires_publish=true`，与定价发布流程一致。
- 定向复测 `/tmp/okapi-core-linux-fixes.log` **115/115 通过，退出 0，无 ignored 或软跳过**；包括全部首轮失败套件、规则删除与 398 条权限/错误探针（141 条普通用户管理请求、257 条匿名入口请求）。探针中的公开入口与两项协议拒绝分别记录，不冒充受保护端点成功流程。
- 最新 Clippy 全目标 `-D warnings` 再次通过（`/tmp/okapi-core-linux-clippy-final.log`）；11 项 Python 守卫回归、格式与 diff 检查通过。
- SPA 软跳过来自 Cargo 测试工作目录与相对前端路径不同。最新全量显式设置 `OKAPI_WEB_DIR=/work/frontend/dist`；CI 前端 job 上传真实 dist，Rust job 下载后使用绝对路径，显式配置但缺失产物改为测试失败。CI 工作流尚未在 GitHub 执行，不能称 CI 已通过。
- 修复后的全工作区运行 `/tmp/okapi-core-linux-full-r2.log` 已结束（退出 101）：607 通过、1 失败、0 ignored、0 软跳过；873 个源码/配置指纹前后全部一致。首轮 16 项失败已通过，SPA 浏览器导航这次实际执行。唯一失败是 `console_stats::model_trend_folds_tail_into_other` 的共享排名样本污染，不能把这轮记为全量成功。
- 直接读取隔离 CH 验证失败原因：Top 3 为 `trend-stat-model-a` 6,000,000,000、另一个运维用例的 `m-ops-*` 5,001,000,000、`trend-stat-model-b` 4,000,000,000 micro-USD。接口排序符合金额，旧测试却要求固定 A/B 永远占前两名；固定模型名累加不足以隔离其他用例的数据。
- 趋势测试现创建唯一 CH 数据库，通过真实原始表与物化视图写入 5 条小额样本，不消费共享 outbox，测试结束（包括断言 panic）清理自己的数据库。覆盖空结果、按总额排序、同额稳定排序、两个小时桶、小时/天粒度、limit 下限及金额/请求数逐桶精确守恒。统计 8/8 通过后单独再跑该用例 1/1 通过；最新全目标 Clippy、11 项 Python 守卫、清单同步和格式检查通过。
- 最终复测 `/tmp/okapi-core-linux-full-r3.log` 已完成（退出 0）：**608/608 通过，0 failed、0 ignored、0 软跳过**；119 个套件/文档测试块，无未结束块或解析错误。结构化报告为 `/tmp/okapi-core-linux-r3-report.json`。873 文件指纹记录在 `/tmp/okapi-core-linux-r3-source-manifest.json`，运行前后完全一致；相对 r2 仅 `console_stats.rs` 改动。统计 8、分析 11、Responses 13、SPA 导航 3、NATS 2、PG 结算 7 项都实际执行。398 条权限/错误探针仍单列，不等同于 398 项完整业务验证。
- 全量结束后只更新文档与 CI 报告步骤，未再改 Rust 源码或测试。CI 与本地统一使用 `--test-threads=1`，避免不同用例修改全局配置互相干扰，用例内部的并发场景继续执行；保存完整输出和结构化报告，出现 ignored/软跳过或 incomplete 时拒绝把 job 记为通过。工作流 YAML 与 Bash 语法已检查，并以本轮成功日志、上一轮失败日志和软跳过样本回放同一段 CI shell，分别得到退出 0、101、1，确认失败码和跳过信号不会被 tee/报告生成掩盖；GitHub 托管 CI 尚未实际运行。

## 全路由验证的已知漏洞

原 `bins/okapi/tests/route_error_envelope.rs` 存在以下问题，不能证明“每个 API 正确”：

1. 用字符串窗口搜索方法，可能把相邻代码或注释中的 `get/post` 算进路由。
2. 用路径作唯一键，合并 console/gateway 上同路径的两个实际入口；例如两个 `/healthz`。
3. 未识别 `any(...)`，漏掉自定义透传；隐式 HEAD 的行为也尚未逐项验证。
4. 用 `path.contains("realtime")` 跳过所有命中项，连普通 HTTP 的实时统计也跳过。
5. 任意 2xx/3xx 直接被认为“按设计公开”，没有独立的公开端点清单。
6. 405 被跳过；普通用户返回 400/404 也算权限验证通过，即使只撞到了参数校验。

整改方向：生成角色+方法+路径的可追溯清单，未知语法明确报错；公开路由使用明确契约；401/403 与解析错误分别计数；WS、multipart、参数化接口用正确协议和有效请求体覆盖；业务套件结果与清单逐项关联。清单存在本身仍不算业务验证完成。

已落地并经后端定向运行验证（错误/权限契约，不等于业务全覆盖）：

- `scripts/api-surface.py` 生成 [API 清单](api-surface.json)，包含 Batch ZIP GET 后当前 286 个服务角色/方法/路径组合：186 个显式方法、91 个隐式 HEAD、9 个 `any` 方法。每项保留 handler、源码行及源文件 SHA-256。当前两个 router 文件的组装关系已人工核对，清单不是运行时可达性证明。
- 5 项 API 清单 Python 回归通过，覆盖注释/字符串伪路由、嵌套调用、角色同路径、`any`、HEAD、实时统计及未知语法拒绝。CI 检查清单与源码是否同步。
- Rust 门面探测改用同一个源码扫描器；不再普遍跳过 405，不再按 `realtime` 子串跳过 HTTP 统计；明确公开入口，禁止自动跟随重定向掩盖原始状态。`/v1/models` 和 `/v1beta/models` 的公开目录契约有现有业务测试和实现支撑。
- 每次探测输出 `API_PROBE` JSON 记录。普通用户必须先通过 `/api/me`；管理请求必须命中权限拒绝，400/404/422 会使测试失败，不能计入权限完成率。HEAD 单列状态检查。
- 两项协议特例单独断言并标记：Realtime 的隐式 HEAD 必须返回 405；Responses WS 在提取器错误前鉴权，匿名 HEAD 为 401；CONNECT 通过 HTTP/1 发出时不携带 `/pass` 路径，当前空 404 只记为传输层拒绝，**不算透传路由权限或业务覆盖**。`any` 清单枚举九种标准方法，不证明自定义扩展方法或 CONNECT 隧道能力。

## Responses 补齐的协议约束

核对官方文档后，compact 不能通过普通文本摘要或 Chat Completions 转换冒充：需要保留 compaction 项以及完整输出序列，并将返回的 usage 接入既有结算链。[官方 Compaction 指南](https://developers.openai.com/api/docs/guides/compaction)、[compact 接口契约](https://developers.openai.com/api/reference/typescript/resources/responses/methods/compact)。

Responses WS 也不能复用 Realtime 的 `response.done` 计费假设。其创建事件、续聊身份、终态、请求级错误需要单独处理；当前官方文档还定义了 `stream_id` 并行流和 `generate:false` 预热。因此补齐时需同时验证单连接内多轮、流归属和每次生成的结算，不把握手成功当作协议支持完成。[官方 WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode)。

compact 已通过模拟上游集成测试：沿用统一鉴权、预扣、候选治理、结算和账单维度；只选原生 Responses 渠道，尊重 `capabilities.compact=false`。API key 与 Codex OAuth 分别使用各自凭证与身份头，访问独立 JSON 端点。保留输入历史与不透明上下文，拒绝流式请求，不将 404 转成 Chat Completions。成功返回必须有压缩对象、密文项和可解析的输入/输出 usage；缺失时走失败链，不按保留的用户消息估算生成量。4 项集成测试覆盖三类上游、完整内容、缓存写倍率、账单维度/余额、匿名与输入边界、渠道能力筛选、上游 404 和损坏的成功响应退款。普通 `/responses` 的现代流式压缩请求保持原协议，未根据 body 信号改写路径。

### 输入 Token 计数（已实现，定向验证通过）

固定版本 Sub2API 的 `ResponsesInputTokens` 是独立预检链：先鉴权、验证模型与账户资格、选择账户，再计数；不会调用正常生成和消费记账链。[handler](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/handler/openai_gateway_count_tokens.go)。

其 service 对官方入口调用原生 `/responses/input_tokens`，对自定义中转或部分不支持状态改用本地估算，并返回同样的 `response.input_tokens` 对象；因此“有这个路由”并不意味着全部渠道都返回上游精确计数。[service](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/openai_gateway_count_tokens.go)。Okapi 补齐时要明确区分原生计数与可选估算来源，覆盖文本、工具结构、图片/文件及续聊字段，不能把不透明上下文的本地估算当作精确值。[官方计数指南](https://developers.openai.com/api/docs/guides/token-counting)。

新增 `POST /v1/responses/input_tokens`，契约见 [计数接口说明](responses-token-counting.md)。默认只调用原生计数端点，显式 auto/estimate 才允许本地估算。只有已知不支持原生计数才触发自动估算；限流、超时、认证错误和损坏响应不会被“估算成功”掩盖。本地估算拒绝图片、文件、历史/会话引用、保存的 prompt、加密推理及未知输入项。中转上游通过 JSON 或响应头声明的估算标记继续保留。

- `gateway_token_count` 10/10 通过，输出 `/tmp/okapi-count-tests-r4.log`、结构化报告 `/tmp/okapi-count-tests-r4-report.json`，退出 0，无 ignored 或软跳过。使用真实隔离 PG/Redis，供应商为 mock。
- 覆盖原生 OpenAI/兼容/Codex 请求体、凭证与身份头，模型别名/白名单/映射、IP 白名单、渠道池和零留存筛选；输入类型及 stream 校验；合法零值、缺字段、负数、小数、字符串、溢出、无效 estimated 类型、非 JSON 和超大响应。
- 上游 302 不跟随，400/401/429/500/404/405/501 逐项核对状态；错误正文不回显上游私密文本，不调用生成或 Chat 降级。原生能力筛选及 failover、关闭 fallback、显式本地估算边界均有断言。
- 在初始余额为零时成功计数，随后核对 Redis 余额/预扣、PG 余额/API key used_micro、账单/事件/outbox 全无资金副作用。独立 RPM/RPD、共享分组/模型/渠道 RPM、key/渠道并发、成功/失败/超时/handler 取消释放实际执行。
- 定向 r3 为 24 通过、1 失败：新夹具把 JSONB 的 IP 白名单写成 text[]，已修复；Responses 13 项与路由探针 2 项均通过。当前为 399 条权限/错误探针（141 普通用户、258 匿名），新计数入口匿名返回 401。探针不是成功业务全覆盖。
- 全目标 Clippy `-D warnings` 通过：`/tmp/okapi-count-clippy-final.log`。11 项 Python 守卫回归、路由清单同步、格式及金额/错误码守卫通过；新增接口后的完整回归结果见下文。

新增计数接口后的全量回归记录：

- 首轮 `/tmp/okapi-core-count-full.log` 已结束，退出 101；报告 `/tmp/okapi-core-count-full-report.json` 为 617 passed、1 failed、0 ignored、0 软跳过，399 条错误/权限探针。1,074 个源码、夹具、SQLx 缓存、配置和真实前端构建产物指纹前后完全一致（`/tmp/okapi-core-count-source-manifest.json`）。计数 10 项与 Responses 13 项在全量中也通过。
- 唯一失败为 `gateway_pricing_rules::volume_rule_fires_once_monthly_tokens_cross_threshold`。非流式响应的结算在后台执行，`settle_commit` 先 `settle_write` 写 PG，再 `record_settlement_counters` 更新 Redis；旧夹具把 PG committed 当成全部结算已结束，可能在月计数尚未更新时发起第二笔。规则累计本就定义为软实时，此测试不能假设 PG 记录可见等于所有计数可见。
- 夹具现在等待该 AppState 的完整后台结算并断言在途数为零；Token 轴在第二笔前明确验证 120，消费额轴逐笔验证 240→480→600，仍保留真实账单 240/240/120 与规则快照断言。没有直接预填计数或增加固定 sleep，没有改生产计费逻辑，也不宣称 HTTP 响应返回就是阶梯折扣立即生效的界限。
- 定向复测 `/tmp/okapi-count-volume-fix.log` 为 7/7 通过；全目标 Clippy `-D warnings` 与格式检查再次通过（`/tmp/okapi-count-volume-clippy.log`）。第二轮全工作区运行输出 `/tmp/okapi-core-count-full-r2.log`，开始前新指纹 `/tmp/okapi-core-count-r2-source-manifest.json` 相对首轮仅定价测试文件变化。
- 第二轮已结束，观察到实际退出码 0 后生成 `/tmp/okapi-core-count-full-r2-report.json`：**618 passed、0 failed、0 ignored、0 软跳过**，无未完成套件。399 条探针中普通用户权限拒绝 141 条，匿名认证拒绝 154 条、HEAD 状态 71 条、公开契约 21 条、错误壳 10 条、CONNECT 与 WS HEAD 协议特例各 1 条。运行后重新核对全部 1,074 个文件指纹，无变化；计数 10 项、Responses 13 项及阶梯规则 7 项均在本轮全量再次通过。上述记录来自本地隔离环境，未执行实际供应商联调或 GitHub 托管 CI。

### Responses 历史账号绑定（HTTP 已实现，回归通过）

此前源码只有 `stick:sess` L2 优先级亲和，失败仍可能改投其他账号，不能支持有状态历史硬绑定。现新增独立的 L1 读写与身份检查，契约见 [历史路由](responses-history-routing.md)：

- 原生 JSON 响应暴露 ID 前持久绑定；SSE 首字前先绑定上游 response 对象 ID，后续事件要求 ID 不变。记录按用户/API key 隔离，包含渠道、凭证及上游身份摘要，Lua 原子建立且不可覆盖到另一账号，固定 30 天 TTL。
- 生成、compact、计数共用历史查询；未知、过期、跨用户/跨 key 一致返回 404，非法值返回 400。续聊重新读取数据库候选，遵循当前可见性、能力和状态；不跨账号、模型或方言降级。实际续聊账单记录 sticky_layer=1。
- 凭证/入口/账号变更失效；OAuth 稳定 account_id 下正常 token 更新不改绑，刷新锁内若重读出另一个账号则中断当前请求。渠道 strip/inject 与 Codex body 整形都不能静默删除历史引用。原生上游是否保存/接受历史仍由上游决定，没有实际供应商联调证据。
- 首字前或 JSON 绑定写失败返回错误并释放预扣，不再调用另一候选；首字后绑定失败/ID 改变发送 Responses error 事件，保留真实 usage 后按既有流式链结算。Redis 读写各有 2 秒上限，不以存储故障为由随机选路。

验证记录：

- 初次新测试编译失败：夹具引用了不存在的 AppState Redis 字段及错误的 trait 名，未执行业务测试；已改为专用测试 Redis 客户端。`/tmp/okapi-affinity-tests.log` 保留失败输出。
- 第一批 `/tmp/okapi-affinity-tests-r2.log` 实际退出 0，7 项新测试 + 29 项原接口回归共 36 通过；报告 `/tmp/okapi-affinity-tests-r2-report.json`。
- 补充流中错误、模型降级/并发限制、断连存储后，`/tmp/okapi-affinity-tests-r3.log` 实际退出 0，**39 passed、0 failed、0 ignored、0 软跳过**；报告 `/tmp/okapi-affinity-tests-r3-report.json`。新套件 10 项使用真实隔离 PG/Redis 和两个模拟上游，从真实网关首轮返回的 ID 发起续聊；不通过预填 L1 代替端到端验证。既有透传用例显式配置已知历史，仅验证原有协议字段保留。
- 成功路由测试更换网关 AppState、提高另一渠道优先级，并为每次请求使用不同 session ID，排除仅靠 L2 恰好回原渠道造成的假阳性。备用模型先直接调用成功，保证“不降级”测试不是缺价导致的无效路径。流中故障保留实账 240 micro-USD；计数无扣费、首字前失败余额不变、预扣清空分别断言。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 全目标通过（`/tmp/okapi-affinity-clippy-final.log`）；格式、diff、11 项 Python 守卫、API 清单同步、金额与错误码守卫通过。Clippy 曾发现测试夹具过长与 format_collect，已拆分辅助函数并修正，没有放宽 lint。
- 完整工作区 `/tmp/okapi-affinity-full.log` 已结束，实际退出码 **0**；报告 `/tmp/okapi-affinity-full-report.json` 为 **628 passed、0 failed、0 ignored、0 软跳过**，121 个套件/文档测试块，没有未完成套件或解析异常。源文件、夹具、SQLx、配置及真实前端构建产物共 **1,076** 个 SHA-256 指纹前后完全一致（`/tmp/okapi-affinity-source-manifest.json`）。新增 10 项历史绑定、既有 Responses 13 项、计数 10 项和 OAuth 6 项均在全量中再次通过；39 项包含在 628 中，不叠加计数。
- 本轮仍为 **399** 条权限/错误探针：普通用户拒绝 141、匿名认证拒绝 154、HEAD 状态 71、公开契约 21、错误壳 10、CONNECT/WS HEAD 特例各 1。测试使用隔离 PG/Redis/NATS/ClickHouse 和模拟上游；GitHub 托管 CI、真实供应商历史留存/续聊及性能比较尚未执行。

本项未覆盖完整 Responses 生命周期：conversation 创建/归属，item/file 引用、检索/删除历史及 WebSocket 仍需分别核对。固定 TTL 也不是供应商持久化承诺；不能据此宣称所有有状态场景已对齐。

已修复且网关定向测试通过的计费问题：`responses::InputDetails` 原先只读取 `cached_tokens`，把返回的 `cache_write_tokens` 固定写为 0，导致缓存写入倍率不生效。现在保留该字段并复用输入各段的总量约束。新增 JSON/SSE 实账断言：输入 100（普通 40、缓存读 40、缓存写 20），输出 20，缓存写倍率 2 时，应记 280 micro-USD，而不是 240；同时核对账单倍率快照。缺失字段继续按零处理，超量字段由领域归一化限制在剩余输入内；首轮完整运行中 providers 的 63 项单元测试通过，已包含该缓存写解析边界测试。

### 异步图片与批量图片的基准核对（实现前记录）

这两项需要分别验收。固定版本 Sub2API 的单次异步图片复用同步 Images 请求体，先返回 202、Location 和 Retry-After，随后在进程内执行；任务记录保存在 Redis，结果依赖对象存储。关闭新建开关后，已有任务仍可查询；归属同时限定用户与 API key，跨 key 返回 404。默认执行期限 30 分钟、结果期限 24 小时。所查 handler/service 没有证明进程重启后能够恢复执行，不能据此把“重启恢复”算作该单次异步入口已验证的竞品优势。[异步 handler](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/handler/image_task_handler.go)、[任务 service](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/image_task.go)。

批量图片则有独立任务/条目状态、Gemini API 与 Vertex provider、幂等键与请求 hash、冻结额度和定价快照、队列、取消/下载/清理。重复幂等键且请求不同返回冲突；列表与条目采用游标分页，单条下载和 ZIP 下载分别检查归属与完成状态。Okapi 不能用并发执行若干同步图片请求冒充上游原生批处理支持。[批量入口 service](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/batch_image_public.go)、[批量 handler](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/handler/batch_image_handler.go)。

其恢复逻辑尤其值得独立测试：提交中的心跳与上游任务 ID 写入必须排除过期退款；原子转失败后即使审计写失败仍需归还冻结金额；退款失败必须可重试；结算按成功条目和提交时的价格快照执行，有稳定捕获 ID、manifest hash 和重试耗尽处理。Okapi 补齐时继续使用整数微美元，必须覆盖重复执行、部分成功、取消竞态、提交成功但本地写入失败、重启后恢复与余额最终守恒。[恢复逻辑](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/batch_image_billing_recovery.go)、[结算逻辑](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/batch_image_settlement.go)。这些是后续实现验收边界，当前不计为 Okapi 已具备。

## 原生 Responses WS 传输与 TLS

实现契约及网关接入矩阵见 [responses-websocket.md](responses-websocket.md)。下述传输/TLS 阶段没有新增 GET 路由，当时为 258 条静态清单；后续网关接入记录单列于下方，不把传输层当成公开 API 已完成。

- `responses_ws_transport` 的 16 项真实握手/帧测试覆盖同 stream 排队、跨 stream 并发与隔离、预热/续聊字段原样保留、受限队列/缓冲、代理/鉴权头、断开、超时、畸形帧和迟到终态。没有向真实供应商发起计费请求，也没有证明供应商缓存/留存行为。
- 验证发现请求级 `HTTP_11` 不能约束 TLS ALPN；现将普通转发、探针和 WS 的 client 分开，WS 固定 HTTP/1 且不跟随重定向。共用 `Arc<Clients>` 保持上下文克隆轻量，避免增加连接池后异步 future 体积超出 Clippy 限制。
- 新增 5 项真实 TLS 测试：生产构造器的直连/代理 ClientHello、加密帧与 usage、认证 CONNECT、证书与域名拒绝、普通转发和探针保留 h2。测试证书仅用于 localhost，测试 CA 不进入系统信任库。
- 首次全量 `/tmp/okapi-wss-full.log` 实际退出 **101**：**646 passed、4 failed、0 ignored、0 软跳过**，121 个套件/文档测试块。16 项 WS 帧测试及全部既有业务套件通过；4 项失败来自 TLS 测试服务器自动选择 CryptoProvider 时遇到 ring 与 aws-lc-rs 同时启用。测试服务器改用局部、显式的 aws-lc-rs 配置，不修改全局默认、不放宽 TLS 校验。
- 定向复测使用 `cargo test --workspace --lib http::tls_tests --locked --offline -- --test-threads=1 --nocapture`，5/5 通过（`/tmp/okapi-wss-tls-workspace.log`）；其它测试被过滤，不能当作全量通过。最终 Clippy `--workspace --all-targets --locked --offline -- -D warnings` 通过（`/tmp/okapi-wss-clippy-final.log`）。
- 首轮 1,097 个源码、配置、夹具及构建产物指纹前后一致；第二轮相对首轮仅 `http_tls_tests.rs` 变化，记录于 `/tmp/okapi-wss-source-manifest-r2.json`。第二轮运行前后 1,097 个指纹也完全一致。
- 第二轮全量 `/tmp/okapi-wss-full-r2.log` 已结束，实际退出 **101**；报告 `/tmp/okapi-wss-full-r2-report.json` 为 **650 passed、1 failed、0 ignored、0 软跳过**，122 个套件/文档测试块，无未完成块或解析异常。16 项 WS 帧测试、5 项 TLS 测试通过；399 条权限/错误探针口径不变。
- 唯一失败为 `surge_rule_reads_cluster_inflight_and_marks_up_the_bill`：按 `created_at,id` 取第四笔时读到了第三笔加价账单。隔离 PG 中第三笔 ID 2431 金额 360、时间 `00:47:58.807014`，第四笔 ID 2432 金额 240、时间 `00:47:58.780420`（2026-09-27 UTC）；第四笔实际收费正确，但时间戳排序与请求顺序不同。不能凭此断言生产加价未恢复，也不能把失败忽略。
- 测试现在从 HTTP 成功响应取得 `x-okapi-request-id`，按用户和请求 ID 精确等待对应账单，再等待完整后台结算；所有原有金额、规则快照、月计数断言保留，移除已废弃排序查询的 SQLx 缓存。不修改生产定价逻辑。定向日志 `/tmp/okapi-wss-pricing-id-tests.log` 实际退出 0，7/7 通过；严格全目标 Clippy 修正一处多余 raw-string 语法后通过（`/tmp/okapi-wss-pricing-id-clippy-r2.log`），格式与 diff 检查通过。
- 最终全量 `/tmp/okapi-wss-full-r3.log` 已结束，观察到实际退出码 **0** 后生成 `/tmp/okapi-wss-full-r3-report.json`：**651 passed、0 failed、0 ignored、0 软跳过**，122 个套件/文档测试块，没有未完成块或解析异常。定价 7、WS 帧 16、TLS 5、历史绑定 10、Responses HTTP 13、Token 计数 10 项均包含在 651 中，不叠加计数。
- 最终运行前后 1,096 文件指纹完全一致（`/tmp/okapi-wss-source-manifest-r3.json`），相对第二轮仅定价测试文件及其废弃 SQLx 缓存变化。399 条探针仍为普通用户权限拒绝 141、匿名认证拒绝 154、HEAD 状态 71、公开契约 21、错误壳 10、CONNECT/WS HEAD 特例各 1；不等同于 399 个完整业务 API。测试使用隔离 PG/Redis/NATS/ClickHouse 与模拟上游，未执行真实供应商联调或 GitHub 托管 CI。

源码核对同时确认了不能直接复用 HTTP 泵的三处边界：首输出超时可能重发已执行的 WS 轮次；无输出预热会被判为空回复；下游断开后丢弃消费者不会取消持久连接上的模型执行。后续公开入口已做独立事件消费、逐轮结算和归属验证，见下方网关接入记录。

### 高峰负载生命周期（已修复，完整回归通过）

本项与此前账单排序夹具问题分开：新测试实际复现了生产负载量表的生命周期缺陷。

- 旧 `GuardedBody` 只在正常 EOF 启动上报任务，计数守卫稍后才 Drop；任务可能先读到旧计数。客户端断开、响应体报错或 handler 取消时没有对应的 Redis 更新。旧量表又只在进入/结束时上报，持续长流超过 10 秒后会被读侧当成失联节点。
- `/tmp/okapi-surge-red.log` 实际退出 **101**（报告 `/tmp/okapi-surge-red-report.json`）：原有 surge 用例通过，三个新测试分别在响应体错误后未归零、客户端断开后未归零、长流无心跳处失败。没有清空量表、增加等待到陈旧窗口以后或改变预期金额来使测试通过。
- 新 `inflight::InFlightGauge` 每实例一个后台上报任务；没有在途请求时等待通知，活动期间每秒续报。watch 合并计数变更，守卫在正常 EOF、错误、Drop、取消各路径只释放一次；不存在每次结束另起一个上报任务的堆积。
- Redis 写入在实例内串行，取得写锁后才读最新计数；零与非零之间的转换不受节流限制，其余变更最多按一秒采样。节流用单调时钟，每次上报有 2 秒超时。它仍是软实时加价输入，并非严格的集群并发限流器；Redis 故障和进程退出未成功上报时，原有 10 秒失联窗口继续兜底。
- 五项新增测试覆盖：真实 HTTP 长流保持 11 秒后心跳仍新鲜、自然 EOF、客户端断开、响应体错误、响应头前取消，以及读完后仍持有响应体不会继续计数或重复递减。负载产生节点与计费节点不同；通过真实鉴权 chat API 与 request ID 关联 PG 账单，断言负载时 360 micro-USD、清零后 240，且命中/空规则快照一致。
- 取消和保留响应体的两项用例在 Axum service 层操控请求生命周期；另外三项使用真实回环 HTTP 连接。网关定价规则从 7 项增至 12 项，多节点 2 项和流中断 1 项同轮通过，共 **15/15**（`/tmp/okapi-surge-green.log`，实际退出 0；报告 `/tmp/okapi-surge-green-report.json`）。供应商为 mock，PG/Redis 为隔离真实服务。
- 严格全目标 Clippy 通过（`/tmp/okapi-surge-clippy.log`），格式、diff、11 项 Python 守卫、金额/错误码守卫通过；API 清单仅同步源码位置和指纹，仍是 258 条。
- 全工作区 `/tmp/okapi-surge-full.log` 已结束，观察到实际退出 **0** 后生成 `/tmp/okapi-surge-full-report.json`：**656 passed、0 failed、0 ignored、0 软跳过**，122 个套件/文档测试块，无未结束块或解析异常。上述 15 项包含在 656 中，不叠加计数；HTTP Responses、历史绑定、WS 传输/TLS、Token 计数、停机结算、NATS 与账本回归均再次通过。
- 运行前后 1,098 文件指纹完全一致（`/tmp/okapi-surge-source-manifest.json`），本轮额外纳入新增模块和生成的 API 清单。399 条权限/错误探针的分类及数量不变；它们仍不代表所有 API 的完整业务覆盖。测试使用隔离服务与模拟供应商，未执行实际供应商联调、GitHub 托管 CI 或竞品性能比较。

本轮没有修改定价倍率、价格规则或账本扣款公式，也没有新增公开 API；完整 Responses WS 与其它对标缺口继续按原目标推进。

## 完成条件

- 所有核心对标项都有固定版本的证据；真实能力差距落实为实现和验证，不能用一个同名路由或通用透传充数。
- 每个公开 API 有角色、方法、路径、成功场景、权限/归属、输入边界和副作用断言；无遗漏、无无声跳过。
- 所有资金变动端点覆盖并发和重复提交，网关覆盖流中失败/断开和重复结算。
- 本轮测试结果可追溯到工作树及依赖环境；必要的真实供应商联调和性能比较明确记录是否执行。
- “超越”的判断建立在可验证的能力与质量上：更完整的业务断言、计费/权限不变量、分析能力，以及同条件可靠性/性能证据。当前证据不足，**尚不能宣称全面对齐或超越**。


## 原生 Responses WS 网关接入

已注册 GET upgrade，与 POST 共存；支持 openai/Codex 原生上游。共享 HTTP 的逐轮鉴权、报价与预扣、候选资格过滤；WS 独立处理连接固定账号、流队列、终态、超时与结算。完整契约和保留边界见 [Responses WebSocket](responses-websocket.md)。

- 首批 `/tmp/okapi-ws-tests.log` 退出 101：9 通过、1 失败。失败为 Codex 测试夹具向 bytea 凭证列绑定 text，修正为字节后通过；未修改生产凭证存储格式。
- 后续定向 `/tmp/okapi-ws-targeted.log` 实际退出 **0**，报告 `/tmp/okapi-ws-targeted-report.json` 为 **50 passed、0 failed、0 ignored、0 软跳过**：新增网关 13、HTTP Responses 13、历史绑定 10、定价 12、路由探针 2。
- 新套件使用真实 PG/Redis 和 WebSocket 帧。实账验证预热输入 100/输出 0 收 200 micro-USD、生成输入 100/输出 20 收 240；并行三轮合计 720，各自 request ID、账单、余额和预扣释放一致。
- 覆盖握手鉴权/四连接限额/释放、租约存储错误拒绝、逐轮白名单与 key 撤销、未知及跨 key 历史拒绝、账号凭证变更和渠道停用、协议保护字段、原生 Codex 凭证/不透明输入、同流 FIFO/跨流并行、握手前回退/发送后不重放、下游断开后的终态结算、首事件超时后的延迟 usage、失败退款与已用 Token 记账、预热缺 usage 不伪造费用。
- 严格全目标 Clippy 实际退出 0（`/tmp/okapi-ws-clippy-final.log`）；格式、diff、11 项 Python 守卫、路由清单、金额与错误码守卫通过。最终全量前还修正了断开通知与 watch 分支之间的竞争，防止错过通知后等满 5 分钟，且保留首个错误用于账单状态。
- 当前生成清单 **260** 个角色/方法/路径组合；**401** 条探针：普通用户权限拒绝 141、匿名认证拒绝 155、HEAD 状态 72、公开契约 21、错误壳 10、CONNECT/Realtime HEAD 特例各 1。新 WS 完整成功业务证明来自 13 项专项测试，不能把这 401 条当作全部业务验收。
- 完整回归 `/tmp/okapi-ws-full.log` 已结束，观察到实际退出 **0** 后生成 `/tmp/okapi-ws-full-report.json`：**669 passed、0 failed、0 ignored、0 软跳过**。运行前后 1,104 个源码、夹具、SQLx、配置、脚本、前端构建产物和路由清单指纹一致（`/tmp/okapi-ws-source-manifest.json`）。本轮改动集中于后端，未修改前端源码或构建产物。
- 后续复核确认预扣 10 分钟后会被 worker 对账回收；原生 WS 单轮现增加 480 秒硬时限（可配置得更短，上限不提高），再留最多 30 秒取得终态 usage。只改 WS 事件消费与专项测试，未改变公共账本时限或后台对账策略。增补专项 `/tmp/okapi-ws-turn-cap.log` 实际退出 **0**，报告 `/tmp/okapi-ws-turn-cap-report.json` 为 **14 passed、0 failed、0 ignored、0 软跳过**。持续发送 in_progress 仍触发配置的硬时限，超时后终态用量入账 240 micro-USD，保留 504 状态，且无第二次上游调用。最终 Clippy 全目标 `-D warnings` 退出 0（`/tmp/okapi-ws-turn-cap-clippy.log`）；格式、diff、API 清单检查通过。最终源码清单 `/tmp/okapi-ws-turn-cap-manifest.json` 相对全量只有 WS 事件消费与其专项测试两文件变化；此修正不包含在上述 669 项全量结果里。

此时 HTTP 桥接仍待实现，随后进展见下一节。模型级降级、中途干预、完整会话/引用生命周期、真实供应商恢复/缓存/存储行为仍有缺口；没有相对竞品的性能优势证据。

## Responses WS → HTTP/SSE 桥接（2026-09-26，继续推进核心目标）

本轮仅改后端：补 native/http/auto 全局与渠道级协议选择、单次 SSE POST、连接内完整历史快照、本地零费用准备、同流失败淘汰和跨流隔离。契约、容量边界与本地准备区别见 [Responses WebSocket](responses-websocket.md)。

协议依据仍为官方 WS 文档；对照固定 Sub2API 版本的 [HTTP bridge](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/openai_ws_http_bridge.go) 与 [protocol resolver](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/openai_ws_protocol_resolver.go)。本轮 `gh` 不可用，回退到固定 SHA 的公开原始文件读取；没有运行竞品，也不把其重试策略直接当作本项目的安全边界。

- 首批 10 项桥接测试通过（`/tmp/okapi-bridge-tests.log` 实际退出 0）。
- 随后联合定向运行 `/tmp/okapi-bridge-targeted.log` 实际退出 101：50 passed、2 failed。两失败均为测试准备：空白名单仍表示拒绝，以及修改定价未发布 epoch。保留失败报告 `/tmp/okapi-bridge-targeted-report.json`，不计为通过。
- 修正准备步骤、补充错误默认流关联检查后，`/tmp/okapi-bridge-final.log` 实际退出 0；`/tmp/okapi-bridge-final-report.json`：15 passed、0 failed、0 ignored、0 软跳过。
- 最终完整工作区回归 `/tmp/okapi-bridge-full.log` 已观察到实际退出 **0**；报告 `/tmp/okapi-bridge-full-report.json`：**685 passed、0 failed、0 ignored、0 软跳过**，124 个套件记录全部结束，解析无错误。401 条权限/错误探针计数保持一致；不能把这些探针等同于 401 个成功业务场景。清单 `/tmp/okapi-bridge-source-manifest.json` 的 1,109 个文件在运行前后指纹一致，相对上轮只有 4 个后端源码变化、5 个新后端源码/测试文件；前端源码与构建产物没有继续改动。
- 最终全目标 Clippy `-D warnings` 实际退出 0（`/tmp/okapi-bridge-clippy-final.log`）；格式、diff、计费无浮点/禁止 panic 检查、错误码文案检查、API 清单检查及 11 项验证脚本测试通过。路由仍为 260 项 service/method/path；本轮没有通过增加错误路由壳来扩张能力计数。
- 实账断言：每轮输入 100 / 输出 20 收取 240 micro-USD；三个并行/排队请求共 720；已超时的迟到 usage 仍收 240 并保留 504；失败无 usage 退款；按次模型本地准备收 0，随后真正调用收 5000。请求级归属、余额、预扣释放及上游 POST 次数均有断言。
- 续聊覆盖工具调用/结果、encrypted reasoning、终态缺 output 时的 item 收集、矛盾 item 拒绝、新 instructions 不继承、外部存储父 ID 保留、跨 key 拒绝、本地准备跨连接拒绝；权限/传输能力变更、错误 HTTP/SSE、重定向不跟随、上下文超限亦覆盖。

这些是受控上游的协议/账本证据，不是供应商的真实计费、缓存语义或性能优势证据。完整核心目标仍未完成，继续保留模型级降级、中途干预、异步/批量图片、完整会话引用生命周期、passkey 对标和真实供应商联调等缺口。

## 图片协议与计费修正（2026-09-26，异步任务的前置整改）

核对异步图片实现时发现，同步链路原先只把计费 `n` 限制到 1–10，却可能仍向上游发送原始张数；multipart 非法张数甚至会按 1 张预扣。同步链路先统一验证和转发，避免异步队列继承这些问题。当前能力和剩余异步验收条件见 [图片接口契约](images-contract.md)。本轮没有新增 async/tasks/batches 路由，不能计为异步图片功能完成。

- JSON 生成、JSON 编辑、multipart 编辑共用鉴权和计费流程；补齐 JSON `images`/`mask` 引用与 multipart `image[]` 多图，保留文件字节、文件名、MIME 和请求选项。非法张数、已识别字段重复、空输入和不支持的流式请求在预扣前拒绝；未经归属登记的 `file_id` 不透传。
- 按请求张数预扣，按响应实际张数结算；3 张预扣 120,000 micro-USD，只返回 1 张则最终收 40,000、退回 80,000。损坏/空/超量成功响应返回 502 并退款。报价乘法检查溢出，结算保持请求开始时的价格版本，落账上游请求 ID 与实际 failover 次数。
- 图片 POST 仅在明确的 401/402/403/429 拒绝时换渠道；连接/超时/408/5xx 不自动重发。OpenAI/Azure 两类图片请求均不跟随 302/307/308，成功响应缓冲上限 64 MiB，错误正文上限 64 KiB，分块响应同样受限。其他协议的传输行为保持原有策略。
- 定向首轮 `/tmp/okapi-image-contract-tests.log` 编译失败（新测试函数引用错误），未执行业务测试。r2 实际退出 101，计费套件 3 通过、1 失败，原因为命令未指定隔离 ClickHouse 地址；补齐环境后 r3 实际退出 0，报告 `/tmp/okapi-image-contract-tests-r3-report.json` 为 22 passed、0 failed、0 ignored、0 软跳过。r3 发生在新增传输边界用例之前，完整证据以下一条为准。
- 最终完整工作区运行 `/tmp/okapi-images-full.log` 实际退出 **0**；报告 `/tmp/okapi-images-full-report.json` 为 **701 passed、0 failed、0 ignored、0 软跳过**，无未结束套件或解析错误。包含图片原有 3 项、新增契约 16 项、跨界面计费 4 项及现有协议/后台恢复测试；401 条权限/错误探针单列，不能当作成功业务覆盖。
- `/tmp/okapi-images-source-manifest.json` 记录的 1,111 个验证文件运行前后指纹一致；相对上轮仅改动 3 个后端源码、新增 1 个后端模块和 1 个集成测试文件。前端未继续修改。最后仅同步文档，未再次变更源码或测试。
- 严格全目标 Clippy `-D warnings` 实际退出 0（`/tmp/okapi-image-contract-clippy-final.log`）；格式、diff、金额无浮点/禁止 panic、错误码和 API 清单检查通过。路由清单仍为 260 项。

PG/Redis/ClickHouse/NATS 使用已有隔离测试服务，供应商为受控模拟上游；没有实际供应商联调或竞品性能结论。图片流式输出、图片 Token 定价、文件归属生命周期、持久异步执行与结果存储、原生批量任务仍未完成，核心目标保持进行中。

## 持久图片异步队列与恢复结算（2026-09-26）

新增生成/编辑 async、任务查询/取消/私有下载，共 8 个显式路由，加 GET 的隐式 HEAD 后路由清单为 271 项（console 218、gateway 53）。数量只表示静态接口清单，不能等同成功业务覆盖。默认不接受新建异步任务，需迁移 0006、运行 worker 并打开 `image_tasks_enabled`；部署和契约见 [图片接口说明](images-contract.md)。本阶段没有修改前端功能。

- PostgreSQL 保存请求、租约、执行尝试及结果；用户与 key 共同隔离；幂等键重复复用或冲突；排队不预扣，执行前重新核验权限和价格。
- 处理前失联可重领，已标记发送后失联不自动重发。每次领取使用独立预扣 ID，状态/结果写入受租约限制。排队取消不收费；执行中取消不伪装成供应商已停止，已得到的成功结果仍正确结算。
- PG 结果/图片、账单、钱包快照、API key 用量和 outbox 同事务提交；Redis 依靠持久待结算标记恢复。预扣清扫与结果提交共用任务行锁，事务内复用数据库连接，避免小连接池中再次取连接导致等待。
- 输入与结果各有字节预算、任务数量上限和 24 小时保留期；失败/取消元数据也计容量；完成后的 base64 图片通过原 key 私有下载，下载许可持有到响应结束，最多 4 个在途响应。
- 新增异步集成 18 项，加原图片契约 16 项共 34 项；结合 worker 7 项和对账修复 3 项，`/tmp/okapi-image-tasks-edges-r4.log` 实际退出 0，专项报告为 **44 passed、0 failed、0 ignored、0 软跳过**。使用真实隔离 PG/Redis 和受控 HTTP 上游，覆盖新状态实例执行持久请求、并发领取、幂等冲突、重领/旧租约、已发送后中断、不重复扣费、跨 key 归属、权限撤销、队列与执行中取消、JSON/multipart 保真、下载容量、元数据数量预算和 worker 停机排空。
- 事务故障用例通过有效上游结果超出持久元数据预算，验证已写入事务的账单/余额/API key/事件全回滚并释放预扣；另一用例模拟 PG 已提交而 Redis 未结算，验证预扣清扫锁、待结算标记保留、恢复实际张数收费，以及恢复后 TTL 删除图片和领取记录。它们不构成真实供应商费用或远端图片可用期的证明。
- `/tmp/okapi-image-tasks-clippy-r2.log` 的 `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 实际退出 0；格式、金额路径、错误码及 API 清单守卫通过，Python 清单/报告工具的 11 项测试通过，单独计数。

全量验证保留环境故障记录：原宿主机 Cargo/target 缓存目录消失后，用公开包缓存离线恢复；首次全量 `/tmp/okapi-image-tasks-full.log` 在链接时因容器磁盘满退出 101，未进入测试。构建缓存随后复制到宿主机 `target/core-linux-target` 并挂载；第二次 `/tmp/okapi-image-tasks-full-r2.log` 因隔离 PG 在此前满盘时已退出而发生连接池超时，观察到 71 passed、16 failed 后终止该测试进程组，实际退出 143。这两次均记为失败，不能拼接为通过。隔离 PG 保留原数据启动并完成 WAL 恢复，迁移 1–6 和任务表可读取；后续完整结果单独记录。

最终全量 `/tmp/okapi-image-tasks-full-r3.log` 实际退出 **0**，`/tmp/okapi-image-tasks-full-report.json` 为 **719 passed、0 failed、0 ignored、0 软跳过**，没有未结束套件或报告解析错误。412 条权限/错误探针单列，不能当作 412 个成功业务场景。`/tmp/okapi-image-tasks-source-manifest.json` 与 `/tmp/okapi-image-tasks-source-verification.json` 核对 1,117 个源码、迁移、依赖配置、SQLx 缓存、fixture、脚本、构建前端和 API 清单文件，运行前后无变化；排除 `.DS_Store`、`__pycache__` 等非源码文件。

本阶段结束时结果存储为有界 PG BYTEA，外部 URL 未抓取转存；随后 URL/S3 的推进见下一节。供应商原生 Batch、流式图片、Token 计价、文件归属生命周期及其他核心协议/身份缺口继续保留，核心目标未完成。

## 图片 URL 转存与私有 S3（2026-09-26，后端）

固定版本 Sub2API 的 [图片存储服务](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/image_storage.go) 支持 base64、data URL 和远端 URL 转存，[S3 存储实现](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/repository/image_storage_s3.go) 返回公开地址或预签名地址。本轮 Okapi 增加可选 URL 复制与 S3 转存，返回经过原 key 鉴权的私有下载地址；这不是公开分享链接的同等契约，也不是对竞品整体可靠性的比较。

- 新迁移 `0007_image_objects.sql` 保存上传/清理意图、版本、内容摘要、工作租约和重试状态。意图先于远端 PUT，确认后才释放 PG 图片；上传失败不影响已保存结果的下载和原账单。任务过期、幂等键释放均等待对象清理，不因数据库级联删除丢失远端清理依据。
- 每个来源 URL/重定向跳转校验协议和 DNS 地址并固定解析，不携带身份头；默认只访问公网 HTTPS，可信私网 origin 需单独配置。结果预算按声明长度及实际分块累计检查，未知内容拒绝；失败退款且不重复生成。
- S3 使用 SigV4、不可变键和条件 PUT；重试遇到已有对象必须核对内容。下载核验归属、长度和 SHA-256；带版本对象按版本删除，确认丢失时 HEAD 查版本。存储位置摘要避免旧 ID 被配置成另一桶后静默改读；凭证轮换不改变位置标识。详细部署、权限、旧配置保留及生命周期兜底要求见 [图片契约](images-contract.md)。
- `/tmp/okapi-image-storage-contract.log` 实际退出 **0**：图片契约 **42 passed、0 failed、0 ignored**，其中新增存储用例 8 项。覆盖 URL/data URL 转存、私网/DNS/跨来源重定向拒绝、容量/内容拒绝、上传确认丢失、worker 中断及并发领取恢复、版本删除及删除失败重试、内容损坏、旧配置停用/地址变更和跨 key 拒绝。每项检查上游调用或存储副作用，相关用例核对余额与唯一账单。
- `/tmp/okapi-image-storage-provider.log` 实际退出 **0**：provider 单元测试 **70 passed、0 failed、0 ignored**。新增 [AWS 官方 S3 GET/特殊字符 PUT 签名向量](https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sig-v4-header-based-auth.html) 和 IP 地址边界测试。它们验证计算结果和地址策略，不等于真实云端协议联调。
- 首次严格 Clippy `/tmp/okapi-image-storage-clippy.log` 退出 101（固定静态错误类型按值传递 lint）；为该类型派生 `Copy/Clone` 后，`/tmp/okapi-image-storage-clippy-r2.log` 的完整工作区/all-targets/`-D warnings` 实际退出 **0**。未放宽 lint。11 项 Python 守卫、API 清单同步、格式、diff、金额与错误码检查通过。
- 独立 MinIO 服务尚未成功启动：官方 Quay 固定标签拉取返回 401，Docker Hub 同标签返回 pull access denied，官方二进制下载返回 410。输出保留在 `/tmp/okapi-image-storage-minio-pull.log`、`/tmp/okapi-image-storage-minio-hub-pull.log`；不能把这些尝试记为联调通过。新增 `verify_image_store` example 已通过全目标编译检查，需要显式测试桶配置才能运行，不软跳过，也不计入默认测试数量。真实 AWS/S3、虚拟主机寻址及真实 STS 联调仍未验证。

本轮未修改前端源码或构建产物，未执行 GitHub 托管 CI、真实供应商生成或竞品性能比较。S3 控制台管理、公开分享/预签名下载和跨存储迁移没有在本轮实现。完整核心对标目标继续，不能据此宣称全面超越。

本轮全量和最终边界修复分别保留证据：

- `/tmp/okapi-image-storage-full.log` 观察到实际退出 **0**；报告 `/tmp/okapi-image-storage-full-report.json`：**729 passed、0 failed、0 ignored、0 软跳过**，125 个套件/文档测试块完整结束，报告无解析错误。412 条权限/错误探针单列；不是 412 个成功业务场景。运行前后 `/tmp/okapi-image-storage-source-manifest.json` 的 1,125 文件指纹一致，核对记录 `/tmp/okapi-image-storage-source-verification.json`。
- 全量期间源码复核发现一个未覆盖的边界：上游同时返回非空 URL 与空 `b64_json`，外层形状检查通过，异步处理却优先保存空字节；开启 S3 后，历史空记录反复触发对象大小约束错误，使后续任务无法推进。先添加两个用例，`/tmp/okapi-image-storage-empty-red.log` 实际退出 **101**：0 passed、2 failed，分别复现错误 completed 终态和 worker 500。红灯报告单独保存，不算通过；本次失败夹具新建的两个临时数据库已核对前后清单后清理，主测试数据库未重置。
- 修复只涉及两个后端文件：异步结果解码后拒绝空内容；转存选择跳过历史空字节并让其按原 TTL 清理，不改已有账单。最终 `/tmp/okapi-image-storage-final-contract.log` 实际退出 **0**，对应报告为 **44 passed、0 failed、0 ignored、0 软跳过**，含新增两个回归：新空内容无账单/预扣副作用，旧空记录不会阻塞正常任务/上传/下载，且仍能到期清理。
- 最终 `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 实际退出 **0**（`/tmp/okapi-image-storage-final-clippy.log`）；API 清单、金额守卫、格式、diff 检查再通过。`/tmp/okapi-image-storage-final-source-manifest.json` 相对全量仅有 `tasks.rs`、`objects/db.rs` 和 `support/image_storage_cases.rs` 三文件变化，指纹复核见 `/tmp/okapi-image-storage-final-source-verification.json`。没有把最终两项用例加到 729 上声称新的全量通过；最后仅同步文档。

## 原生批处理协议与重复预扣修复

Gemini/Vertex/GCS 新协议层、固定版本依据、20 项协议测试以及最终适配层 **135 passed、0 failed、0 ignored、0 软跳过**的范围见 [原生批处理边界](native-image-batches.md)。全目标严格 Clippy 已通过；未注册公开 Batch 路由，没有将模拟协议测试当成真实供应商或计费联调。

在检查长期冻结接入前提时，发现现有同步 `reserve.lua` 没有检查相同 request_id 的活跃预扣。再次预扣会继续扣余额并覆盖唯一 `r:<request_id>` 记录；订阅第一次越界后重放甚至会改扣钱包。该问题影响公共账本，已单独修复：

- 先加入 5 项真实隔离 Redis 回归。首个日志 `/tmp/okapi-reservation-replay-red.log` 因测试夹具使用未启用的 Redis KEYS 接口编译失败，不能计为业务复现。改为读取用例确切的 key 后，`/tmp/okapi-reservation-replay-red-r2.log` 实际退出 **101**：**0 passed、5 failed**，成功复现重复扣款、记录覆盖、并发重复准入及订阅改扣钱包。
- Lua 在限流/选池/任何写入前以 `HEXISTS` 检查原记录；重复返回 `RESERVATION_EXISTS`，Rust 返回专用 `LedgerError::ReservationExists`。不刷新截止时间，不改余额、原 key、池或限流/并发计数，也不删除旧格式/异常记录。该错误不会被视为新的上游调用许可。
- `/tmp/okapi-reservation-replay-green.log` 实际退出 **0**；报告为 **63 passed、0 failed、0 ignored、0 软跳过**，包含 `okapi-domain`、`okapi-pricing`、`okapi-ledger` 的全套测试、New API 定价 fixtures 和性质测试。新增 5 项断言相同/变更参数重放、跨分钟及过期未回收重放、32 路并发只准入一次、订阅越界不转钱包、零金额和旧/异常格式保护。精确核对金额、预扣记录及四种计数器，拒绝必须是专用错误，不能以任意 Redis 故障充数。
- `/tmp/okapi-reservation-replay-clippy.log` 的完整工作区/all-targets/`-D warnings` 实际退出 **0**。修复只增加活跃记录防重，不改价格公式、订阅选池政策或十分钟截止时间；数据库 [Lua 契约](database.md#22-余额热账本与-lua-契约) 已同步。

保护范围是尚有预扣记录的请求；终态后仍由调用方保证 request_id 全链路唯一。这不是跨重启永久幂等，也不是长期资金冻结。原生 Batch 的公开 API、持久任务编排、长时冻结、逐项校验与结算、清理和真实供应商联调继续保留为未完成项。

本轮最后完成一次全工作区回归（`/tmp/okapi-native-batch-ledger-full.log`），观察到实际退出 **0**。核对日志时发现 Cargo 的 stderr `Running unittests …` 插入 stdout 的 `test result … filtered out` 后、分号前，旧汇总脚本将两个套件合并，少计 API crate 的 1 项，初报为 755。原始日志不改动，初报另存为 `/tmp/okapi-native-batch-ledger-full-report-initial.json`。

- 汇总器现在识别这一特定拼接，并对每套件的测试终态数量与结果汇总做一致性检查；重复结果、缺失终态和计数不符不能得到通过结论。新增 3 项 Python 测试先复现失败；一次兼容性修正排除 Python 3.9 Counter 的显式零项差异后，14 项脚本测试全部通过（`/tmp/okapi-summary-interleaved-green-r2.log`）。旧 729 项完整日志、135 项适配层和63项账本/定价日志复核结果不变，红灯日志仍为失败。
- 修正后的 `/tmp/okapi-native-batch-ledger-full-report.json`：**756 passed、0 failed、0 ignored、0 软跳过**，127 个完整套件，无解析错误。756 是该次完整日志逐套件核对后的结果，未靠相加不同运行得出；新增 27 个测试名称、没有删掉任何旧用例。412 条权限/错误探针单列，不当作 412 个成功业务测试。
- `/tmp/okapi-native-batch-ledger-full-manifest.json` 与 `/tmp/okapi-native-batch-ledger-full-verification.json` 证明运行前后 1,136 文件指纹一致。随后仅改汇总器及其测试两个脚本，最终清单 `/tmp/okapi-native-batch-final-manifest.json` 单独记录差异；没有为统计修正重跑未变化的 Rust 业务套件。金额、错误码、格式、API 清单及 diff 检查通过。

## 长期冻结与后台对账（2026-09-27，后端）

本轮只改后端账本、订阅协作、worker、迁移、测试和文档。新增 `balance_holds` 的 PG 意图/确认/结算状态以及 Redis 独立 h:* 与永久凭证；不进入同步请求的十分钟预扣回收。固定用户/key/模型/摘要/价格，PG 账单和 outbox 提交后才释放未用资金；并发重放、失败退款、跨订阅窗口与 Redis 冷恢复都通过实际隔离 PG/Redis 验证。未确认准入的取消先落 PG 标记，再 seal 阻止迟到扣款；若此前已冻结则按原凭证退款。详见 [长期冻结契约](durable-balance-holds.md)。

- 首批 `/tmp/okapi-durable-holds-r1.log` 实际退出 0，9 项通过。r2 的测试 DDL 未按 SQLx 0.9 动态 SQL 类型要求声明已审核，编译退出 101，未执行业务测试；仅对测试生成的数字 ID/UUID DDL 加明确审核包装后，r3 实际退出 0、16 项通过，r4 的确认前取消增补达到 19 项通过。最终容量、旧订阅窗口及凭证矛盾测试包含在下述定向/全量结果中，不叠加计数。
- `/tmp/okapi-durable-holds-money.log` 实际退出 **0**；domain/pricing/ledger 完整测试为 **86 passed、0 failed、0 ignored、0 软跳过**，含长期冻结 23 项、parity 5 项和性质测试 6 项。故障注入覆盖冻结确认失败、关闭确认失败和 outbox 写入失败；测试不是实际断电、真实云或供应商收费联调。
- 严格 Clippy 首轮指出布尔表达式和重复分支，第二轮指出测试导入位置、单变体通配及分号样式；修正后 `/tmp/okapi-durable-holds-clippy-r3.log` 的全工作区/all-targets/`-D warnings` 实际退出 **0**，没有降低 lint。
- 全工作区 `/tmp/okapi-durable-holds-full.log` 实际退出 **0**，完成后生成 `/tmp/okapi-durable-holds-full-report.json`：**780 passed、0 failed、0 ignored、0 软跳过**，128 个完整套件/文档测试块，无解析错误或未结束块。新增 23 项冻结测试及 1 项 worker 清扫/冷恢复测试均在这次完整运行中；412 条权限/错误探针仍单列，不当作成功业务测试。
- `/tmp/okapi-durable-holds-source-manifest.json` 与 `...-source-verification.json` 核对 1,150 文件运行前后一致；相对前轮新增 14 个源码/测试/迁移文件，没有改变 Cargo 依赖或公开路由。API 清单检查仍一致，14 项 Python 守卫、计费无浮点/禁止 panic、错误码、格式和 diff 检查通过。后续收尾边界修正单列如下。
- 全量后复核发现两个 Lua 只检查计算后余额：初始 Redis 整数若已超出 `2^53−1`，先转换再加减可能舍入回允许区间，而 HINCRBY 仍处理原整数。先新增两个用例，`/tmp/okapi-durable-holds-boundary-red.log` 实际退出 **101**，**0 passed、2 failed**，分别复现异常钱包余额仍获准冻结、异常负订阅余额被退款掩盖；测试恢复了各自注入的异常值。修复为先验证原余额，再计算和校验结果。
- 最终 `/tmp/okapi-durable-holds-final-money.log` 实际退出 **0**，完整 domain/pricing/ledger（含 parity）**88 passed、0 failed、0 ignored、0 软跳过**，长期冻结 25 项。`/tmp/okapi-durable-holds-final-clippy.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**，金额守卫、格式与 diff 再通过。最终 `/tmp/okapi-durable-holds-final-manifest.json` 相对全量清单只有 `hold_reserve.lua`、`hold_close.lua`、`tests/support/holds_recovery.rs` 三个文件变化；没有把最终两项用例加到 780 上声称新的全量通过，之后仅更新文档。

这完成了长任务资金基础，尚未实现公开原生 Batch API。后续须绑定持久批任务/条目、预算和准入限制、供应商提交意图/租约、取消确认、逐项输出校验与一次性收费、私有结果清理；订阅注资中断恢复和真实基础设施/云服务故障也仍待业务级验证。不能据此宣称所有核心功能已对齐或性能超越竞品，原目标保持进行中。

本轮未改前端源码/构建产物、未重置开发数据库；未执行 GitHub 托管 CI、真实云供应商计费联调或竞品性能比较。核心目标保持进行中。

## 原生批任务持久状态与加价修复（2026-09-27，后端）

本轮新增 `0010_image_batches.sql` 与 `okapi_store::image_batches`，将批任务元数据、私有输入/账号绑定、条目、预分配输出槽位分开保存；实现原子容量准入、并发幂等、用户/key 隔离、服务端游标分页、租约与提交意图、迟到远端确认、私有结果暂存及 closed hold 后才能发布的结算门槛。`reserve_frozen` 从服务自身持久价格快照恢复冻结，不重新报价。详细边界见 [批任务存储契约](native-image-batch-jobs.md)。

发现并通过真实 PG/Redis 复现两项问题：

- 加价模型合法的负 discount 被新增长期冻结结算误拒。`/tmp/okapi-native-batch-surcharge-red.log` 实际退出 101、0 passed/1 failed；移除错误的负值拒绝，仍核验原额减优惠等于实扣和冻结上限。随后 `/tmp/okapi-native-batch-surcharge-money.log` 实际退出 0，domain/pricing/ledger 89 项通过。
- 租约在等待行锁期间过期时，只写在 WHERE 中的期限条件仍可能放行旧执行器。`/tmp/okapi-native-batch-lease-red.log` 实际退出 101、0 passed/1 failed；改为持锁后再次读数据库时钟。`/tmp/okapi-native-batch-jobs-money.log` 实际退出 0，domain/pricing/ledger 101 项通过，包括 12 项批任务集成/金额测试；未将这些定向计数叠加为全量结果。

新增用例核对 16 路创建只产生一份任务/输入/槽位、24 路领取只有一个执行权、失联提交进入 uncertain 而不能重发、迟到确认不替换远端身份、23 条列表分为 20+3、跨用户/key/游标/父任务访问拒绝、容量和参数拒绝不留半份任务、变更当前报价后仍用保存价格、部分成功按两张结算 1,000 micro-USD、取消不能提前退款、closed hold 金额或单位数不符时不能开放图片。私有内容检查覆盖 staging、封存后未结算、跨归属和保留期届满。

错误码守卫发现新增内部状态冲突被直接归到控制台 `StoreError::Conflict`，会无条件透出新 HTTP 文案码；已改为独立类型化执行错误，后续 API 必须显式映射到本地化错误壳。守卫和前端文件均未修改；守卫最终通过。此调整之后的最终全工作区结果单独记录。

**上述 795 项运行时尚未注册原生 Batch 路由，亦未运行供应商执行器。** 后续接入见下一节。真实 Gemini/Vertex/GCS 费用、IAM 和故障恢复没有由这些存储测试证明；不据此宣称核心功能已经全部对齐。

## 原生 Batch 公开接口与执行器（本轮）

新增八个显式方法/路径组合及隐式 HEAD，均先验证身份；列表和条目默认 20 条、最大 100 条。资金意图与任务创建同事务，key 预算包含未关闭冻结；后台处理冻结、固定文件上传、单次提交意图、轮询/取消、逐槽解析、按成功图片结算和发布。发送前检查最新 key/模型/原渠道权限；持久价格、账号与账单字段跨重启不漂移。原生创建结果不明确时保留冻结并进入 uncertain，不重发供应商 POST。

首轮 `/tmp/okapi-native-batch-endpoints-first.log` 实际退出 **0**，`gateway_native_image_batches` **6 passed**。使用每个用例独立创建并清理的 PostgreSQL 数据库，共享测试 Redis 使用随机 ID 避免碰撞；真实 HTTP 网关和受控 Gemini 原生协议上游。断言覆盖：

- 三个槽位只有一个成功时，冻结最大金额而只收一张费用；结算前 404，结算后私有下载；新 worker 与后续改价不改变旧账单。
- pending/held 阶段取消或撤销 key，零费用收尾且没有供应商 IO；最终源码另补模型停用分支。
- 重复结果保留私有暂存、没有账单，修正结果后复用同一任务、只结算一次。
- 上游创建 502 后不再次提交且不擅自退冻结；明确 400 则零费用关闭，错误正文不回显。
- 同用户其他 key 不能读取/取消/删除/下载；幂等冲突、非法输入、独立 key 预算和分页上限；无凭证先拒绝而非先解析正文。
- 21 条任务按 20+1 分页；远端取消收到确认后继续核对终态再结算。

后续补充展开输入的逐槽有界序列化测试、上传会话恢复身份/长度/来源验证，以及发送前模型状态复核。`/tmp/okapi-native-batch-endpoints-clippy-pass.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**；14 项 Python 工具测试、格式/diff/金额/错误码/清单守卫通过。

最终 `cargo test --workspace --no-fail-fast --locked --offline -- --test-threads=1 --nocapture` 实际退出 **0**。`/tmp/okapi-native-batch-endpoints-full.log` 与 `...-full-report.json` 记录 **802 passed、0 failed、0 ignored、0 软跳过**；425 条权限/错误探针独立计数。`...-source-manifest.json` 与 `...-source-verification.json` 核对 **1,169 个文件运行前后无变化**；相对上轮基线有 15 个已有文件变化、11 个新增文件，全部为后端源码/测试或生成的 API 清单，文档单独同步。未修改前端源码、构建产物、Cargo 依赖、已应用迁移或 SQLx 缓存。

本轮没有前端源码或构建产物改动。批任务开关默认关闭；实际 Gemini/Vertex/GCS 费用、IAM、云端故障、自动找回、远端/本地物理清理、ZIP、筛选和完整准入仍是未完成事项，详见 [原生批处理边界](native-image-batches.md)。

最终全工作区结果（与前述定向运行分别记录）：

- `/tmp/okapi-native-batch-jobs-full.log` 观察到实际退出 **0**；报告 `/tmp/okapi-native-batch-jobs-full-report.json` 为 **795 passed、0 failed、0 ignored、0 软跳过**，128 个完整套件/文档测试块，无未结束项和解析错误。412 条权限/错误探针单列，不能当作 412 个成功业务场景。
- `/tmp/okapi-native-batch-jobs-source-manifest.json` 与 `...-source-verification.json` 核对 **1,158 文件无变化**。相对前轮最终清单仅 13 个后端源码/测试/迁移文件变化，其中 8 个新增；前端构建产物、依赖锁和公开路由未变化，本轮亦未修改前端源码。
- `/tmp/okapi-native-batch-jobs-clippy-final.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**。格式、diff、计费无浮点/禁止 panic、错误码及 API 清单检查通过；14 项 Python 工具测试通过，独立计数。

本轮最终只同步文档，未再修改已验证后端源码。迁移仅在隔离测试服务应用，没有重置开发数据库，也没有进行真实云计费或竞品性能验收。核心对标目标保持进行中。

## 原生 Batch 未知提交找回（2026-09-27）

新增 `0011_image_batch_recovery.sql` 和恢复执行路径。上游接收创建但回包丢失时，仍只发送一次创建；固定账号逐页查询带原 UUID/提交意图的标签。查完所有分页且只有一个候选后，新鲜 GET 核对模型、输入文件和远端身份，再以当前有效租约接管。分页检查点保存在 PG，重启继续下一页；多候选或身份矛盾形成持久冲突，找不到和 404 继续保留冻结。最多扫描 1,024 页，不把扫描不完整当作未执行。

官方 SDK 核对还发现 Gemini 成功结果可能在 metadata.output，旧解析遗漏该位置。`/tmp/okapi-batch-recovery-metadata-red.log` 实际退出 101、0 passed/1 failed，确实复现；协议修复后的首轮为 21 passed。取消任务在 metadata 保留的部分输出也逐项核对并按实际成功数收费，Operation.error/response 矛盾仍拒绝。

- `/tmp/okapi-batch-recovery-targeted.log` 首次运行实际退出 101、12 passed/1 failed；失败来自新增测试夹具误写 channels.base_url，尚未执行其他目标。改用真实 api_base 字段，没有放宽业务断言。
- `/tmp/okapi-batch-recovery-targeted-r2.log` 实际退出 **0**，**81 passed、0 failed、0 ignored、0 软跳过**；对应 report 分开记录 HTTP 13、账本/存储 42、协议 26。新增恢复、取消、实际收费、租约失效和分页竞争均在这次运行中通过。
- 严格 Clippy 要求拆分过长恢复函数及统一表达式格式，修复后 `/tmp/okapi-batch-recovery-clippy-pass.log` 的全工作区/all-targets/`-D warnings` 实际退出 **0**，没有禁用 lint。最终全量回归在该版本上单独执行。

本轮未修改前端或新增公开路由。真实 Gemini/Vertex/GCS 的身份回显、IAM、费用和恢复行为尚未联调；未知创建并非所有情况都能自动消歧。物理清理、ZIP、筛选、完整准入和其他核心对标缺口继续保留，批任务入口默认关闭，核心目标保持进行中。

最终全量 `/tmp/okapi-batch-recovery-full.log` 已观察到实际退出 **0**，报告 `/tmp/okapi-batch-recovery-full-report.json` 为 **819 passed、0 failed、0 ignored、0 软跳过**，129 个完整套件，无未结束套件或解析错误。这是最终函数拆分及格式修正后的完整运行，不是将 81 项定向结果加到历史计数。

425 条 API 探针另计：141 条受限身份权限拒绝、171 条匿名鉴权拒绝、80 条 HEAD 状态、21 条公开契约、10 条错误壳、1 条 CONNECT、1 条 WebSocket 方法检查。它们只证明实际断言的边界，不代表逐接口全部业务覆盖。格式、diff、金额守卫、错误码守卫、API 清单及 14 项 Python 工具测试均通过。

`/tmp/okapi-batch-recovery-source-manifest.json` 和 `...-source-verification.json` 核对 **1,178 文件运行前后一致**；相对上一 1,169 文件清单，11 个既有文件变化、9 个新增文件，均在后端源码、测试和新迁移内，前端构建无变化。文档单独更新；最终验证后只同步文档。迁移仅在隔离测试服务应用，没有重置开发数据库或执行真实云计费验收。

## Vertex/GCS 批任务结果收集（2026-09-27）

补充真实 HTTP 网关、隔离 PG/Redis、受控 OAuth/Vertex/GCS 的端到端用例后，先复现四个实际问题：官方失败行的 status 与空 response 被误判冲突；收集忽略返回的具体目录而读取相邻目录；多文件各自有界但没有共享总量限制；成功任务缺结果文件时会提前按零结果结算。`/tmp/okapi-vertex-batch-red.log` 实际退出 **101**，**0 passed、4 failed**，四项均进入业务断言失败，不是夹具或编译故障。官方输出格式及本地边界见 [协议说明](native-image-batches.md)。

修复后的收集路径按返回目录和对象 generation 读取，共享原存储预算两倍的文件总预算，空白字节也计数。JSONL 可显式配置总量至 8 GiB，保持逐行流式读取和 64 MiB 单行上限；没有运行 8 GiB 吞吐基准。合法 status/空 response 作为失败条目，错误配真实非空响应仍拒绝。Succeeded/PartiallySucceeded 缺槽位结果继续 collecting；取消或失败终态保留完成图片，按实际成功数收费。

- 最终 `/tmp/okapi-vertex-batch-verified.log` 实际退出 **0**，对应报告 **50 passed、0 failed、0 ignored、0 软跳过**：22 项网关业务、28 项协议。包含新增 9 项 Vertex 业务和 2 项协议测试，也保留全部原 Gemini 批任务与找回用例。
- 业务断言覆盖真实令牌交换路径、固定 generation、上传回包丢失后哈希核验、创建回包丢失后跨进程分页恢复、价格快照、唯一账单、取消后部分产出收费、后页错误/断流、重复行、畸形/矛盾响应、超总预算、缺结果重试、错误后冻结金额及成功私有下载字节。OAuth/Vertex/GCS 为模拟上游，未验证真实 IAM、供应商费用或云服务时序。
- `/tmp/okapi-vertex-batch-clippy-final.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**；格式、diff、金额守卫、错误码守卫及 API 清单通过。没有新增公开路由、依赖或数据库迁移。
- `/tmp/okapi-vertex-batch-source-manifest.json` 与 `...-source-verification.json` 核对 **1,182 文件运行前后一致**；相对上一清单，5 个既有文件变更、4 个新增，均为后端或测试。前端无改动，验证后仅更新文档。

本轮执行与修改相关的定向回归，没有重跑全工作区；上一节 819 项是当时版本的全量结果，不能与新增用例相加。物理清理、ZIP、筛选、完整准入、多模态用量和真实云端验证仍待完成，入口仍默认关闭，核心对标目标继续进行。

## 原生 Batch 删除和到期回收（2026-09-27）

新增 `0012_image_batch_cleanup.sql`、终态清理租约和后台回收路径。已删除或到期任务必须具有匹配的 closed hold 才能领取；远端先确认终态并删除 job，再清理输入与输出。Vertex 的异步删除 operation 持久化，重启继续查询，最终以原 job 的新鲜 GET 404 确认；operation 失败/404 不被单独当作已删除。GCS 列举任务目录全部 generation，固定版本删除后从目录起点重扫，避免分页位置变化导致漏删。

本地事务在远端文件清理完成后删除私有输入、连接快照、会话、条目与图片，释放产物名额和预算；保留每任务 512 KiB 元数据预算、历史金额、账单、资金凭证及幂等身份。主动删除后的原幂等键返回冲突，到期后同一请求仍返回原任务。公开元数据增加 cleanup_done；DELETE 的 cleanup_pending 反映是否仍待回收。清理不调用生成或结算逻辑，不重新扣款。

- 首轮 `/tmp/okapi-batch-cleanup-targeted.log` 实际退出 101，编译发现测试辅助方法的模块可见性问题，尚未执行业务测试；不能计为业务问题复现。
- 修正后 `/tmp/okapi-batch-cleanup-targeted-2.log` 实际退出 **0**，报告 **103 passed、0 failed、0 ignored、0 软跳过**：HTTP 业务 27、账本/存储 45、协议 31。包含自动到期 worker、Gemini 文件结果、删除响应丢失、权限失败、Vertex 异步删除与全版本回收、唯一账单/实际余额、并发领取、过期租约及容量/幂等保留。
- 此后补强 Vertex 重启后沿用原账号/operation、远端状态倒退拒绝、空分页循环/畸形后页/其他任务对象拒绝。严格 Clippy 指出的按值借用、测试状态组织、函数长度和重复导入均已修正，没有降低 lint 级别。最终 `/tmp/okapi-batch-cleanup-clippy-final.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**。
- 最终源码全量回归 `/tmp/okapi-batch-cleanup-full.log` 已观察到实际退出 **0**，对应 `...-full-report.json`：**842 passed、0 failed、0 ignored、0 软跳过**，129 个完整套件，无未结束套件或解析错误。其中批任务 HTTP 28、长期冻结/批任务存储 45、原生协议 31；以上 103 项是中间定向运行，未用于推算最终总数。
- 425 条 API 探针独立统计：权限拒绝 141、匿名鉴权拒绝 171、HEAD 状态 80、公开契约 21、错误壳 10、CONNECT 1、WebSocket 方法 1；它们不代表全部成功业务场景。格式、diff、金额守卫、错误码守卫与 API 清单检查均通过。
- `/tmp/okapi-batch-cleanup-source-manifest.json` 与 `...-source-verification.json` 核对 **1,190 文件运行前后一致**。相对上轮清单，10 个既有后端/测试文件变化、8 个新增后端/测试/迁移文件；前端构建、依赖和公开路由清单无变化。迁移仅在隔离测试数据库应用；最终验证后仅同步文档。

物理回收指执行供应商删除及本地 SQL 清除，不绕过云桶软删除/保留策略、备份或底层介质回收。测试使用受控 Gemini/Vertex/GCS 和隔离的真实 PG/Redis，真实云端 IAM、费用和时序仍未验收。ZIP、筛选、完整准入、用量完善及其他核心对标缺口继续保留；本轮未改前端或新增公开路由。


## 原生 Batch ZIP 和列表筛选（2026-09-27）

新增 `GET /v1/images/batches/{id}/download` 和可用时的 download_url，原用户/key 才能下载已结算成功图片，不要求仍有余额，也不重复生成或扣费。ZIP 包含安全槽位文件名和公开结果清单，用户 custom_id 仅出现在 JSON，不参与路径。逐张读取、验证长度及 SHA-256，Stored ZIP 经 64 KiB 管道发送；损坏/超时令响应体失败。HEAD 仅查元数据，不记录下载。列表支持 status、q、created_from/created_before、downloaded，在 SQL 中先筛选再分页，默认 20、最大 100；删除当前游标记录不妨碍同归属继续翻页。

迁移 `0013_image_batch_downloads.sql` 保存 600 秒租约，进程内共享 4 个下载槽、单任务最多 16 个跨进程有效租约；生成 ZIP 的任务最多运行 590 秒。领取下载与回收锁定同一批任务，回收在拿锁后复核下载租约；已授权下载可在用户删除后完成，新的下载拒绝。断开或 EOF 释放租约，进程异常依靠到期恢复；流不长期占用 PG 连接。依赖增加 async_zip 0.0.18（只启用 tokio，Stored 模式）及锁文件中的两个传递依赖。

- 第一轮 `/tmp/okapi-batch-archive-targeted.log` 实际退出 101，测试使用了当前未启用的 reqwest query 方法，尚未执行测试；改为 URL 编码已有查询参数，不增加网络客户端功能。
- 第二轮 `/tmp/okapi-batch-archive-targeted-2.log` 实际退出 101，**81 passed、1 failed**。失败的零余额下载夹具只修改 PG，而 Env 已在 Redis 预置余额；改为同时通过 ledger.repair 同步测试起始余额，保留耗尽到零与下载后仍为零的断言。这不是生产逻辑缺陷复现，也未放宽账单断言。
- 严格 Clippy 提示路由/测试函数长度和日志回调按值借用，按职责抽取/复用已有 helper，未降低 lint。`/tmp/okapi-batch-archive-clippy-final-r5.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**；格式、diff、金额/错误码守卫、API 清单检查通过。
- `cargo deny --locked --offline check licenses sources` 实际退出 0，许可证与来源通过；仅提示本配置未启用的 option-ext 例外。此检查未宣称在线漏洞库更新或漏洞审计通过。
- Python 标准库 zipfile 独立读取实际 HTTP 响应产物，验证 CRC、准确文件集合、PNG 魔数、失败槽位和含路径字符的 custom_id 只在清单出现；不是只由同一个 Rust ZIP 库自写自读。产物 `/tmp/okapi-batch-archive-independent-final.zip`、报告 `...-independent-final-report.json`，来自最终定向运行的下载用例；清单中本次 downloaded_at 已同步存在。图片为 28 字节协议夹具，不能当成完整图片解码或大容量性能验收。

- 最终定向 `/tmp/okapi-batch-archive-targeted-final.log` 实际退出 **0**，报告 **82 passed、0 failed、0 ignored、0 软跳过**：HTTP 业务 33、账本/存储 47、路由契约 2。覆盖 ZIP 内容、越权/到期/未发布拒绝、零余额、1 MiB 慢读背压、4 路并发与取消释放、下载期间 PG 连接可用、内容损坏、删除/租约到期回收、16 租约上限、筛选分页及 HEAD 无副作用。427 条 API 权限/错误探针单独统计。

- 最终全工作区 `/tmp/okapi-batch-archive-full.log` 已观察到实际退出 **0**，对应 `...-full-report.json`：**849 passed、0 failed、0 ignored、0 软跳过**，129 个完整套件，无未完成套件或报告解析错误。HTTP 业务 33、账本/存储 47、原生协议 31 随全量再次通过，不以先前 842 项或定向用例相加推算。
- 427 条探针单列：权限拒绝 141、匿名鉴权拒绝 172、HEAD 状态 81、公开契约 21、错误壳 10、CONNECT 1、WebSocket 方法 1。静态 API 清单现为 286 个角色/方法/路径组合；路由清单与探针不是所有成功业务路径的覆盖证明。
- `/tmp/okapi-batch-archive-source-manifest.json` 和 `...-source-verification.json` 核对 **1,197 个验证文件运行前后一致**；相对清理阶段，12 个既有文件变化、7 个新增，涉及后端/测试/迁移/依赖/生成 API 清单，前端构建保持原指纹。许可证/来源检查通过，未执行在线漏洞库刷新。迁移仅应用于隔离测试数据库；未改前端源码或构建产物。

最大容量、真实供应商/IAM、完整准入及多模态统计仍未验收；批任务创建开关仍默认关闭，核心对标目标继续进行。本轮性能证据仅为 1 MiB 背压与连接可用性断言，不能外推为 200×16 MiB 或生产吞吐验收。


## 原生 Batch 统计补记与软限额（2026-09-27）

复核发现批任务绕过普通请求的结算统计旁路：实际账单已写入，成员月消费、volume 的月 Token/消费、渠道 key 日消费、实时 KPI 没有更新；异步 latency_ms 恒为 0。`/tmp/okapi-batch-statistics-red.log` 实际退出 **101**，**0 passed、2 failed**，分别观察到真实消费 20,000 micro 后成员累计为 0，以及创建时间提前两分钟的任务账单耗时为 0；这两项是新增断言复现的生产缺口。

迁移 `0014_image_batch_statistics.sql` 保存创建时成员和结果封存时间，并增加与首次公开终态同事务的统计投递记录。金额/Token 取已关闭的结算凭据，桶时间取首次账单 created_at；投递独立租约、重试及每指标 Redis 同槽凭据，补记可在图片清理后继续。重放不重新生成、不重复扣费，不把旧月/秒桶改为当前流量；过期桶跳过。金额通过 INCRBY 的整数文本参数累加，不转 Lua double。新耗时为创建至结果封存，首次封存后不随结算重试变化；不是供应商 RTT，旧 settling 记录保留原先 0 以兼容已写收据。

成员限额在创建与发送前检查；任务创建后修改 key 归属不改变已经提交任务的消费成员。渠道日消费在批任务创建时筛选候选，发送前复查原账号；达到上限时不会继续发送新生成。volume 报价读取补记后的 Token/实付金额。成员和渠道仍使用已有的结算后软限额，不声称包含全部在途冻结的原子硬预算；Redis 全库丢失/独立键淘汰后的统计重建尚未实现。

- 首轮修复定向 `/tmp/okapi-batch-statistics-targeted.log` 实际退出 **0**，6 项通过。覆盖成员下一笔拒绝、真实耗时、统计中途失败和确认丢失/重启、固定成员归属、发布事务失败后相同收据重试、提交前限额复查、下一笔 volume 折扣和渠道日上限。
- 后续增加跨月/过期桶、KPI 重放、大整数、统计租约、图片回收后仍可补记，以及 Redis Cluster 哈希槽验证；最终实际结果另记，不把中间定向数相加。
- `/tmp/okapi-batch-statistics-clippy-pass.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**。格式、diff、金额与错误码守卫和 API 清单同步通过，未新增依赖包、未修改已应用迁移。
- 工作区在本轮同时收到其他任务的目录接口、Ingress/错误提示、OAuth/降级测试与前端构建产物变更；保留这些内容。为保持严格检查，对目录能力查询及 OAuth 测试辅助逻辑做了行为不变的函数拆分。当前验证对象是合并后的实际源码，不将这些并行功能归为本轮统计实现，也不声称工作区全部前端文件均未变化；本轮没有编辑前端或运行其构建。

- 关联回归 `/tmp/okapi-batch-statistics-related.log` 实际退出 **101**：**103 passed、1 failed**。42 项 Batch HTTP（含全部 9 项统计用例）、47 项账本/存储及相关 OAuth、门户、降级测试通过；全路由探测发现公开 `/api/pricing` 的 GET 和 HEAD 超过原 10 秒时限。保留这一失败，不以通过子集替代整体结果。
- 对隔离数据库的只读核对发现约 9,000 个模型、783 个分组，多数分组使用 default 池。并行新增的每分组接口能力字段导致重复展开大量 JSON 节点。后端修复把模型/池/接口能力建索引，相同可见性复用预序列化 JSON；保持分组、端点、空列表、可空说明和公开字段白名单的返回契约。HEAD 省去正文生成。仅开启已存在 serde_json 包的 raw_value 功能，未新增包；分组隔离、降级池和厂商能力差异另加测试，不增加原探测超时、不清理测试数据。

- 目录修复首轮 `/tmp/okapi-batch-statistics-pricing-targeted.log` 实际退出 **101**，89 passed、1 failed。原 GET/HEAD 10 秒路由探测恢复；新增 HEAD 契约断言发现框架从空正文推断 `Content-Length: 0`，与 GET 不一致。改为不声明长度的空流，让框架正确省略 Content-Length，保留原断言。
- 最终目录定向 `/tmp/okapi-batch-statistics-pricing-targeted-2.log` 实际退出 **0**，**90 passed、0 failed、0 ignored、0 软跳过**：75 项 okapi 单元、6 项诊断、7 项门户、2 项路由；427 条权限/错误探针另计。该次 GET 从发出请求到接收完 832,093,454 字节耗时 1,048 ms；这是隔离容器/本机测试的一次观测，不是生产吞吐或最大容量承诺。解析时间未计入该耗时；完整目录仍有巨大响应，兼容模式的分页/响应体规模需要继续处理。
- 最终 `/tmp/okapi-batch-statistics-clippy-verified.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**，格式、diff、金额、错误码、API 清单检查均通过。最终源码指纹记录 `/tmp/okapi-batch-statistics-final-source-manifest.json`，含 1,205 个验证文件。

- 完整工作区 `/tmp/okapi-batch-statistics-full.log` 已观察实际退出 **101**；对应 `...-full-report.json` 为 **862 passed、1 failed、0 ignored、0 软跳过**，129 个完整套件，无未完成套件或解析错误。42 项 Batch HTTP、47 项账本/存储、31 项原生协议均通过。427 条 API 探针全部完成，分类与前述定向一致，未相加为业务测试数量。
- 唯一失败 `compact_filters_incompatible_and_explicitly_disabled_channels` 是并行新增 Ingress 错误分类后遗漏的旧 `503` 断言：有渠道但入口不兼容现在返回 `400 unsupported_endpoint`，并列出允许的入口。保留生产行为，只强化该测试，精确断言 400/错误码/端点提示/request_id，再停用渠道验证 503/no_available_channel，且两种拒绝都不调用上游、不扣费。`/tmp/okapi-batch-statistics-responses-contract.log` 实际退出 **0**，完整 Responses 套件 **13 passed、0 failed**，不是仅重跑一个断言。
- 全量结束后的严格 `/tmp/okapi-batch-statistics-clippy-contract-final.log` 实际退出 **0**；格式、diff、金额、错误码、API 清单检查通过。`...-final-source-verification.json` 核对 1,205 文件：全量后仅 `bins/okapi/tests/gateway_responses.rs` 变化，无新增/删除，生产源码、依赖、迁移和内嵌资源不变。修正后的完整文件指纹见 `...-responses-source-manifest.json`。因只改一个测试，未再次执行完整工作区；保留全量失败报告，不拼接结果宣称单次 863 项全过。

本轮仅编辑后端、测试和验证文档，未运行前端构建，未应用迁移到开发数据库。完整限流、全在途硬预算、多模态用量细分、目录响应体规模及真实云端验收等核心差距仍在继续处理。


## 后端模型目录分页与旧收据兼容（2026-09-27）

在保留旧全量倍率导出的基础上，新增 `GET /api/pricing/models`，默认 20、最多 100 条；旧 `/api/pricing` 显式带分页/筛选参数或 `paged=true` 也进入有界分页。支持字面关键词、精确模型、厂商/未归类、明确布尔能力、分组及调用入口的组合筛选，先筛选再 LIMIT/OFFSET。total、厂商统计、模型和池能力在同一次只读 Repeatable Read 事务读取，发出正文前释放连接。只为当前页展开可用分组和接口；端点判断复用网关 Ingress。GET/HEAD 的分页响应头一致，原全量 JSON 字段保持兼容。契约及限制见 [模型目录](model-catalog.md#后端分页目录2026-09-27)。

- `/tmp/okapi-model-catalog-red.log` 实际退出 **101**，0 passed、1 failed：新路径尚无后端 JSON 分页，SPA 兜底返回 HTML。首次严格检查发现 SQLx 0.9 不接受 format! 生成的动态 SQL 字符串，改为 QueryBuilder 拼接固定片段、值全部绑定；未绕过 SQL 安全检查。`...-clippy-2.log` 实际退出 **0**。
- `/tmp/okapi-model-catalog-targeted.log` 实际退出 **101**，尚未执行业务测试：工作区并行加入缓存是否上报字段，audio/chat/embeddings/realtime 的 5 个 TokenUsage 初始化未补齐。按未上报/估算的语义补 false，不将其标为真实零命中；保留并行改动及其前端构建产物，本轮没有编辑前端或执行构建。
- `/tmp/okapi-model-catalog-targeted-2.log` 实际退出 **0**，**21 passed、0 failed、0 ignored、0 软跳过**：新目录 6、原门户 7、诊断 6、路由 2。验证跨页无重复遗漏、未定价/停用排除、厂商统计、字面 `%`/`_`/反斜线与 SQL 注入文本、公开字段白名单、上限/越界/非法参数、主池/降级池与 Codex 接口隔离、HEAD、旧接口显式分页。429 条 API 探针单列，其中公开契约 23，其他分类与上一轮一致。
- 同一隔离大目录的一次 HTTP 观测：默认 20 条返回 **2,282,543 字节、28 ms**；旧全量返回 **918,666,507 字节、1,939 ms**。不含后续 JSON 解析，不是生产吞吐/负载验收；分组仍全量返回，体积也受分组数影响。现有前端选择器/目录仍调用旧全量路径，本轮只提供后端迁移能力，没有宣称旧页面已使用服务端分页。

检查并行新增缓存状态字段时，发现原持久冻结收据采用 JSON 严格相等比较，旧 receipt.usage 缺少这两个字段会被判为另一笔结算。`/tmp/okapi-cache-receipt-red.log` 实际退出 **101**，0 passed、1 failed、47 filtered，真实 PG/Redis 回放返回 HoldConflict。修复仅在重放比较时将缺失字段补为默认 false，不修改原收据；金额、Token 和显式状态变化仍拒绝。新增用例同时验证旧收据两次重放、原收据不改写、余额不变、账单/outbox/key 累计仅一次，以及 Token 或明确上报状态变化仍冲突。

- `/tmp/okapi-model-catalog-ledger-related.log` 实际退出 **0**，**114 passed、0 failed、0 ignored、0 软跳过**：完整 durable_holds、Batch HTTP、audio、embeddings、realtime 和 Responses 六套件。包含新增旧收据回放用例，未重写原持久收据。
- 并行缓存采集状态改动还遗留了计价/转换测试初始化及私有解析结构命名告警。保留当前明确上报的 true 字段，估算为 false，JSON 字段名不变；纯计价夹具通过 Default 补齐其他字段，现有金额断言保留。最终 `/tmp/okapi-model-catalog-clippy-final-4.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**；格式、diff、金额、错误码及 API 清单检查通过。
- 全量运行前 `/tmp/okapi-model-catalog-full-source-manifest.json` 记录 1,211 个验证文件，包括当前并行改动的缓存采集和分析实现，以及其他任务生成的前端资源。本轮没有编辑或构建前端。
- `/tmp/okapi-model-catalog-full.log` 已观察实际退出 **101**，对应 `...-full-report.json` 为 **875 passed、2 failed、0 ignored、0 软跳过**，131 个完整套件，无未完成套件或解析错误。目录 6、Batch HTTP 42、长期冻结/存储 48、原生协议 31、Responses 13 项均通过；429 条探针分别为权限拒绝 141、匿名鉴权拒绝 172、HEAD 状态 81、公开契约 23、错误壳 10、CONNECT 1、WebSocket 方法 1，不当作成功业务覆盖率。
- 两处失败为 `console_logs::log_stat_switches_rate_source` 和 `console_portal::partner_employee_keys_see_own_usage`。前者固定缓存读 40 的 outbox 样本未标记已上报，后者要求已采集写入为零却未在模拟上游明确发送零。仅对样本补 `cache_read_reported=true` 和 `cache_write_tokens=0`，另断言日志已知请求数为 5；保留原命中率、真实零值、账单金额及账户隔离断言。缺失字段仍保持未知，未修改生产聚合规则来迁就测试。
- `/tmp/okapi-model-catalog-cache-contract.log` 实际退出 **0**，对应报告 **40 passed、0 failed、0 ignored、0 软跳过**，五个完整套件：日志 13、门户 1、统计 9、分析 13、供应商缓存协议 4。最终 `/tmp/okapi-model-catalog-clippy-contract-final.log` 严格全工作区/all-targets/`-D warnings` 实际退出 **0**；格式、diff、金额、错误码和 API 清单守卫均通过。
- `...-full-source-verification.json` 确认完整运行期间后端生产源码、测试、依赖和迁移与起点一致；前端 dist 被并行任务更新，1 个同名文件变化、58 个旧资源替换为 58 个新资源。本轮未编辑这些资源，不能声称所有验证文件均未改变或据此验收最新 UI。`...-final-source-verification.json` 显示全量后后端仅上述两个测试文件变化，最终 1,211 文件指纹保存于 `/tmp/okapi-model-catalog-verified-source-manifest.json`。只变动测试样本及断言，未再次执行整轮；保留全量失败报告，不将多轮结果拼成单次 877 项全过。

本轮仅编辑后端、测试和验证文档，没有新增迁移或依赖包，没有操作开发数据库。旧全量目录调用方迁移、分组规模控制、完整准入/在途硬预算、多模态用量细分和真实云端验证等差距仍保留，整体核心对标目标未完成。

## 批任务共享请求次数准入（2026-09-27）

批任务原来用 Unix 天数作为 RPD 桶，而普通请求账本及速率查询用 UTC YYYYMMDD，交替请求可绕过同一密钥日额度；批任务只按一笔计数，也未检查模型 RPM。限流早于数据库幂等确认，并发重放会重复占次数或被错误拒绝。`/tmp/okapi-batch-admission-red-4.log` 实际退出 **101**，**0 passed、6 failed、42 filtered**，六项真实 HTTP/PG/Redis 用例复现跨接口日上限、展开子请求数、模型/分组限制、幂等并发和异常计数问题。前三次日志仅为编译失败，不能计为业务问题复现。

新 rate 模块复用普通请求的五个次数键，全部带用户 hash tag，按展开的上游行数增加。单 Lua 写入前验证全部计数，不足返回对应 429，非法数值/类型或连接故障返回 503。存储层新增准入回调：先检查幂等、父任务、独立 key 预算和容量，再在现有创建锁下执行限时 Redis 检查，最后插入任务与 pending hold。回调提前加载设置，不另取 PG 连接或调用供应商。Redis 已执行而 PG 提交失败或未知时保守保留次数，不猜测回退。没有回填旧错误 RPD 桶。详细口径和边界见 [批任务契约](native-image-batch-jobs.md#创建时的请求次数限流2026-09-27)。

- `/tmp/okapi-batch-admission-targeted.log` 实际退出 **0**，六项复现用例通过。随后增加三个边界用例，覆盖两个独立网关共享幂等与最后额度、数据库预算/父任务/容量先拒绝，以及最后一个 Redis 轴类型错误时前面计数不部分写入；另验证非规范数字、负值和越界数值。
- 工作区并行加入门户日志汇总、账单用量明细和 `0015_billing_usage_details.sql`。本轮保留并验证这些改动，仅修正其 SQLx 0.9 类型签名、无用字符串定界符和测试排版；不将这些功能归为本轮限流实现。限流没有新增依赖或迁移，未修改已应用迁移；数据库执行限于隔离服务。
- `/tmp/okapi-batch-admission-related.log` 实际退出 **101**，对应报告 **164 passed、2 failed、0 ignored、0 软跳过**，10 个完整套件，无未完成套件或解析错误。通过：Batch HTTP 51（含准入 9）、持久资金/存储 48、原生协议 31、普通请求 6、调度 5、分组限流 4、兼容/模型限流 4、门户 8、路由 2；PG 记账为 5 通过、2 失败。
- 431 条 API 探针单独统计：权限拒绝 141、匿名鉴权拒绝 173、HEAD 状态 82、公开契约 23、错误壳 10、CONNECT 1、WebSocket 方法 1。新增探针来自并行门户 `/api/me/logs/stat` 路由，清单与源码同步；不作为全部成功业务覆盖证明。
- 两处失败都在并行新增的 PG 用量明细断言：输入 cache_write_tokens=10，编译时断言为 0。运行期间并行任务已将断言修正为 10。当前源码完整 `/tmp/okapi-batch-admission-ledger-current.log` 实际退出 **0**，**7 passed、0 failed、0 ignored、0 软跳过**。字段落盘、幂等/并发、事务回滚、退款和订阅池均通过；保留失败报告，不拼为单次 166 项全过。
- `/tmp/okapi-batch-admission-clippy-final.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**；格式、diff、金额、错误码和 API 清单守卫通过。`...-source-manifest.json` 与 `...-source-verification.json` 核对 1,216 文件：生产后端保持一致，运行期间只有上述 PG 测试断言变化；其他任务更新了前端 1 个同名文件、153 个替换资源，本轮未编辑或构建前端。当前指纹见 `/tmp/okapi-batch-admission-verified-source-manifest.json`。

本轮未重跑全工作区或进行真实云端验收。TPM、多模态估算、排队后发送窗口、渠道/跨接口并发和全部在途硬预算仍待完成，核心对标目标继续进行。


## 批任务与普通请求共用密钥并发（2026-09-27）

原普通预扣只读取 `conc:{uid}:k:<kid>`，长期冻结只检查资金与持久任务容量，两个入口可分别通过同一密钥的并发上限。新增真实 HTTP 复现 `/tmp/okapi-batch-concurrency-red.log` 实际退出 **101**，**0 passed、2 failed、51 filtered**：已冻结批任务存在时普通请求仍返回 200；普通 HTTP 尚在上游执行时批任务也直接进入 preparing。

修复在相同用户 Redis 槽内，将普通占用与 `bal:{uid}` 的长期 `hc:<kid>` 占用合并检查。冻结、关闭和 PG 权威恢复同步维护派生计数；旧版无索引时从 held 凭证推导。每个已冻结任务占一个密钥名额，未冻结任务满额时保持 funding，约三秒后重试且不冻结资金、不伪造上游错误。取消意图、未知提交、重启及幂等重放都不提前释放名额；真正结算关闭才释放。详见 [长期冻结契约](durable-balance-holds.md)。

- 初步定向 `/tmp/okapi-batch-concurrency-targeted.log` 实际退出 **0**，两项最初复现通过。随后将排队改为无错误的重试，并补充恢复、取消与账本边界。
- 当前完整关联 `/tmp/okapi-batch-concurrency-related.log` 实际退出 **0**，**116 passed、0 failed、0 ignored、0 软跳过**，4 个完整套件：Batch HTTP 55、长期冻结/存储 52、worker 修复 4、重复预扣 5。包含新增 4 项 HTTP 与 4 项账本测试；后者覆盖普通/长期原子竞争、零费用占用、重复关闭、旧索引兼容、异常索引拒绝且无写入、PG 冷恢复、不同 key 隔离和调低上限。
- 严格全工作区/all-targets Clippy `/tmp/okapi-batch-concurrency-clippy.log` 实际退出 **0**；格式、差异、计费浮点、错误码与 API 清单守卫通过。1,220 文件源码指纹包含 Lua，关联运行与后续全量启动间无变化。

验证使用隔离真实 PG/Redis 及受控 HTTP 上游。此修复不代表渠道并发、供应商批任务内部并行数、TPM 或在途硬预算已完成。Redis 全丢后须先完成 PG 权威恢复，尚不能保证恢复前零费用普通请求识别已丢失的长期占用。本任务未修改前端；完整后端回归结果如下。

最终完整运行 `/tmp/okapi-batch-concurrency-full.log` 实际退出 **0**，报告 `/tmp/okapi-batch-concurrency-full-report.json` 为 **895 passed、0 failed、0 ignored、0 软跳过**，131 个完整块，解析错误与未结束块均为空。431 条探针为 141 权限拒绝、173 匿名鉴权、82 HEAD 状态、23 公开契约、10 错误壳、1 CONNECT、1 WS 方法检查；不视为 431 个完整 API 业务场景。上面的 116 项关联验证不另加到 895 中。

最终源码记录 `/tmp/okapi-batch-concurrency-verified-source-manifest.json` 与初始 1,220 文件比较，所有后端源文件、Lua、配置、测试和迁移完全一致。全量过程中共享前端 dist 的 index 内容变化、58 个资源替换，因此最初的全文件相等断言失败；进一步逐项核对确认变化均为前端产物，保留差异清单，不将其改写成整个工作区一致。本任务未生成前端构建，也未为这些外部变化重跑后端全量。完整测试结束后格式、差异、计费浮点、错误码、API 清单守卫再次通过；其后仅更新文档。

额外容量观察：本次全量 `console_portal_pages::public_pricing_no_auth` 在积累的隔离测试库中记录旧无参数 `/api/pricing` 响应 **1,057,435,934 字节**，客户端读取耗时日志 **2,341 ms**。这是特定测试库与本地链路的观察，不是生产容量或性能验收。新 `/api/pricing/models` 默认 20/最多 100 条并不限制旧全量入口；每模型可用分组展开与全量分组清单还可能放大响应。这一后端容量缺口需继续处理，不能因旧契约测试返回 200 而视为已经解决，也未通过清空测试库掩盖它。


## 实际目录入口、嵌套列表与同步的容量边界（2026-09-27）

上一阶段观察到旧 `/api/pricing` 默认全量且每模型重复所有分组/入口，隔离测试库响应超过 1 GB；新增分页模型路由没有封住实际旧入口和嵌套列表。先补三项 HTTP 复现，`/tmp/okapi-catalog-bounds-red.log` 实际退出 **101**，**0 passed、3 failed、6 filtered**：旧入口 HEAD 没有分页上限、模型页仍返回全部分组、新分组查询落到 SPA HTML。

本阶段只修改后端、测试与文档：两个目录入口现在无条件默认模型分页；分组和厂商也独立默认 20/最多 100。当前模型仅展开当前分组页，完整性通过分页元数据表达。模型的分组/入口筛选移到 SQL EXISTS，避免全量取出所有模型路由后再筛选；数据库入口判据和网关 Ingress 用 12 个供应商/配置组合对照。分组列表增加公开查询，支持关键词、精确代码和模型可达性，不回显池地址/凭证。契约及调用方影响见 [模型目录](model-catalog.md#当前后端有界目录与倍率同步2026-09-27)。

分页改变了原全量来源契约，因此一并适配后端倍率同步：保留原地址和筛选，只跟进校验后的数字 offset，不跟随 next_url/重定向；校验总数、条目数、重复模型和推进关系。逐块读取实施单页 2 MiB、总计 16 MiB、512 页及整条来源的超时限制；后续任何一页失败，都不提供部分价格差异。旧三种来源形状与逐项应用仍由原测试验证。

执行证据（隔离真实 PG/Redis，受控本地 HTTP；没有调用真实供应商或清空测试数据）：

- `/tmp/okapi-catalog-bounds-related.log` 实际退出 **0**，**34 passed、0 failed、0 ignored、0 软跳过**：目录 13、诊断 6、门户 8、倍率同步 5、路由 2。433 条探针单列为 141 权限、173 鉴权、82 HEAD 状态、25 公开契约、10 错误壳、1 CONNECT、1 WS 方法，不是 433 个成功业务验收。
- 目录 13 项覆盖两入口默认封页（包括 paged=false）、分组/厂商稳定翻页、字面搜索、真实计数、超大 limit 收敛、非法参数、越界 offset、公开字段白名单、零路由/停用/删除、主池/降级池和全部五个聊天入口。旧门户/诊断测试改为显式请求目标模型及分组，保留原价格、能力和归属断言；不再要求某个模型必须出现在默认第一页。
- `/tmp/okapi-catalog-bounds-units-sync.log` 实际退出 **0**，**81 passed**：okapi 库单元 76、完整倍率同步 5。之后新增累计来源容量/最多页数边界，`/tmp/okapi-catalog-bounds-sync-final.log` 实际退出 **0**，完整同步 **6 passed**，包含真实 512 次分页请求后拒绝，以及累计正文达上限时拒绝；都断言没有部分差异。两轮均无失败、忽略、软跳过、解析错误或未完成块，不拼接成一次总通过数。
- `/tmp/okapi-catalog-bounds-clippy-final.log` 全工作区/all-targets/`-D warnings` 实际退出 **0**；格式、差异、计费浮点、错误码、API 清单守卫通过。初期 SQLx 0.9 安全 SQL 构造/类型参数及 reqwest 夹具编译失败日志保留，不能计为业务复现或通过。
- 最终 1,228 文件指纹保存在 `/tmp/okapi-catalog-bounds-source-manifest.json`，采集于集成测试和最终静态检查完成后，不宣称本阶段执行了完整工作区测试或具备全程同一前端快照。与前一阶段相比的后端新增/修改均为本次目录、同步、测试与路由清单。

容量观察：同一积累的隔离库中，新默认 `/api/pricing/models` 正文 **65,225 字节**，接收日志 **24 ms**；直接旧入口 `/api/pricing` 的无参数及 paged=false 用例均要求正文小于 256 KiB，并验证三列表上限。旧公开单模型测试改用 model 精确筛选后记录 3,200 字节，该值不能拿来冒充默认目录体积。以上是本地测试样本，不是生产吞吐/延迟承诺。

没有修改/构建前端、重启开发实例或部署。现有模型广场、模型输入、Playground、连接指南的一次性拉取尚未改为读分页元数据，配合新后端会只拿到首批数据；这个调用方接入仍须完成。跨页配置快照、完整 TPM/渠道并发/在途硬预算以及真实云端验收等核心缺口继续保留，目标未完成。

## 普通请求预扣的数据异常原子性（2026-09-27）

按用户要求继续核对后端。普通 `reserve.lua` 原先在 cap≤0 时跳过计数器读取，却仍在扣款和写入 r:* 后递增；计数器类型/数值异常会触发 Redis 运行时错误，已有资金写入不会回滚。内部传入负数预扣也会增加钱包余额。四项新增真实 Redis 复现 `/tmp/okapi-reserve-atomicity-red.log` 实际退出 **101**，**0 passed、4 failed、5 filtered**，均进入业务断言，包含钱包、订阅池、异常计数及负数预估。这里没有证明负数预估可由公开 API 外部直接构造。

修复保留原选池、订阅最后一笔可越界、重复预扣拒绝与共享并发规则。任何资金写入前先验证 RPM、TPM、RPD、普通并发四个会修改的键，即使不限额也检查类型、规范十进制整数和递增上界；金额/Token 预估必须非负，参与计算的整数限定在 2^53−1 内。大数增量以十进制字符串传给 Redis。新增内部错误仍映射既有 HTTP 500 `internal_error`，异常计数不会自动清零或伪装成正常限流。脚本外模型/分组计数及 Redis 执行确认丢失不在本次原子范围内，详见 [账本契约](database.md)。

- `/tmp/okapi-reserve-atomicity-targeted.log` 实际退出 **101**，两项 HTTP 测试已通过拒绝请求的余额、计数、凭证、上游零调用检查，但在恢复成功后立即查询异步账单得到 0。按既有结算时序增加最多五秒的有界等待，保留唯一账单与资金一致性断言，没有修改生产结算逻辑或放宽为忽略。
- `/tmp/okapi-reserve-atomicity-targeted-r2.log` 实际退出 **0**，**13 passed、0 failed、0 ignored、0 软跳过**，两个完整套件，无解析错误或未结束块。其中账本 11 项含原有重放/并发 5 项与新增异常/边界 6 项；真实 HTTP 2 项分别核对钱包与订阅，四轴异常均不调用上游、不改余额/凭证/计数/TTL、不产生账单，修正计数后正常调用并生成唯一账单，余额变化等于账单金额。

此后在边界用例补充了四个计数恰好递增至 2^53−1 的成功断言。严格全工作区/all-targets Clippy `/tmp/okapi-reserve-atomicity-clippy.log` 实际退出 **0**；最终关联回归结果单独记录如下，不与前期 13 项相加。本轮没有修改或构建前端，没有新增迁移/依赖、操作开发数据库或部署。目录调用方接入、完整准入/在途预算及真实云端验证等原有缺口仍保留，整体核心目标未完成。

- 首次关联 `/tmp/okapi-reserve-atomicity-related.log` 实际退出 **101**，**125 passed、11 failed、0 ignored、0 软跳过**。11 个失败均为持久批任务存储用例创建时的 `Capacity`；真实 Batch HTTP 55、共享并发及 worker 修复等已通过。Cargo 在该套件失败后停止，未执行后面的 Lua、PG 记账与重放套件，不将其计为通过。
- 只读核实旧隔离 PG 库有 722 条批任务、674 条未清理，`storage_budget` 总计 **34,335,205,920 字节**，距离默认 **34,359,738,368 字节（32 GiB）**仅剩 **24,532,448 字节**，已不足新增默认三图任务的预留预算。记录于 `/tmp/okapi-reserve-atomicity-capacity.json`。这是预算容量拒绝，不是磁盘实占量；生产容量校验保持原样。
- 保留旧库、Redis DB0 及失败报告，新建 `okapi_reserve_atomicity_20260927` 专用 PG 库，使用核实空闲的隔离 Redis DB15，执行同一代码、同一 12 套件及未放宽的断言。`/tmp/okapi-reserve-atomicity-isolated.log` 实际退出 **0**，对应报告 **162 passed、0 failed、0 ignored、0 软跳过**，无解析错误或未结束块：订阅 6、兼容 4、分组限流 4、鉴权准入 3、普通计费 6、Batch HTTP 55、新预扣 HTTP 2、worker 修复 4、持久冻结/存储 52、Lua 契约 8、PG 记账 7、预扣重放/异常 11。容量拒绝本身的专用测试亦保留并通过，不把不同运行拼成一轮结果。
- `/tmp/okapi-reserve-atomicity-source-manifest.json` 与 `...-source-verification.json` 验证 **1,011 个后端源码、测试、Lua、SQLx 和依赖文件在严格检查及两次关联运行期间一致**。前端未纳入这个指纹，不声称工作区所有文件没有并行变化。最终格式、diff、金额与错误码守卫、API 清单检查通过；之后仅更新文档。本轮没有执行完整工作区回归或真实供应商费用验收。


## 普通结算、退款与资金池归属（2026-09-27）

继续核对后端关闭预扣的链路，原 commit/refund 在读取并发计数前先改钱、删除 r:*，遇到异常 Redis 类型会留下部分结果；负数 actual 会多退钱，大整数 tostring 可能转成科学计数法，导致钱已动而 Rust 解析报错。关闭操作还未核对凭证中的 key，订阅退款失败的 HTTP 账单会默认写为钱包。这些都是账本/接口问题，本阶段没有编辑或构建前端。

现在关闭脚本共用严格的只读预检：先校验凭证、原 API key、非负实际金额、余额与并发类型及安全整数结果，再改钱、删凭证和释放并发。金额以十进制字符串传递和返回，支持正负 2^53−1 的余额边界；API key 按字符串比较，支持完整 PG bigint。过期并发键不创建负计数，旧两段/三段凭证仍有明确兼容规则。订阅置额也先验证全部在途凭证和累加结果，防止大额度返回解析失败或异常窗口部分更新。详细契约见 [数据库账本说明](database.md#22-余额热账本与-lua-契约)。

- `/tmp/okapi-settlement-atomicity-red.log` 实际退出 **101**，0 通过/6 失败；其中恢复重试用例在大额度 sub_set 夹具准备阶段暴露科学计数法问题，不能说六项都已进入关闭断言。改用正常额度夹具并独立增加大额度测试后，`...-red-r2.log` 实际退出 **101**，**0 passed、7 failed、11 filtered**，保留两次原始证据。
- 初步脚本修复后 `...-targeted.log` 实际退出 **0**，7 通过/11 filtered；随后增加数字越界、旧凭证、大 key、订阅窗口、32 路并发关闭边界。HTTP 复现 `/tmp/okapi-settlement-http-red.log` 实际退出 **101**，1 通过/1 失败/2 filtered，订阅失败账单真实结果 `(status=40,pool=0,amount=0)`，预期 pool=1。Chat、Embeddings/Rerank 和 Realtime 现保留 reserve 返回的池，退款报错仍按原来源记录失败，不猜为钱包。
- `/tmp/okapi-settlement-atomicity-protocols.log` 实际退出 **0**，**34 passed、0 failed、0 ignored、0 软跳过**：账本 23、真实 HTTP 故障注入 5、Realtime 6。验证异常关闭前后的余额/凭证/并发、恢复后只关闭一次、失败账单的资金池、worker 延迟退款。成功 HTTP 的 Redis commit 失败用例只断言凭证保留并显式重试已知 mock 费用，未把它当作自动持久恢复证明。
- `/tmp/okapi-settlement-atomicity-related.log` 实际退出 **0**，**245 passed、0 failed、0 ignored、0 软跳过**，17 个完整套件，无解析错误或未结束块：账单一致性 4、订阅 6、audio 3、embeddings 3、images 3、图片契约 44、普通计费 6、Batch HTTP 55、Realtime 6、新预扣/结算 HTTP 5、Responses 13、videos 3、worker 4、长期冻结 52、Lua 契约 8、PG 记账 7、预扣重放/原子性 23。严格全工作区/all-targets Clippy `...-clippy-final.log` 实际退出 **0**。与启动前指纹相比，1,016 个后端源码、Lua、测试、SQLx 与依赖文件无内容变化。

上述运行使用新建专用 PG 库 `okapi_settlement_atomicity_20260927` 和核实空闲的 Redis DB14，避免前一 HTTP 红测故意留下的异常计数影响 sweep。此前测试库、Redis DB15、失败日志和容量拒绝证据全部保留，未清空或放宽生产限制。供应商为受控上游，没有真实云费用验收、开发实例重启或部署。

245 项运行之后又发现重复退款的元数据边界：r:* 已释放时，既有 refund 幂等结果为零金额/钱包池；请求稍后失败仍应记录它最初的订阅来源。新增 `/tmp/okapi-settlement-duplicate-refund-red.log` 与 `...-http-red.log` 分别实际退出 **101**，各 0 通过/1 失败，均真实得到 pool=0 而预期 1。首轮 Cargo 在 Realtime 失败后停止，HTTP 单独补跑，不能声称首轮执行了两个套件。现对零释放结果也采用原预扣池，测试还要求余额不重复增加、并发为零且账单唯一；最终复测见下。

最终 `/tmp/okapi-settlement-atomicity-final.log` 已观察实际退出 **0**，报告为 **74 passed、0 failed、0 ignored、0 软跳过**，八个完整套件，无解析错误或未结束块：账单一致性 4、embeddings 3、普通计费 6、动态价格 12、Realtime 7、HTTP 预扣/结算 6、账本重放/边界 23、Responses 13。重复退款用例覆盖 Chat、Embeddings、Rerank 和 Realtime，钱包/订阅异常退款及正常计费仍通过。与前面的 245 项为独立运行，不相加成一次总数。最终指纹与核对记录 `/tmp/okapi-settlement-atomicity-final-source-{manifest,verification}.json` 覆盖 1,016 个后端文件，最终复测期间无内容变化；前端未纳入指纹。

最终严格全工作区/all-targets/`-D warnings` 检查 `/tmp/okapi-settlement-atomicity-clippy-complete.log` 实际退出 **0**；格式、diff、金额与错误码守卫、API 清单均通过。本阶段没有新增迁移、依赖或公开路由；没有运行前端构建或部署。

仍存在实质性缺口：普通成功请求的实际用量尚未先持久化为结算意图，Redis commit 失败后进程退出时，worker 仍可能只能按过期预扣退款，不能保证按已完成的真实用量恢复。下一阶段应补持久结算意图与跨进程恢复；本轮只修复关闭过程和失败账单归属，不宣称该缺口已完成。repair 脚本的大整数/异常凭证边界、部分音频/视频失败记录、目录调用方分页接入及完整 TPM/预算等也继续保留。核心对标目标仍在进行，不能据关联测试声称已全面超越竞品。


## 普通请求先落盘、后同步余额（2026-09-27）

上一阶段只保留了失败的 Redis 预扣，成功请求仍没有可供 worker 使用的实际用量。将真实 HTTP 故障用例改为要求持久账单与自动恢复，`/tmp/okapi-durable-sync-red.log` 实际退出 **101**：0 passed、1 failed、5 filtered；上游已完成、实际费用 24 micro，但 PG 账单结果为空。此失败改变的是恢复要求，没有删除原余额/凭证原子性断言。

新增 `0016_billing_sync.sql` 和公共持久结算入口，账单、usage、价格、事件、用户/key 累计、outbox 与待同步记录同 PG 事务提交。各同步计费入口保留最初预扣池，之后再同步 Redis；Redis 错误不会丢弃已提交的实际用量。worker 不等十分钟到期就处理待同步记录；关闭确认丢失或 Redis 数据丢失时在用户锁下按 PG 事件恢复，保留其他活跃预扣。订阅滚窗、手动修复和过期清理接入相同互斥；待同步记录存在时管理员退款拒绝。详见 [持久结算契约](synchronous-settlements.md)。

普通 JSON 响应现在等待持久账单；PG 提交失败则返回既有 internal_error，不重新生成。SSE 完成标记等待结算，内容仍边生成边发送；数据库失败时追加错误事件。Realtime/Responses WS 活跃过程中逐帧用量持久化尚未补齐；本阶段不能保证在任意流中时刻强制杀进程也不丢实际用量。普通成功事件 balance_after 为空，表示同步后余额尚未知。旁路实时/软计数也尚未具备与财务 outbox 相同的恢复保证。

- `/tmp/okapi-durable-sync-check.log` 实际退出 **101**：测试容器的仓库只读挂载，SQLx 在线校验无法把元数据写入 /work/.sqlx。改为容器临时目录输出后，`...-check-r2.log` 全工作区/all-targets 实际退出 **0**，7 个新查询的离线元数据取回仓库，已有缓存未覆盖。
- 独立 PG 库 `okapi_durable_sync_20260927` 从上一隔离库复制，使用核实空闲的 Redis DB13，迁移仅在该测试库应用。保留此前原始故障库，没有操作开发数据库。供应商均为受控上游，未做真实云端费用验收。
- `/tmp/okapi-durable-sync-targeted.log` 实际退出 **0**，**19 passed、0 failed、0 ignored、0 软跳过**：HTTP 6、PG 记账/持久恢复 13。新增六项账本测试覆盖钱包/订阅、PG 回滚、Redis 关闭故障、丢失确认、Redis 数据丢失、保留其他预扣、12 路并发、金额/来源/key 冲突与错误池关闭前拒绝。旧成功 HTTP 测试改为由新 ledger 实例/worker 读取实际费用自动恢复，不再手工指定费用重试。
- 增加 JSON 响应必须等待 PG 提交、数据库拒写后返回 HTTP 500 且不重复调用供应商两个业务用例，`/tmp/okapi-durable-sync-protocols.log` 实际退出 **0**，**59 passed、0 failed、0 ignored、0 软跳过**，七个完整套件；包含普通聊天、Messages、Gemini、Responses、Realtime、故障注入 HTTP 与 PG 记账。与前面的 19 项不相加。
- 初次严格 Clippy `...-clippy.log` 实际退出 **101**，只发现管理员退款函数新增待同步检查后超行数；提取共享事务内检查，不修改资金断言或放宽警告标准。后续严格检查和最终相关回归结果继续记录。

- 第二次严格检查 `...-clippy-r2.log` 实际退出 **101**，发现 SSE 泵函数超行数和局部 use 位置；将结束帧发送与用量采集提取为小函数，将 Connection 导入移到模块顶部。`/tmp/okapi-durable-sync-clippy-final.log` 严格全工作区/all-targets/`-D warnings` 实际退出 **0**。格式、diff、计费浮点、错误码及 API 清单守卫全部通过；没有放宽检查标准。
- 新增真实 SSE 阻塞验证，检查内容能先发送，但数据库锁释放前不能发送 [DONE] 或关闭响应。它包含在随后完整运行中，不能计入此前 59 项。完整回归使用专用 ClickHouse `okapi-sync-ch-20260927` 和 NATS `okapi-sync-nats-20260927`，原隔离服务与数据仍保留；启动前拍下 1,022 个后端/Lua/迁移/查询元数据/测试文件指纹 `/tmp/okapi-durable-sync-full-source-manifest.json`。当前前端产物由原工作区提供，本任务没有编辑或构建前端。

- 首次完整 `/tmp/okapi-durable-sync-full.log` 实际退出 **101**。此前 45 个完整块有 263 项通过、无已报告的断言失败，但 `gateway_backlog` 块未结束，不能记为完整通过。源码检查与运行前 1,022 文件指纹完全一致。只读核对定位到旧夹具永久持有 settle_gate，却同步等待第一笔请求返回 200；新契约要求持久落盘，构成确定等待环。确认实际测试 PID 后仅终止该测试进程（SIGTERM），保留原始报告；不是因观察超时重启。
- 积压测试改为并发启动前两笔，先验证它们仍在等待；第三笔仍须 503、上游调用数仍为 2、余额不变、预扣集合不增加。释放写入闸后验证两笔响应成功且两笔账单都存在，再验证恢复后的第三笔与最终金额。HTTP 夹具增加十秒请求超时，避免未来断言前永久等待。原有积压上界、零上限配置及金额断言保留，没有修改生产契约来迁就旧测试。

- 响应时序定向 `/tmp/okapi-durable-sync-response-order.log` 实际退出 **101**，9 passed、1 failed。积压用例已通过；SSE 已通过“内容先出、完成帧等落盘”的断言，最终费用为 32 而旧预期为 24。只读 PG 核实为 prompt=10、completion=6：夹具的默认不可信渠道启用了本地输出复核，hello 的估算高于 mock 上报 2。将该时序用例的模拟渠道显式设为可信，并增加精确 `(10,2,24)` 断言；未修改生产复核规则。原不可信用量套件继续保留。
- `/tmp/okapi-durable-sync-clippy-order-final.log` 最终严格全工作区/all-targets 实际退出 **0**。与首轮完整运行相比，只有 gateway_backlog 和新增 SSE 支持测试两处源码变化，生产后端/Lua/迁移保持一致。新 1,022 文件指纹保存为 `/tmp/okapi-durable-sync-full-r2-source-manifest.json`，完整复跑结果随后记录，不把 263、19、59 或 9 项相加成单次通过数。


后续核对保留两项明确问题，不能用当前测试数量掩盖：一是自动重建与充值/管理员退款的并发时序尚需专项验收，相关入口并未全部使用同一用户锁；二是 `console::query::DEFAULT_LIMIT` 当前仍为 50，兑换码旧用例也按 50 验证，与用户此前要求的每页 20 条不一致。新模型目录默认 20 的通过结果不能证明其他管理列表已统一为 20。现有调用方分页、流中用量恢复和真实供应商联调等原有剩余项也继续保留。


最终完整复跑 `/tmp/okapi-durable-sync-full-r2.log` 实际退出 **0**，结构化报告 `...-full-r2-report.json` 为 **941 passed、0 failed、0 ignored、0 软跳过**；132 个完整套件/文档块，无解析错误或未完成块。原生 Batch HTTP 55、图片契约 44、普通故障/响应时序 HTTP 9、积压 1、PG 记账/持久恢复 13、账本重放/边界 23、长期冻结 52、Lua 契约 8 等均在本次运行中通过；domain 12、pricing 单元 20、对拍 5、性质 6 满足财务改动必跑要求。完整运行总数不叠加此前定向结果。

433 条 API 探针分别为 141 权限拒绝、173 匿名鉴权、82 HEAD 状态、25 公开契约、10 错误壳、1 CONNECT 传输拒绝、1 WS 方法检查，继续与成功业务验收区分。账单/门户/分析/统计、消息传输、正常下线、音视频、透传、各聊天协议和 worker 恢复也随整轮通过，但不证明未断言的竞争窗口或真实供应商行为。

`/tmp/okapi-durable-sync-full-r2-source-verification.json` 核对 1,022 文件无新增/删除/内容变化；前端未纳入该指纹，本任务未编辑或构建前端。最终格式、diff、金额与错误码守卫及 API 清单检查通过，严格全工作区/all-targets Clippy 结果仍对应同一后端源码。本轮未部署、未重启开发实例，也未标记整体目标完成；下一步继续验证资金重建与其他入账操作的并发，以及用户要求的管理列表默认 20 条。


## 资金操作与余额恢复的并发隔离（2026-09-27）

普通结算在 Redis 关闭确认丢失后会按 PG 事件恢复余额，但原充值/退款入口没有持有恢复使用的用户锁：退款先提交 PG，恢复可将其加回，随后 Redis credit 又加一次；充值先改 Redis，恢复也可能在事件提交前覆盖它。这是实际并发缺口，不能由此前 941 项完整回归推断已经安全。

- 失败复现 `/tmp/okapi-money-lock-red.log` 实际退出 101，两项 HTTP 回归均失败：恢复锁仍持有时，充值和退款已返回 200 并动账。测试通过 `pg_locks` 观察请求实际等待，而非仅凭固定睡眠假定请求启动。
- 新 `ledger::operations` 将管理员充值/退款、MCP 调整、兑换、支付入账/返利、注册赠送、单用户引导、两类迁移、余额到期清零接入同一用户锁；锁连接直接用于 PG 事务，兼容池大小为 1。运行源码已无绕过封装自行拼接 `credit` + `record_credit` 的入口。
- 充值先准备 PG 事件和用户快照，再修改 Redis，最后提交；退款仍以 PG 状态防重放，锁覆盖 PG 提交至 Redis 原池回补。迁移来源事件检查移到锁内；到期清零重新读取有效期并锁住用户行，避免等待期间被延期仍被清零。
- 新测试另发现不存在用户的充值会产生无归属余额：`billing_events` 没有用户外键，用户快照 UPDATE 影响 0 行被忽略。现明确检查必须更新一行；失败回滚事件、不动 Redis，并通过 API 返回 `404 not_found`。
- `/tmp/okapi-money-lock-ledger.log` 实际退出 101，新增测试漏闭括号、尚未执行；修正后 r2 实际退出 101、89 passed/1 failed，揭示上述不存在用户问题。r3 实际退出 101、89 passed/1 failed，后续 Redis 故障夹具误用了 `wallet` 字段；改用现有池字段 `avail` 后才真正注入故障。失败日志保留，没有减少故障断言。
- 最终 `/tmp/okapi-money-lock-ledger-r4.log` 实际退出 0：**144 passed、0 failed、0 ignored、0 软跳过**，包含 domain 12、pricing 31（含对拍与性质测试）、ledger 101。新增覆盖丢确认后的恢复再充值、单连接池、12 路重复退款/迁移、未知用户/Redis 明确拒绝不产生单边资金，以及到期等待期间延长有效期。

这是并发隔离修复，不是跨库原子事务。Redis 执行成功但确认丢失、Redis 与 PG 提交之间退出、退款提交后回补失败仍需对账/补偿；支付订单变 paid、兑换核销与赠送状态和入账分离的崩溃恢复仍须补齐。本轮没有修改、构建前端或部署服务。管理列表默认 20 条也尚未修正，旧兑换码分页 50 的测试通过不作为该要求的完成证据。

最终关联 API `/tmp/okapi-money-lock-api-r2.log` 实际退出 0，**48 passed、0 failed、0 ignored、0 软跳过**，11 个套件：注册认证、MCP、管理退款/充值、支付、兑换、订阅、用户管理、两类迁移及 worker。此前首轮 `/tmp/okapi-money-lock-api.log` 为 47 passed，新增未知用户接口断言后复跑为 48；两轮不相加，也不与领域/账本运行拼成一次完整工作区结果。严格全工作区/all-targets Clippy `/tmp/okapi-money-lock-clippy.log` 实际退出 0；格式、diff、无浮点、错误码与 API 清单守卫通过。最终 API/Clippy 验证期间 1,027 个后端文件无新增、删除或内容变化（`/tmp/okapi-money-lock-source-verification.json`），前端不在该指纹内且本轮未编辑或构建。整体对标目标保持未完成。


## 管理资源默认 20 条后端分页（2026-09-27）

用户此前要求兑换码和价格分组等页面后端分页，每页默认 20 条。实际源码 `PageQuery::DEFAULT_LIMIT=50`，配置类 `slice()` 仍传 `None`，只传 offset 也可能全量返回；`list_pagination` 甚至显式跳过了不带 limit 的上限检查。该例外现已删除，不能用旧测试的绿色结果证明用户要求完成。

- `/tmp/okapi-management-pages-red.log` 实际退出 101，0 passed/1 failed/3 filtered：兑换码默认返回 50 而非 20。
- `/tmp/okapi-management-pages-config-red.log` 实际退出 101，0 passed/1 failed/2 filtered：管理密钥和用户默认 50；池默认 84 条、offset=20 后 64 条；个人密钥默认 25。说明缺口不只是兑换码。
- `PageQuery::slice` 和 `bounded` 现在共用有界切片，默认 20、显式 limit 夹到 1～200、负 offset 按 0。12 类 HTTP 资源列表的 total、搜索/归属、越界空页保持正确；不改变内部 `Slice::ALL` 价簿/配置加载。
- 新矩阵为每类至少准备 205 条，在全新库也能覆盖 200 上限。独立 SQL 计数与 HTTP total 核对；同时验证默认/offset-only、非正或超大 limit、末页和两页拼接，门户密钥与团队只计当前用户。模型/渠道关键词、密钥用户过滤和独立兑换批次进一步验证筛选先于分页。
- 首轮关联 `/tmp/okapi-management-pages-related.log` 实际退出 101：37 passed/3 failed。三处是旧调用方假定第一页包含全部模型/角色/渠道，分别改用模型/渠道搜索以及按 total 逐页读取。删除角色的否定断言也改为查完整分页结果，避免“刚好不在第一页”造成假通过。没有恢复无界返回，也没有放宽业务内容断言。
- 修正后 `/tmp/okapi-management-pages-related-r2.log` 实际退出 0：**40 passed、0 failed、0 ignored、0 软跳过**，10 个套件。随后检查其他调用方，将测活/余额回显、渠道成本和规则停用回显的全量假设改为逐页读取；这些新增适配由完整工作区回归验证。严格全工作区/all-targets Clippy `/tmp/okapi-management-pages-clippy.log` 实际退出 0。

接口范围、调用方迁移和一致性限制见 [管理资源分页](management-pagination.md)。日志/审计/死信等使用独立游标和 limit 类型的入口不属于本次 12 类契约；不得把本阶段扩大为“所有分页接口均已统一”。本轮只修改后端、测试和文档，没有改/构建前端，没有部署。当前页面及下拉选择器的全量读取调用仍须适配后再做整站验收。

最终完整工作区 `/tmp/okapi-management-pages-full.log` 实际退出 **0**，报告 `...-full-report.json`：**950 passed、0 failed、0 ignored、0 软跳过**，132 个完整套件/文档块，无未结束块或解析错误。domain 12、pricing 31、ledger 101 随本轮再次通过；所有更新的分页调用方，包括测活、成本和规则回显，也在本轮通过。433 条 API 探针与业务测试分别统计，不把探针数当作成功业务 API 数量，也不拼接历史定向计数推导 950。

`/tmp/okapi-management-pages-full-source-verification.json` 确认 1,029 个后端源码/Lua/SQLx/迁移/测试/依赖文件无新增、删除或变化；严格 Clippy 已对应同一份源码通过。完整运行结束后的格式、diff、计费无浮点、错误码和 API 清单守卫均通过。前端未纳入源码指纹、也未被本轮编辑或构建，不能据此宣称整个前端已适配新默认分页。整体核心对标目标继续保持 active；下一步补资金业务状态与入账之间的故障恢复，不将本次列表修复当作全部目标完成。

## 钱包资金故障恢复（2026-09-27）

真实 HTTP 故障复现 `/tmp/okapi-fund-recovery-red.log` 实际退出 **101**，**0 passed、3 failed**：支付订单已 paid、兑换码已使用、账单已退款后，Redis 写入失败；重新创建账本连接并调用 worker，三者余额仍未恢复。测试在业务金额断言失败，没有用环境缺失作为跳过。之前完整 950 项是修改前基线，不能表示本阶段已经通过。

新增 `0017_fund_transfers`，业务状态/事件/钱包快照与入账意图同事务；Redis 同槽凭据防重复应用，并持久记录应用和清理的顺序。故障后的正常 HTTP 受理结果包含待入账标记，不伪造可用余额。细节、API 兼容性与明确未完成范围见 [钱包入账持久恢复](durable-fund-transfers.md)。本阶段只做后端。

第一次在线编译 `/tmp/okapi-fund-check.log` 退出 101：新增 console 事务调用缺少 sqlx→StoreError→AppError 转换。修正后 `...-check-2.log` 实际退出 0，新增 10 个 SQLx 离线元数据。业务回归与严格检查仍在进行，结果另行记录。

定向 API `/tmp/okapi-fund-api-green.log` 实际退出 0，**16 passed、0 failed、0 ignored、0 软跳过**。原三项故障现分别验证持久受理后由新账本连接/worker 自动恢复，重复回调/核销/退款不增加第二笔金额。该次运行尚不包含随后新增的三项 PG 意图写入拒绝用例。

计费红线要求的 `/tmp/okapi-fund-ledger.log` 实际退出 0，**150 passed**：domain 12、pricing 31（对拍 5/性质 6）、ledger 107；无失败/忽略/软跳过。六项新增底层测试验证 Redis 结果丢确认、PG 已确认后中断清理、钱包键丢失/历史事件清理、修复包含多笔待入账且保留活跃预扣、凭据冲突/大整数拒绝，以及订阅来源池退款。原 Redis 错误必须回滚的调账测试已明确调整为“PG 持久受理并待恢复”，并验证仅一笔事件和恢复后精确余额，没有把失败吞成无断言成功。

首次严格 Clippy `...-clippy.log` 退出 101：兑换函数新增事务超过行数限制。提取事务内钱包权益函数并修正新增测试的环境字段引用后，`/tmp/okapi-fund-clippy-2.log` 实际退出 0，workspace/all-targets `-D warnings` 通过。完整工作区运行前静态守卫通过，固定 1,046 个后端/Lua/SQLx/迁移/测试/依赖文件。

同时只读核对了下一项订阅恢复风险（尚未新增故障测试，不计为已复现/已修复）：`store::subscriptions::activate` 的 source 目前只是存储字段，没有唯一受理闸；重复 grant 同一来源会走续期，再延长时长。`ledger::subscriptions::fund_window` 仍先 sub_set 再写事件，grant/end/roll 的 PG 业务状态也先于该函数。因此后续不能仅给现有 grant 加一个重试循环，必须先完成来源幂等和窗口意图，再验证中断恢复。兑换批次的 per-IP 计数与 PG 回滚之间也有独立补偿缺口，当前新增 PG 回滚用例仅证明不带 IP 配额的兑换可重试，不扩大为所有配额场景。

完整回归运行期间独立复核发现新的时序缺口。`/tmp/okapi-fund-late-red.log` 实际退出 **1**：在独立 Redis 样本执行 500 入账，模拟后台已确认并清理 c:* 凭据，再让原超时 EVAL 迟到，余额从 9,500 再变成 10,000。样本已在 finally 清除，不改变工作区源码或其他测试样本。这证明仅靠可清理凭据不足以处理任意迟到命令，初版通过项不能作为最终防重结论。需在同一用户余额 hash 保留持久操作序号，并让修复包含已清理操作的序号。

初版完整工作区 `/tmp/okapi-fund-full.log` 实际退出 0，962 passed、0 failed、0 ignored、0 软跳过，433 条 API 探针另计；1,046 个后端文件运行期间未变。三项 PG 最终意图写入拒绝用例也通过，证明订单/核销/退款状态和资金记录同事务回滚。独立迟到请求仍能重复加钱，因此该轮不作为最终防重结论。

随后前滚 0018 操作序号；Redis fund_seq 与金额同一次 HSET 写入，旧序号迟到不重复追加。修复余额同时保留全部已接受操作（包括已清理操作）的最高序号。完整 bigint 使用十进制字符串比较。`/tmp/okapi-fund-late-green.log` 实际退出 0，余额 9,500 保持不变；在线 SQL 校验 `...-sequence-check.log` 退出 0，新增两项 SQLx 元数据。

序号阶段首次计费回归 `...-sequence-ledger.log` 实际退出 101，146 passed/6 failed：直接调用 Lua 的测试助手漏传新序号参数，脚本按契约返回 invalid。修正助手参数后 `...-sequence-ledger-2.log` 实际退出 0、152 passed，但报告标为 incomplete：Docker stdout/stderr 混合时 Doc-tests 标题提前出现在性质测试结果前（211–219 行），导致两个解析错误。没有放宽解析器；改为在容器内合并输出后再验证。严格 workspace/all-targets Clippy `...-sequence-clippy.log` 实际退出 0。

最终序号版计费回归 `/tmp/okapi-fund-sequence-ledger-3.log` 实际退出 **0**，**152 passed**（domain 12、pricing 31、ledger 109），容器内合并输出后报告完整，无失败/忽略/软跳过/解析错误。

最终完整工作区 `/tmp/okapi-fund-sequence-full.log` 实际退出 **0**，对应 `...-full-report.json`：**964 passed、0 failed、0 ignored、0 软跳过**，132 个完整套件/文档块，无未结束块或解析错误。433 条 API 探针独立统计，不计作额外业务测试。本阶段相对 950 基线新增 14 项真实故障/恢复测试，包含三项 Redis 故障 HTTP、三项 PG 事务回滚 HTTP、八项资金恢复与顺序检查。没有拼接不同运行推导 964。

`/tmp/okapi-fund-sequence-full-source-verification.json` 确認 1,049 个后端/Rust/Lua/SQLx/迁移/测试/依赖文件无新增、删除或内容变化；严格 Clippy 对应同一源码。结束后的格式、diff、金额与错误码守卫及 API 清单检查通过。0017/0018 只应用于隔离测试库，未修改开发数据库、未部署、未编辑或构建前端。

后续优先验证历史资金结转与订阅发放：源码确认 retention_months 非零会删除 billing_events 历史月分区，而通用对账和普通结算缺凭据恢复仍对现存事件求和；本阶段冷钱包读取快照的通过用例不能证明其他重建路径正确。订阅 source 尚未作为幂等受理键，套餐部分字段仍从可修改的 plans 联表读取，支付后的权益快照、重复发放和失败恢复须补验。其他已列核心差距仍保留；整体目标继续 active。

## 历史账本清理：资金结转与财务凭证

原实现按名称直接删除过期分区，余额重建仍只累加剩余事件。新的五项故障复现 `/tmp/okapi-retention-red-3.log` 实际退出 101、0 passed / 5 failed：钱包修复得到 −100,000 而非 8,900,000、历史退款 404、旧结算请求再次落账、迁移余额重复入账导致 4,000,000 而非 3,000,000，以及同名前缀的独立表被误删。此前两次 red 执行因测试 key_prefix 超长而失败，仅是夹具问题，不算业务故障证据。

修复新增 `0019_billing_retention.sql`。过期事件先按用户/池/actor/事件类型结转；过期账单保留唯一请求身份、状态、原支付池、四金额和计价依据，再在同一事务删除该详细分区。合并历史接入对账、两池余额重建、缺失结算凭据恢复、导入防重、累计邀请奖励、管理员退款及图片任务恢复。读取合并历史与清理之间使用共享/排他事务锁；清理只识别真实父表/OID/schema/实际月份边界一致的分区，不使用 CASCADE。

第一轮针对性行为回归 `/tmp/okapi-retention-green-2.log` 实际退出 0，12 passed、无失败或跳过，包含原五项、订阅池退款、冷余额/缺凭据恢复、失败 DROP 回滚、并发清理、读锁互斥、月边界和零净额导入标记。green-1 以及 check-2 的失败来自测试访问私有/不存在的 Redis 客户端字段，已修正为独立测试连接，不计入通过证据。另增加财务凭证冲突与超 bigint 生命周期累计边界两项，纳入最终完整回归。

本阶段只使用 `okapi_retention_20260927` 和逐测试新建的 `okapi_retention_case_*` 隔离 PG 数据库，配套独立 Redis/NATS/CH。旧阶段验证数据保留，开发服务和前端未改动、未部署。详细表结构、并发保证与范围见 [历史账本结转](billing-retention.md)。

最终完整工作区 `/tmp/okapi-retention-full.log` 实际退出 **0**，`...-full-report.json` 为 passed：**978 passed、0 failed、0 ignored、0 filtered、无软跳过**，133 个完整套件/文档块，无解析错误或未完成套件。新增两项凭证冲突与超 bigint 累计测试也在这一完整运行通过，worker_retention 共 14 项全部通过。domain 12、pricing 31、ledger 109 均包含在同一次运行，433 条 API 探针独立统计，不与业务测试数量相加。没有拼接不同运行推导 978。

严格 `/tmp/okapi-retention-clippy-final.log` 实际退出 0；格式、diff、无浮点/禁 panic 守卫、错误码文案与 API 清单检查通过。新增 21 份 SQLx 查询缓存，离线编译通过。`/tmp/okapi-retention-full-source-manifest.json` 与 `...-source-verification.json` 确认 1,074 个后端/Rust/Lua/迁移/查询缓存/依赖文件在完整测试期间无新增、删除或修改。

已有局限仍明确保留：不能恢复升级前已删历史；明细接口和 CH TTL 仍可能过期；大表清理的吞吐和锁竞争尚未验收；订阅发放的 source 幂等、权益快照与跨存储中断恢复仍需后续验证。源码再次确认订阅订单 paid 提交后才调用 grant，失败只记日志；同 source 续期没有防重，period/group 仍读取可修改套餐。下一阶段先复现这些接受与发放之间的故障，再补持久受理和恢复。整体对标目标保持 active。


## 订阅快照、受理与余额恢复（2026-09-27，阶段回归完成）

- 隔离环境：新库 `okapi_subscription_20260927`（共享测试 PG 集群 35432），独立 Redis 36381、NATS 34225、ClickHouse 38126。没有清理旧夹具、修改开发服务或构建前端。迁移 0020/0021 已在该测试库应用，开发/生产尚未部署。
- 原始失败复现 `/tmp/okapi-subscription-red.log` 实际退出 101：8 项新测试全部失败、6 个既有用例被筛选。暴露目录修改追溯改变条款、取消误收组、已支付/已核销权益遗失、发放事件写入失败、结束来源重放和管理员同键重复续期。这不是预期失败被计为通过。
- 快照阶段 `/tmp/okapi-subscription-snapshot-2.log` 实际退出 101：3 通过、5 失败；权益与分组修复有效，但受理后恢复仍缺失。随后新增持久发放队列、同源幂等、PG 状态/事件/待同步同事务和后台恢复，原 8 项在 `green-1` 中全部通过。
- `/tmp/okapi-subscription-api-3.log` 实际退出 0：26 通过、0 failed/ignored/filtered。包含原 6 项和新增 20 项：购买及兑换固定权益、8 个并发同键请求只受理一次、已禁用套餐的回执重放、已结束回执不显示新订阅余额、已付冲突权益可见并在旧订阅结束后补发、20 条后端游标分页/用户隔离、PG 接受/发放/取消/滚窗故障原子回滚、旧扫描不重复补满已用额度。
- `/tmp/okapi-subscription-money.log` 实际退出 0：154 passed、0 failed/ignored/filtered、无软跳过或解析错误；domain 12、pricing 31（5 对拍、6 性质）、ledger 111。新增迟到余额修复被拒且不覆盖新扣费，以及整个余额 hash 丢失后跨窗长期冻结仍保留的回归。不能把这次定向结果与上面的 HTTP 计数相加当作全量。
- 在线严格 Clippy 前两次实际退出 101，分别要求合并嵌套条件和移动局部 import；修正后 `/tmp/okapi-subscription-clippy-online-3.log` 全工作区/all-targets `-D warnings` 实际退出 0。生成并复制 35 个新增/变化 SQLx 元数据；格式、diff、禁浮点、错误码与路由清单守卫通过。
- 首轮完整回归启动前记录 1,115 个后端源文件/Lua/迁移/SQLx/配置指纹，失败结果与后续修正、最终复测见下文；不能据本阶段通过声称已全面对齐/超越竞品。

未解决/未证明：升级前被改写的套餐条款与旧 paid/已用来源的孤立权益不能自动重建；普通同步预扣缺少订阅窗口身份，跨窗退款规则与长期冻结尚未统一；套餐可售参数极值、续期与过期扫描并发还需专项核对。真实支付渠道联调、生产吞吐和长期运行仍未验证。保留整体核心对标目标，继续逐域测试，不因本阶段通过而宣称完成。


订阅阶段首轮全工作区 `/tmp/okapi-subscription-full.log` 已观察退出 101：**999 passed、1 failed、0 ignored、0 filtered、0 软跳过**，133 个完整块，无解析错误或未结束块，1,115 个后端文件指纹全程不变。唯一失败为 `console_mcp_write::mcp_write_full_scenario` 的审计排序断言。只读核查得到同一 actor 的三条记录 ID 351/352/353，target 依次 auth/routing/pricebook；对应时间为 22:36:38.747804、.771215、.654921（UTC），第三条墙上时钟回退约 116 毫秒。API 行为与逐次写入正常，按 created_at 排序无法证明串行请求顺序。测试改按递增审计 ID 读取，仍严格要求三个范围、相同顺序且每个恰好一次；生产接口未因此变更。失败日志、报告与指纹保留，定向及完整复测结果如下。


审计顺序修正后的 `/tmp/okapi-subscription-audit-order.log` 实际退出 0，MCP 完整写入场景 1/1 通过。增加对应查询离线缓存后，`/tmp/okapi-subscription-clippy-final.log` 在 `SQLX_OFFLINE=true` 下执行 `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 实际退出 0，格式及静态守卫通过。

最终 `/tmp/okapi-subscription-full-r2.log` 实际退出 **0**，结构化报告 `/tmp/okapi-subscription-full-r2-report.json`：**1,000 passed、0 failed、0 ignored、0 filtered、0 软跳过**，133 个完整套件/文档块，无解析错误或未结束块。domain 12、pricing 31、ledger 111 与订阅 HTTP 26 全部包含在这一次完整运行内；433 条权限/公开契约/错误探针另计，不等于 433 项成功业务 API。`/tmp/okapi-subscription-full-r2-source-manifest.json` 与 `-source-verification.json` 证实 1,116 个后端文件全程没有新增、删除或内容变化。此后只更新文档，未改后端源码或测试，未修改/构建前端、未部署。核心逐项对标目标继续保留。


## 订阅并发维护、恢复公平性与报价边界（2026-09-27，阶段回归完成）

- 原始 `/tmp/okapi-sub-lifecycle-red.log` 实际退出 101：7 项中 1 通过、6 失败。确认 HTTP 续期先获得用户锁成功后，旧到期扫描仍取消订阅；恢复列表 batch limit=1 会被坏用户反复占据；管理端可发布超额配额/不可表示的天数，旧错误条款仍可下单；零/负汇率仍生成订单；大额汇率乘法被 i64 饱和截断。
- 维护失败最初采用 Redis 坏值注入，这只造成已持久提交后的待同步，未阻止维护推进，故原用例通过。改用 PG `sub_reset` 事件写入失败复现真实事务失败，`/tmp/okapi-sub-lifecycle-due-red.log` 实际退出 101，1 项失败：单个坏用户中止整批维护。
- 修复后 `green-1` 实际退出 101：6 通过、1 失败。剩余大额测试的正确换算值超过现有 `NUMERIC(12,2)` 原币范围，数据库拒绝导致 500；未扩表或放宽范围。报价新增写入前范围校验，并分别验证超范围 400、仍在范围内但中间乘积超过 i64 的 9,800,000,000.00 报价，以及极小金额向上取整 0.01。
- 最终定向 `/tmp/okapi-sub-lifecycle-green-2.log` 实际退出 0：10 passed、0 failed/ignored、26 filtered。新例每个使用独立测试库和不重叠 Redis 用户 ID；并发场景等待真实 PG advisory 锁队列再释放，未靠任意延时猜测竞态。包含并发续期/过期、两 worker 滚窗只计一次、实例维护/未兑现发放/已兑现待同步三类队列的公平性、失败恢复不重复发放，以及套餐与报价边界。
- 迁移 0022 增加三处 nullable 重试时间，worker 对失败用户退避 60 秒并处理其他用户；状态变化清理相关退避。维护在持锁后重读有效期和窗口，按实际状态计 rolled/expired/failed。支付换算只在分层取整，不修改模型价格公式或账单四金额。
- `/tmp/okapi-sub-lifecycle-money.log` 实际退出 0：154 passed（domain 12、pricing 31、ledger 111），无失败、忽略、筛选或软跳过。`/tmp/okapi-sub-lifecycle-clippy-final.log` 最终离线 workspace/all-targets `-D warnings` 实际退出 0；本阶段增加 10 份 SQLx 离线缓存，格式/diff/禁浮点/错误码/路由清单守卫通过。
- 全量环境新建 `okapi_sub_lifecycle_20260927` 数据库、Redis 36382、NATS 34226、ClickHouse 38127；三个新服务读探针/就绪日志通过。旧开发与测试数据均保留。在线编译库预置 0022 后登记实际 SQLx SHA-384 校验和；新案例和全量新库由迁移器执行全部迁移。全量源指纹启动时记录 1,129 个后端文件，最终比对一致，结果见下文。

下一优先项是支付回调正确性，不能把本轮报价修正等同于回调校验完成。当前 `epay_callback` 只检验签名/成功字符串后按订单号发放，没有核对 pid、金额或订单网关；Stripe 只处理 completed，没有核对 payment_status/amount_total/currency，也未实施签名时间窗口。此处是源码发现，待新增有效签名负例和真实 HTTP 回归证明修复。

通过 agent-reach 的 Jina Reader 读取 [Stripe 官方履约文档](https://docs.stripe.com/checkout/fulfillment)：Checkout 完成可能仍在等待延迟支付，需要检查支付状态，并在异步成功事件后兑现；[Session 字段文档](https://docs.stripe.com/api/checkout/sessions/object)列出了金额、币种和 paid/unpaid/no_payment_required；[Webhook 官方文档](https://docs.stripe.com/webhooks)说明签名时间窗口和重发时重新签名。抓取存于 `/tmp/okapi-stripe-{fulfillment,session,webhooks}.md`，未使用真实支付账号或发起支付。普通同步预扣仍缺少周期身份、跨窗退款语义仍待处理，整体核心目标保持进行中。


本阶段最终完整运行 `/tmp/okapi-sub-lifecycle-full.log` 实际退出 **0**，报告 `/tmp/okapi-sub-lifecycle-full-report.json`：**1,010 passed、0 failed、0 ignored、0 filtered、0 软跳过**，133 个完整套件/文档块，解析错误/未结束块均为空。订阅 36、domain 12、pricing 31、ledger 111 全部包含在同一次运行内；433 条权限/公开契约/错误探针仍单列。`/tmp/okapi-sub-lifecycle-full-source-manifest.json` 与 `-source-verification.json` 确认 1,129 文件全程无新增、删除或内容变化。金额专项 `/tmp/okapi-sub-lifecycle-money-report.json` 154 项通过，最终离线严格 Clippy `/tmp/okapi-sub-lifecycle-clippy-final.log` 退出 0，静态守卫通过。此后只补文档，未改源码或测试、未改前端、未部署；下一优先阶段为支付回调契约和有效签名负例，整体目标继续。


## 支付回调契约与交易认领（专项通过，完整回归待运行）

新增有效签名 HTTP 负例的首轮 `/tmp/okapi-payment-validation-red.log` 实际退出 101，12 项全部复现：金额/商户/网关不匹配仍核销、交易号跨用户复用、已付订单替换交易号、Epay URL 编码与额外签名字段解析错误、Stripe 未支付提前发放、异步成功漏发、会话/金额/币种/状态未核对、旧签名重放、多 v1 轮换签名误拒绝。不是仅凭源码推断。

修复引入 `0023_payment_receipts.sql`：新订单保存商户与支付会话身份，网关/币种/报价在核销事务中核对，支付来源按网关、商户和交易号唯一认领。订单 paid、认领记录、钱包资金任务或订阅发放任务同一 PG 事务提交；资金任务失败会一起回滚，Redis 失败仍由已有 worker 恢复。报价变更不追改旧订单，订阅回调走相同核验。

Epay 先按表单协议 URL 解码全部字段，再排序验签；拒绝重复字段、无交易号、坏转义和非十进制金额，金额只按整数分精确比较。Stripe 验证原始请求体、唯一时间戳、五分钟时间窗和任一有效 v1；仅 paid 的 completed/async_payment_succeeded 可以发放，unpaid 不发放。Checkout 返回的会话与 HTTPS 支付 URL 校验通过并存下后才返回成功；回调早于会话持久化时返回 503 供网关重试。

旧订单 `payment_contract_version=0` 不伪造历史商户/Stripe session 快照。旧待付订单仍要求金额/币种/网关正确；历史已付交易会阻止新订单再次认领。旧 Stripe 无会话记录只能保持有限兼容，无法补回当时没存的身份绑定。商户账号或 webhook 密钥切换的历史凭证管理、支付退款/争议事件及真实商户联调尚未完成。

对照 [Stripe 官方履约](https://docs.stripe.com/checkout/fulfillment)、[Webhook 签名](https://docs.stripe.com/webhooks)、[Session 字段](https://docs.stripe.com/api/checkout/sessions/object)与[Epay 通知字段](https://www.ezfpy.cn/doc/result)。只使用本地模拟支付网关，没有发起真实付款。测试编译库在独立 `okapi_sub_lifecycle_20260927` 中原子应用 0023 并登记实际 SHA-384；后续全量会在新建 `okapi_payment_20260927` 通过正式内嵌 migrator 从空库迁移。

本阶段提交时的验证结果：

- `/tmp/okapi-payment-api-final.log` 实际退出 0，24 项支付 + 37 项订阅 HTTP 测试全部通过，0 failed/ignored/filtered；报告 `/tmp/okapi-payment-api-report.json`。
- `/tmp/okapi-payment-money.log` 实际退出 0，domain 12、pricing 31（含 5 对拍与 6 性质）、ledger 111，共 154 passed，0 failed/ignored/filtered；报告 `/tmp/okapi-payment-money-report.json`。
- 严格 Clippy workspace/all-targets 在线及离线检查均退出 0，格式、diff、计费红线、错误码和 API 路由清单守卫通过。新增 6 个 SQLx 查询快照。
- 支付改动后的完整工作区回归尚未运行；上文 1,010 passed 属于支付修复前的订阅阶段，不冒充当前版本全量结果。新环境数据库 `okapi_payment_20260927`、Redis 36383、NATS 34227、CH 38128 已就绪；本轮没有修改或构建前端，没有部署、重启开发服务或调用真实支付账号。
