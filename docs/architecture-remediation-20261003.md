# 架构审计 A01–A55 修复与验证（2026-10-03）

覆盖上一轮已确认的 55 项发现。保留原编号，修复、回归与验证限制逐项对应。55 项修复已完成，隔离回归和静态检查通过。

## 实现边界

推理主流程固定为凭证解析、公共准入、注册传输、协议转换和结算。Transport/Factory 与账号 Hook 分离；订阅行为保持在插件内。协议默认值由 `okapi-api::provider_contract` 目录统一持有；扩展原生协议能力时添加目录元数据和 transport 注册，不修改存储查询或网关 dispatch。

并发限制使用每个渠道 Key 的唯一租约，适用于普通 Key 和订阅。长流通过响应所有权保留许可，取消/EOF/错误会释放；续租失败终止继续使用该许可的请求。账本到期、预扣、恢复共用用户锁。

## 验证方式

所有服务端回归在一次性 PostgreSQL、Redis、ClickHouse 24.8 和 NATS JetStream 中执行。测试不会载入开发 `.env`，不启动业务 worker，不读取真实账号 token。默认 UI 回归使用构建产物与拦截接口；显式运行真实 API E2E 必须提供隔离资源。

```sh
python3 scripts/test-isolated.py
SQLX_OFFLINE=true cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
pnpm --dir frontend test:interactions
pnpm --dir frontend lint
bash scripts/guard-no-float.sh
bash scripts/guard-i18n.sh
python3 scripts/guard-i18n-keys.py
python3 scripts/guard-frontend-permissions.py
python3 scripts/guard-error-codes.py
python3 scripts/guard-deploy-manifests.py
```

隔离 runner 在每个 Rust 测试 executable 前创建独立数据库并清空自有旁路存储。单套件内部串行运行；并发竞争测试在用例内部显式派发并发任务。`--match audit_regressions` 和 `--match channel_controls` 可单独运行本轮重点回归。CI 的 `--services` 仅用于专用服务，并要求隔离标记及测试库名称。大套件有 1800 秒有界截止（`--timeout-secs` 可调整）。本机最终完整回归划为 `--shard 0/4` 至 `--shard 3/4` 四个互斥分区，各分区拥有独立的四种旁路服务；分区合并覆盖完整 executable 清单，doctest 由第 0 分区执行。

## 最终验证结果

- 后端完整回归：1700 通过、0 失败、3 个真实账号/本机 CLI 的显式 opt-in 用例未执行。四个分区均以 exit 0 完成，162 个测试 executable 各运行一次，doctest 由第 0 分区执行；无隐式跳过信号和未结束套件。
- 最终专项回归：providers 8/8、pricing 2/2、网关/存储 19/19、channel_controls 15/15。19 项版本包含完整回归构建后新增的 A44 实际 HTTP + ClickHouse 回归；其它专项用例与完整回归有重叠，不能相加视为独立覆盖数。
- 前端完整交互回归：673/673；构建、TypeScript、oxlint 通过。
- `cargo fmt`、全目标严格 Clippy、SQLx 在线元数据核对、计费/翻译/权限/错误码/部署守卫、API 清单与测试报告解析检查通过。
- [逐项状态与机器可读验证结果](architecture-remediation-results-20261003.json) 保留分区计数、日志摘要和 SHA-256。业务数据库和真实账号未用于本次测试。

## 逐项清单

| ID | 问题 / 修复 | 实现 | 验证 |
|---|---|---|---|
| A01 | **任务退出计数**：PendingTask 在 Drop 中退计数，panic/取消也可停机 | [bins/okapi/src/shutdown.rs](../bins/okapi/src/shutdown.rs) | shutdown 单测：panic_does_not_block_shutdown / cancelling_an_unpolled_task_drains_its_counter |
| A02 | **候选排序与复制**：共享 Arc 候选快照、只排列索引；加权无放回排序改为 O(n log n)，时延批量读取 | [bins/okapi/src/gateway/scheduler.rs](../bins/okapi/src/gateway/scheduler.rs) | scheduler 排序/权重/借用单测；release CPU 基准 |
| A03 | **未到期 OAuth 热路径**：使用缓存渠道策略，未到期凭证直接返回，避免每请求重读 PG | [bins/okapi/src/gateway/credentials/oauth.rs](../bins/okapi/src/gateway/credentials/oauth.rs) | channel_controls::fresh_managed_oauth_uses_the_cached_policy_without_reading_postgres |
| A04 | **负数按次价格**：价簿编译边界拒绝负数按次价格 | [crates/okapi-pricing/src/model.rs](../crates/okapi-pricing/src/model.rs) | okapi-pricing/tests/audit_regressions::negative_fixed_price_is_rejected_by_library_compilation |
| A05 | **音频价格乘法溢出**：使用 checked 乘法，将溢出转为定价错误 | [crates/okapi-pricing/src/engine.rs](../crates/okapi-pricing/src/engine.rs) | okapi-pricing/tests/audit_regressions::extreme_audio_ratio_returns_error_without_panicking |
| A06 | **推理适配器扩展**：Transport/Factory 注册表封装传输；网关固定执行链，协议默认值由共享目录持有 | [crates/okapi-providers/src/inference.rs](../crates/okapi-providers/src/inference.rs) | okapi-providers/tests/audit_regressions::new_inference_transport_requires_only_a_registration；registry 协议合同单测；各网关协议套件 |
| A07 | **分析页循环依赖**：将路由状态与解析迁出路由文件，视图只依赖功能层模块 | [frontend/src/features/analytics/route-state.ts](../frontend/src/features/analytics/route-state.ts) | 前端构建、类型检查和 charts / analytics 交互 |
| A08 | **负值流向无限加载**：退款负值保留明细并显示说明，成功但为空的图显示空态 | [frontend/src/features/analytics/FlowView.tsx](../frontend/src/features/analytics/FlowView.tsx) | frontend/e2e/audit-regressions.spec.ts A08 |
| A09 | **余额到期与无限额 Key 竞争**：无限额预扣也经过同一用户锁及持久同步栅栏 | [crates/okapi-ledger/src/key_budget.rs](../crates/okapi-ledger/src/key_budget.rs) | audit_regressions::unlimited_key_admission_waits_for_balance_expiry_fence |
| A10 | **用户锁连接反复关闭**：先武装 close_on_drop，正常释放验证解锁/重置再归还；取消仍关闭锁连接 | [crates/okapi-ledger/src/holds.rs](../crates/okapi-ledger/src/holds.rs) | audit_regressions::user_guard_reuses_unlocked_connection_and_cancellation_releases_lock |
| A11 | **恢复被单个用户锁阻塞**：逐用户隔离恢复错误，遇到竞争继续处理其它用户 | [crates/okapi-ledger/src/sync.rs](../crates/okapi-ledger/src/sync.rs) | audit_regressions::locked_user_does_not_starve_other_pending_settlements |
| A12 | **账本测试共享渠道计数**：每个测试使用独立渠道标识，避免并行污染生命周期累计 | [crates/okapi-ledger/tests/pg_settlement.rs](../crates/okapi-ledger/tests/pg_settlement.rs) | pg_settlement 31 项及隔离全量回归 |
| A13 | **请求体重复深复制**：JSON 直接传 Bytes，multipart 使用流与长度，共享底层字节 | [crates/okapi-providers/src/openai.rs](../crates/okapi-providers/src/openai.rs) | 源码检查；gateway_audio / gateway_images / gateway_responses / gateway_usage_modalities |
| A14 | **上游错误元数据不一致**：错误体有界读取；读取失败仍保留 HTTP 状态与 Retry-After，包含 API Key/订阅计数接口 | [crates/okapi-providers/src/anthropic.rs](../crates/okapi-providers/src/anthropic.rs) | okapi-providers/tests/audit_regressions：truncated error / count_tokens；gateway_audio / multipart |
| A15 | **Bedrock 截断帧静默结束**：EOF 校验解码缓存，残帧明确报错 | [crates/okapi-providers/src/aws_eventstream.rs](../crates/okapi-providers/src/aws_eventstream.rs) | okapi-providers/tests/audit_regressions::bedrock_partial_frame_at_eof_is_an_error |
| A16 | **媒体遗漏渠道并发控制**：共享 execute 获取 ChannelPermit；流响应持有许可到 EOF/drop，Realtime/WS 也接入 | [bins/okapi/src/gateway/account_control/mod.rs](../bins/okapi/src/gateway/account_control/mod.rs) | audit_regressions::media_routers_share_the_channel_concurrency_fence_and_refund_denied_admission；channel_controls shared-wire test；Realtime / WS 套件 |
| A17 | **撤销后旧鉴权快照复活**：PG 读取前捕获版本，写缓存必须仍是该版本；旧缓存格式失效 | [bins/okapi/src/gateway/auth.rs](../bins/okapi/src/gateway/auth.rs) | audit_regressions::late_auth_snapshot_cannot_restore_revoked_key |
| A18 | **并发 TTL 与旧许可误释放**：唯一成员租约、Redis 时钟、定时续租；只释放自己的成员；租约丢失终止流 | [bins/okapi/src/gateway/sched_redis/channel_permit.rs](../bins/okapi/src/gateway/sched_redis/channel_permit.rs) | channel_permit 单测：renew / stale release / EOF/drop；共享入口回归 |
| A19 | **匿名请求先分词且预算按片段**：四种聊天入口先鉴权；128 KiB 精确分词预算按整请求分配 | [bins/okapi/src/gateway/estimate.rs](../bins/okapi/src/gateway/estimate.rs) | audit_regressions::all_chat_ingresses_authenticate_before_parsing_or_estimating_body；estimate 单测；源码预算检查 |
| A20 | **并行工具调用串扰**：按 tool index 保存独立 Anthropic 内容块及参数增量 | [crates/okapi-providers/src/convert/anthropic_to_openai.rs](../crates/okapi-providers/src/convert/anthropic_to_openai.rs) | okapi-providers/tests/audit_regressions::parallel_tool_arguments_keep_distinct_anthropic_blocks |
| A21 | **Responses 终态与完成事件**：length/content_filter 保留 incomplete；补齐文本、内容块和输出项完成事件 | [crates/okapi-providers/src/convert/responses_to_chat.rs](../crates/okapi-providers/src/convert/responses_to_chat.rs) | okapi-providers/tests/audit_regressions::responses_preserve_length_and_filter_termination_in_json_and_sse；gateway_responses |
| A22 | **工具索引无界扩容**：校验工具调用索引小于 128，恶意值在分配前拒绝 | [crates/okapi-providers/src/convert/gemini_to_openai.rs](../crates/okapi-providers/src/convert/gemini_to_openai.rs) | okapi-providers/tests/audit_regressions::hostile_tool_indices_fail_before_unbounded_growth |
| A23 | **JetStream 串行双确认**：PG 持久化后最多 32 并行确认，单笔 5 秒、整批 10 秒截止；回放靠收据幂等 | [bins/okapi/src/worker/nats_relay.rs](../bins/okapi/src/worker/nats_relay.rs) | acknowledgement_tests::stalled_acknowledgements_have_a_batch_deadline；nats_relay 套件 |
| A24 | **切换账号残留前账号缓存**：统一 auth reset，清查询缓存/取消任务，旧 epoch 响应拒绝入新上下文 | [frontend/src/lib/api.ts](../frontend/src/lib/api.ts) | frontend/e2e/audit-regressions.spec.ts A24；auth-routing / profile 交互 |
| A25 | **OAuth 首次登录孤儿用户**：identity 竞争失败回滚整笔临时用户事务，再读取赢家 | [crates/okapi-store/src/identity.rs](../crates/okapi-store/src/identity.rs) | audit_regressions::concurrent_oauth_first_login_creates_no_orphan_users |
| A26 | **密封覆盖新轮换凭证**：按 id 与原字节 CAS 更新；仅成功更新计数 | [crates/okapi-store/src/credential.rs](../crates/okapi-store/src/credential.rs) | audit_regressions::credential_sealing_preserves_a_concurrent_rotation |
| A27 | **开发重置遗留 NATS 历史**：重置 BILLING 流及消费状态，保留其它流；活进程下拒绝重置 | [scripts/nats-reset-stream.py](../scripts/nats-reset-stream.py) | audit_regressions::development_reset_removes_billing_history_and_preserves_unrelated_streams |
| A28 | **worker 重复数据库资源图**：注入已经创建的 PG/Redis 资源；不再构造第二套连接池 | [bins/okapi/src/worker/mod.rs](../bins/okapi/src/worker/mod.rs) | 源码资源路径检查；guard-deploy-manifests.py；worker / nats / reconcile 套件 |
| A29 | **Realtime 错误发送绕过截止**：所有错误发送和 close 使用有界 deadline，避免结算无限等待客户端 | [bins/okapi/src/gateway/realtime.rs](../bins/okapi/src/gateway/realtime.rs) | gateway_realtime / gateway_responses_ws 截止与退款套件；错误路径源码检查 |
| A30 | **视频数量与计费不一致**：统一校验/规范化 seconds：缺省 4，显式整数 1..60，非法输入在预扣前拒绝 | [bins/okapi/src/gateway/videos.rs](../bins/okapi/src/gateway/videos.rs) | 视频 seconds 边界单测；gateway_videos 计费及失败退款 |
| A31 | **PgBouncer 锁语义不兼容**：部署模板与文档统一要求 session pooling 或直连，移除 transaction pooling 推荐 | [deploy/docker-compose.yml](../deploy/docker-compose.yml) | 部署模板静态检查；实际 PG 用户锁竞争/取消测试；未部署 PgBouncer 实例 |
| A32 | **SQL 日历重写损坏字面量**：词法保护单引号/双引号/反引号/注释等非代码段，只重写 SQL 代码 | [crates/okapi-store/src/ch/calendar/lexical.rs](../crates/okapi-store/src/ch/calendar/lexical.rs) | audit_regressions::calendar_rewrites_leave_literals_and_identifiers_unchanged（实际 CH）；lexical 单测 |
| A33 | **管理面响应无限读取/吞错**：共享有界 collector，传输错误传播；OAuth/Turnstile/模型/余额明确设置容量 | [crates/okapi-providers/src/limits.rs](../crates/okapi-providers/src/limits.rs) | okapi-providers/tests/audit_regressions::bounded_collector_propagates_middle_stream_errors；console OAuth / balance / manage |
| A34 | **Path 参数错误返回纯文本**：统一 JSON Path 提取器，类型错误遵循 AppError 合同 | [bins/okapi/src/gateway/extract.rs](../bins/okapi/src/gateway/extract.rs) | route_error_envelope：匿名和普通用户逐路由检查 |
| A35 | **配置读取失败开放注册**：读取失败不写缺失缓存；注册策略读取/解析失败明确拒绝 | [bins/okapi/src/console/registration.rs](../bins/okapi/src/console/registration.rs) | audit_regressions::database_failure_does_not_cache_an_open_registration_policy |
| A36 | **会话索引与撤销非原子**：Lua 原子维护映射、索引、元数据；索引错误不能产生有效孤儿会话；三者 TTL 同步滑动 | [bins/okapi/src/gateway/sched_redis.rs](../bins/okapi/src/gateway/sched_redis.rs) | audit_regressions::sessions_reject_orphans_slide_all_ttls_and_revoke_atomically |
| A37 | **未校验 UTF-8 字节切片**：API 版本先校验 ASCII，金额小数先校验数字再截取 | [bins/okapi/src/console/channel_balance.rs](../bins/okapi/src/console/channel_balance.rs) | API version / decimal parser 非 ASCII 边界单测 |
| A38 | **成员替换并发产生并集**：替换前锁渠道/用户父行，空成员集也可串行化 | [crates/okapi-store/src/admin.rs](../crates/okapi-store/src/admin.rs) | audit_regressions::replacing_empty_membership_is_serialized_on_the_parent |
| A39 | **复制渠道丢失账号与池元数据**：保留 credential_kind 与池 priority/weight 覆盖 | [crates/okapi-store/src/mutate.rs](../crates/okapi-store/src/mutate.rs) | audit_regressions::duplicate_preserves_oauth_kind_and_pool_overrides |
| A40 | **MCP 封禁绕过超管保护**：HTTP/MCP 复用 actor-aware 管理策略，并保留存储层条件保护 | [bins/okapi/src/console/user_management.rs](../bins/okapi/src/console/user_management.rs) | audit_regressions::http_and_mcp_share_protected_user_policy |
| A41 | **路由隐私约束悄然降级**：路由指令解析失败返回 400；JSON 转义键按同一解析结果识别/移除 | [bins/okapi/src/gateway/routing_prefs.rs](../bins/okapi/src/gateway/routing_prefs.rs) | routing_prefs 单测；gateway_routing_prefs |
| A42 | **团队身份与钱包所有人混淆**：分离 actor/wallet 身份；验证成员存在/状态；个人 Key 操作仅授权属主 | [crates/okapi-store/src/auth.rs](../crates/okapi-store/src/auth.rs) | audit_regressions::team_members_cannot_delete_peer_keys_and_banned_members_cannot_authenticate；console_teams |
| A43 | **分析查询重复维护观测字段**：共享 Source/Kind/Grain 目录与字段归属，保留各来源独立恢复语义 | [bins/okapi/src/console/observation_sources.rs](../bins/okapi/src/console/observation_sources.rs) | 目录唯一性/完整性单测；console_analytics / console_stats / 观测来源套件 |
| A44 | **堆叠趋势丢失成本覆盖分母**：统一加法指标目录，financial_records 在分桶/折叠/合计中守恒 | [bins/okapi/src/console/analytics.rs](../bins/okapi/src/console/analytics.rs) | audit_regressions::stacked_cost_buckets_preserve_financial_records_coverage_and_margin（实际 HTTP + CH）；console_analytics / 成本与退款统计套件 |
| A45 | **毛利熔断覆盖人工解除**：评估与清理使用 Redis CAS，过期判断以当前条目为准 | [bins/okapi/src/margin.rs](../bins/okapi/src/margin.rs) | audit_regressions::old_margin_evaluation_cannot_overwrite_manual_lift |
| A46 | **定价批量写入部分成功**：先校验完整批次，排序锁模型，价格与审计在同一事务提交 | [bins/okapi/src/console/ratio_sync.rs](../bins/okapi/src/console/ratio_sync.rs) | audit_regressions::failed_pricing_batch_rolls_back_every_model |
| A47 | **模态键盘误确认/关错层**：共享栈管理顶层 Escape、滚动锁与焦点；取消按钮遵循原生 Enter，排除 IME | [frontend/src/hooks/use-modal-focus.ts](../frontend/src/hooks/use-modal-focus.ts) | write-forms 嵌套确认 Cancel Enter/Escape；security-layout 移动端弹层 |
| A48 | **非法并发输入清空上限**：共享正整数解析器，区分无修改、无限制和非法值，非法状态禁保存 | [frontend/src/features/channels/KeyParamRow.tsx](../frontend/src/features/channels/KeyParamRow.tsx) | channel-controls / write-forms 并发参数交互 |
| A49 | **可编辑名称作 React Key**：自定义 Header 与注入字段编辑使用稳定行标识，键入不重挂载 | [frontend/src/features/channels/ChannelDrawer.tsx](../frontend/src/features/channels/ChannelDrawer.tsx) | write-forms 连续输入并验证焦点保留 |
| A50 | **编辑规则丢失状态/有效期**：完整替换表单保留 enabled/valid_from/valid_to | [frontend/src/features/rules/RuleDrawer.tsx](../frontend/src/features/rules/RuleDrawer.tsx) | frontend/e2e/audit-regressions.spec.ts A50 |
| A51 | **退款预览与提交目标错配**：只接受当前查询身份的回包；确认操作捕获不可变 request_id 与 reason | [frontend/src/features/ops/RefundCard.tsx](../frontend/src/features/ops/RefundCard.tsx) | frontend/e2e/audit-regressions.spec.ts A51 |
| A52 | **缓存趋势把未知稀释成零**：使用配对已观测请求口径聚合；未知显示空隙，部分覆盖明确说明 | [frontend/src/features/portal-overview/cache-metrics.ts](../frontend/src/features/portal-overview/cache-metrics.ts) | frontend/e2e/audit-regressions.spec.ts A52；charts 缓存图 |
| A53 | **日志时间范围丢秒/毫秒**：输入保留秒和毫秒，深链接到查询保持原始时刻 | [frontend/src/routes/admin.logs.tsx](../frontend/src/routes/admin.logs.tsx) | frontend/e2e/audit-regressions.spec.ts A53 |
| A54 | **E2E 复用业务服务**：默认仅使用独立前端预览与接口桩，集成配置必须显式隔离且禁止复用服务器 | [frontend/playwright.config.ts](../frontend/playwright.config.ts) | 完整前端交互回归；integration 配置检查 |
| A55 | **集成测试污染开发全局配置**：禁止 dotenv 默认回落；每套件独立 PG、清理自有 Redis/CH/NATS；CI 使用同一隔离 runner | [scripts/test-isolated.py](../scripts/test-isolated.py) | test_support::fixtures_reject_developer_stores_and_missing_opt_in；隔离完整后端回归 |

## 性能与验证限制

相同机器、release、CPU 微基准（7 次中位数）：2048 候选排序从 2763 µs 降至 58.88 µs；8192 候选从 45323 µs 降至 255.08 µs。新路径仅排序借用快照的索引，旧路径还需要约 892 µs 的候选深复制（8192）。该结果只描述排序层，不代表生产端到端吞吐。

PgBouncer 修复是部署合同与模板修正，未启动 PgBouncer 做故障注入。真实 Claude/Codex 订阅和本机 CLI 的 opt-in 用例不在这次回归中执行；本轮网络调用只访问本地 synthetic mock。

本轮新增的许可、锁、CAS 和会话测试覆盖了已复现的竞争场景；这些结果不等于所有生产并发时序的形式化证明。

## 可见行为变化

- 非法视频秒数在预扣前拒绝，传给上游的有效数量与报价数量保持一致。
- 格式不正确的路由限制明确返回错误，避免隐私与禁止回退约束被静默取消。
- 切换账号会清空前账号私有缓存，旧请求不能把结果写入新登录上下文。
- PG 注册策略读取/解析失败时暂停注册，恢复后重新读取有效策略。
- 直接运行共享开发库的集成测试会被拒绝；请使用隔离 runner。
