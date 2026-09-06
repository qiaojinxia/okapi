# Okapi 验证清单（模块 × 维度）与执行记录

首版：2026-09-06 ｜ 范围：全部 6 crates + `bins/okapi` 三角色 + `frontend/` ｜ 口径：以**现有自动化用例**为准逐模块登记覆盖了哪些维度，没有用例的维度直接标缺口，不以文档自述代替。

用途：发布前 / 大改后按第 1 节分层跑完，把结果追加到第 4 节；第 3 节的缺口是下一轮补测试的待办。

## 0. 维度定义

| 代号 | 维度 | 判定依据 |
| --- | --- | --- |
| A | 功能正确性 | 端点 / 函数按设计输入输出；错误只回 `error_code`（IMPLEMENTATION §8） |
| B | 计费一致性 | 金额 micro-USD 整数、pricing_snapshot 可复算、预扣 / 结算 / 退款闭合（DESIGN §3） |
| C | 安全 | 鉴权、RBAC、限流、IP 白名单 / 信任闸、SSRF、凭证加密、审计留痕 |
| D | 可靠性 | failover / 重试矩阵 / key 状态机 / 对账修复 / DLQ / 多副本一致性 |
| E | 协议兼容 | 与 OpenAI / Anthropic / Gemini / Azure 方言的双向转换 parity，错误壳形状 |
| F | 前端交互 | 页面可用、i18n 双语、键盘 / 移动端 / 深色、权限裁剪、不伪装零数据 |
| G | 性能 | `docs/perf-report.md` 口径（RPS / P99 / SSE 持有），非每轮回归 |
| H | 迁移与 schema | 迁移只前滚、schema 形状守卫、外部库导入全量校验 |
| I | 部署形态 | `okapi all` 单机、embed-web、compose / k8s 模板 |

## 1. 分层执行入口

前置：`scripts/dev-deps.sh up`（PG / Redis / NATS / CH 四容器）+ `.env`。所有命令在仓库根执行。

| 层 | 内容 | 命令 | 依赖 |
| --- | --- | --- | --- |
| L0 静态守卫 | rustfmt、clippy `-D warnings`（含测试目标）、sqlx 离线快照完整、cargo-deny（advisories / bans / licenses / sources）、前端 tsc / oxlint、四道守卫（浮点 / i18n 裸文案 / i18n 键对齐 / 前端权限点） | `cargo fmt --all -- --check` · `SQLX_OFFLINE=true cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny check` · `cd frontend && pnpm exec tsc -b && pnpm exec oxlint` · `bash scripts/guard-no-float.sh && bash scripts/guard-i18n.sh && python3 scripts/guard-i18n-keys.py && python3 scripts/guard-frontend-permissions.py` | 无 |
| L1 单元 / 性质 | crate 内 `#[cfg(test)]` 与 `crates/*/tests`（pricing 对拍 + proptest、providers 转换 parity、ledger Lua 契约） | `cargo test -p okapi-domain -p okapi-pricing -p okapi-ledger -p okapi-providers -p okapi-api -p okapi-store` | ledger 契约需 Redis；store 部分用例需 PG |
| L2 集成 | `bins/okapi/tests/*.rs` 71 个套件（gateway / console / worker / migrate） | `cargo test --workspace --no-fail-fast` | 四容器；CH / NATS 缺失时相关套件自跳过 |
| L3 前端交互 e2e | 构建产物 + 接口桩，不碰数据库 | `cd frontend && pnpm test:interactions`（= `pnpm build` + `playwright test -c playwright.interactions.config.ts`） | 无（自起 vite preview :4175） |
| L4 前端冒烟 e2e | 打真实 console（API + SPA 同源），注册真实用户；管理端用例需演示超管（`scripts/dev-reset.sh` 灌注），缺则跳过 | `CARGO_TARGET_DIR=target cargo build --bin okapi && cd frontend && pnpm build && pnpm exec playwright test -c playwright.config.ts smoke.spec.ts` | 四容器 + `target/debug/okapi` + `frontend/dist` |
| L5 单机形态冒烟 | `okapi all` 一进程三角色：双 healthz、root key 引导、SPA 可达、数据面 fail-closed | `bash scripts/smoke-all.sh` | 四容器；占用 :8080 / :8081 |
| L6 性能 | loadgen 缩尺 / Linux 容器复测 | `cargo run --release --example loadgen -- ...`、`scripts/linux-bench.sh`（见 `docs/perf-report.md`） | 独占机器，按需 |

注意事项：
- 本机 Cursor 沙箱会把 `CARGO_TARGET_DIR` 重定向到缓存目录，L4 / L5 依赖的 `target/debug/okapi` 必须显式 `CARGO_TARGET_DIR=target` 构建，否则跑的是旧二进制。
- `.sqlx` 快照必须用 `cargo sqlx prepare --workspace -- --all-targets` 生成；不带 `--all-targets` 会漏掉测试里的 `query!`，CI 离线编译即红。
- L4 与 L5 都占 :8081，顺序执行；L4 的 Playwright `webServer` 在 `reuseExistingServer: true` 下会复用已在跑的 console。

## 2. 覆盖矩阵

「维度」列只登记**有自动化用例**的维度；括号内为主要套件（`bins/okapi/tests/<name>.rs` 省略路径）。

### 2.1 领域 crates

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `okapi-domain` | Money newtype、ID newtype、计费状态机、token 计数 | A B | 单元：`money.rs` / `state.rs` / `tokens.rs`；守卫：`guard-no-float.sh` | 状态机转移矩阵穷举在 `state.rs` 单测 |
| `okapi-pricing` | PriceBook 编译、三层倍率 / 按次 / 阶梯 / 缓存双轴、规则栈、快照 | A B | `parity.rs`（new-api 对拍 fixtures）、`prop.rs`（proptest）、单元 `book/engine/handle/model/ratio/rules`；集成 `gateway_m1`（cache ratio）、`gateway_tier`、`gateway_pricing_rules`、`gateway_model_modifiers`、`console_pricing_write`、`console_import` | — |
| `okapi-ledger` | Redis Lua reserve / commit / refund / repair / sub_set + PG 同事务记账 + outbox | A B D | crate 级 `tests/lua_contract.rs`（09-06 新增，7 例）：预扣字段四段 / 多退少补 / 重复 commit 与 refund 任意顺序幂等 / `avail == est` 放行、`avail < est` 拒绝且零写入 / 四个 key 级限额各自 which 且拒绝零写入、并发槽随结算释放 / repair 绕开在途、不动另一池、负目标不夹逼 / drain 只取正余额 / 13 步交错序列逐步验证 `avail + Σ在途 == 入账 − Σ实际`；订阅池选池由 `console_subscriptions::lua_pool_contract` 覆盖；crate 级 `tests/pg_settlement.rs`（09-06 第四轮，5 例）：records / events / users 快照 / api_keys 用量 / outbox 五处同事务且四金额列与 pool 三处一致（含 INET 列真落）、第二条语句失败整体回滚、订阅池结算与订阅事件不动钱包快照、失败请求零金额落 error_code、`admin_refund` 只对 committed 生效一次并逐项回冲；集成：`gateway_m1`、`worker_m2`、`worker_reconcile_repair`、`console_ops`、`console_teams`、`gateway_realtime`；第五轮补 `replaying_a_settled_request_writes_nothing`（重放 request_id 五处零写入） | — |
| `okapi-providers` | openai / anthropic / gemini / azure / custom_pass / responses 客户端，`convert/*` 按方向转换，modifiers / reasoning，`http.rs` 代理与额外头 | A E | `convert_a2o` / `convert_anthropic` / `convert_gemini` / `reasoning_t2c` / `stream_usage`；单元 `azure` / `gemini_to_openai` / `http` / `modifiers` / `reasoning` / `responses`；集成见 2.2 chat 行 | `gemini_to_openai` 只有单元 + `gateway_gemini_ingress` 集成，无独立 parity fixture 文件 |
| `okapi-store` | sqlx 查询、迁移、凭证信封 AES-GCM、身份（argon2 / bcrypt 双轨）、分页、CIDR 匹配、CH schema | A C H | 编译期：`.sqlx` 离线校验全部 `query!`；`schema_shape`（迁移形状守卫）、`channel_credential`（密文落库 / 无主密钥 fail-closed）、`console_manage::price_group_pagination_matches_database_pages`、`gateway_ip_allowlist`（netmatch）、`worker_ch`（CH 表与 MV）；单元 `credential` / `identity` / `listing` / `mutate` / `netmatch` / `subscriptions` / `vendor` | — |
| `okapi-api` | DTO、`AppError` 错误码壳、权限点清单 | A C | 单元 `permissions.rs`；`console_m2::permission_point_matrix`；守卫 `guard-frontend-permissions.py`（前端引用的权限点都在后端清单） | 后端自然语言检查（i18n-audit §3）为人工 `rg` |

### 2.2 gateway 角色（`bins/okapi/src/gateway`）

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `auth.rs` | Bearer / x-api-key / x-goog-api-key 鉴权、无效 key 每 IP 限流、key 级 IP 白名单 | A C | `gateway_invalid_key_rate`、`gateway_ip_allowlist`、`smoke-all.sh`（无凭证 401 fail-closed）、e2e smoke（普通用户管理面 403） | — |
| `clients.rs` | 真实 IP 提取（信任代理 / edge key）、client_type 识别 | A C | 单元；`gateway_ip_allowlist::allowlist_enforced_with_cdn_header_and_peer_fallback`；`console_stats` 客户端分布 | — |
| `scheduler.rs` + `sched_redis.rs` | 候选筛选、优先级 / 权重、三层粘性、双层并发、key 状态机、RPM、web 会话、关键接口限流 | A C D | `gateway_m2_sched`、`channel_key_lifecycle`、`channel_pools`、`gateway_capabilities`、`gateway_fallback`、`gateway_routing_prefs`、`gateway_retry_policy`、`gateway_multipod`、`gateway_compat::per_model_rpm_limit`、`console_diagnose`、`worker_m2::cooled_keys_recover_after_deadline`、`console_auth_web::sessions_list_and_revoke`；单元 `scheduler.rs` | 多副本只有 2 例（在途计数汇总、路由失效广播）。「双副本并发凭证刷新锁」按 IMPLEMENTATION §4.3 **主线只实现 static_key**、OAuth refresh 留扩展点——当前没有会刷新的凭证类型，该验收项不适用，OAuth 上游落地时再补 |
| `chat.rs`（+ `openai_dialect` / `extract` / `estimate` / `rule_inputs`） | `/v1/chat/completions`、`/v1/responses`、`/v1/messages`（+ `count_tokens`）、`/v1beta/models/*:generateContent`；SSE 转发器、failover、usage 复核、reasoning 注入、字段剥离 / 注入 | A B D E | `gateway_m1`（流式精确计费、空回复、首字前 failover、余额不足、缓存 ratio、非流式透传）、`gateway_stream_usage`、`gateway_untrusted_usage`、`gateway_reasoning`、`gateway_reasoning_param`、`gateway_model_modifiers`、`gateway_resp_model`、`gateway_tier`、`gateway_pricing_rules`、`gateway_strip_fields`、`gateway_responses`（原生 / 降级 / 404 回退 / 两跳）、`gateway_messages`、`gateway_gemini_ingress`、`gateway_anthropic`、`gateway_gemini`、`gateway_azure`、`gateway_outbound`（代理 + 额外头）、`gateway_upstream_cost`、`gateway_capabilities`、`gateway_midstream`（09-06 新增：首字后上游掐流 → 不同 key 重试、不 failover 到备用渠道、客户端不见 `[DONE]`、按本地估算结算且余额精确收口、无悬置预扣） | — |
| `embeddings.rs` | `/v1/embeddings`、`/v1/rerank` | A B D | `gateway_embeddings`（prompt-only、failover）、`gateway_azure::azure_embeddings_dispatch` | — |
| `images.rs` | generations / edits（multipart 重组），per_call × n | A B | `gateway_images` | variations 未实现 |
| `audio.rs` | speech 字符计费 / transcriptions & translations per_call | A B | `gateway_audio` | — |
| `videos.rs` | 提交 / 轮询 / 下载，per_call × seconds，任务隔离 | A B C D | `gateway_videos`（跨用户隔离、上游失败退款） | — |
| `realtime.rs` | WS 桥接、连接租约、断开结算 | A B C D | `gateway_realtime`（断开计费、零输出全退、第五连接拒绝、子协议鉴权） | 不走渠道 `proxy_url`（backlog） |
| `custom_pass.rs` | `/pass/{channel_id}/*` 白名单透传 | A B C | `gateway_custom_pass` | — |
| `models.rs` | `/v1/models`、`/v1beta/models` | A | `gateway_gemini_ingress::models_list_is_gemini_shaped`、`console_channel_test` | `/v1/models` 按分组 / key 可见性过滤无直接断言 |
| `dashboard.rs` | new-api 兼容余额端点 | A | `gateway_compat::dashboard_billing_compat` | — |
| `pricing_loader.rs` / `bootstrap.rs` / `state.rs` | PriceBook L1、epoch 订阅热更、`build_state` | A D | `console_m2::pricing_publish_hot_reload_e2e`、`worker_m2::pricebook_hot_reloads_on_new_epoch`、`worker_nats::epoch_broadcast_hot_reload`、`console_ops::cache_flush_pricebook_hotfix` | — |
| `error.rs` | `AppError` → OpenAI / Anthropic / Google 三种错误壳 | A E | `gateway_messages::messages_json_and_error_envelope`、`gateway_gemini_ingress::errors_are_google_rpc_status_shaped`、`console_manage::malformed_query_string_is_rejected_as_error_code` | — |
| 优雅下线（`shutdown.rs`） | SIGINT / SIGTERM → 关监听 → 在途连接排完 → 等后台结算归零 → 退出 | A B D | `gateway_shutdown`（09-06 新增，真实二进制子进程：首块后 SIGTERM，流完整到 `[DONE]`、新连接被拒、退出码 0、账 committed 且无悬置预扣） | SSE 排水无 5min 上限（交编排层 grace period）；无独立 readiness 端点（关监听即等价） |

### 2.3 console 角色（`bins/okapi/src/console`）

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `auth_web.rs` | 注册 / 登录 / TOTP / 兑 key / 会话列举吊销 / 邮箱验证码 / 找回密码 / 关键接口限流 | A C | `console_auth_web`、`console_smtp`（验证码、重置、无 SMTP 501）、`console_audit::login_attempts_are_audited`、e2e smoke（登录 / 登出清 session / session 降级） | 会话数上限未实现 |
| `oauth.rs` | 通用 OAuth2 / OIDC | A C | `console_oauth`（mock IdP 授权码全流程） | 仅一家 mock IdP，无 Discord / LinuxDO 预设形状用例 |
| `registration.rs` | 注册策略、邀请赠送、Turnstile | A C | 单元；`console_auth_web::registration_policy_gates_signup` | Turnstile 外呼无 mock 用例 |
| `setup.rs` | 空库首启向导 | A | `console_setup`（独立临时库） | — |
| `portal.rs` | `/api/me/*`（key、日志、流水、订单、公开价格、公告） | A C | `console_portal`、`console_portal_pages`、`console_stats::personal_activity_covers_calendar_year_and_isolates_owners`、e2e smoke 门户页 | — |
| `manage.rs` / `admin.rs` / `query.rs` | 六类管理面 CRUD、批量、写校验（azure / 出站 / 注入字段）、路由诊断 | A C | `console_manage`、`console_m2`、`console_users`、`console_visibility`（属主范围 / 分组矩阵）、`console_pricing_write`、`console_channel_test`、`console_import`、`console_diagnose`、`gateway_pricing_rules::console_rule_crud_and_validation` | — |
| `analytics.rs` / `stats.rs` / `logs.rs` / `usage_details.rs` / `activity.rs` / `analysis_*` | CH 立方体三端点、看板、日志检索、实时 KPI、毛利 | A B | `console_analytics`、`console_stats`、`console_logs`、`gateway_upstream_cost`；单元 `activity` / `analysis_freshness` / `usage_details` | `console_analytics` 两例曾在全量并行下偶发（outbox 行被别的进程 drain、两张 MV 先后落地），09-06 改为 `poll_until` 全字段谓词，见第 4 节发现 ① |
| `audit.rs` | 管理写操作 + 登录审计 | C | `console_audit`、`console_ops::assist_overview_scoped_and_audited`、`console_mcp_write`（`mcp:{key_id}` 落痕） | — |
| `dlq.rs` | 死信列表 / 重投 / 丢弃 | A D | `console_logs::dlq_list_requeue_and_discard`、`worker_ch::chsink_pipeline_then_dlq`、e2e smoke 运维页 | — |
| `mcp.rs` | MCP Streamable HTTP 只读 + 写工具三道闸 | A C | `console_mcp`、`console_mcp_write` | — |
| `pay.rs` | epay / Stripe 下单、回调验签、重放幂等、返利 | A B C | `console_pay`、`console_subscriptions::checkout_callback_and_redeem_activate` | Creem / 官方支付宝微信不在范围 |
| `subscriptions.rs` | 套餐 CRUD、购买、配额窗、订阅池优先扣 | A B | `console_subscriptions`（Lua 契约、网关优先扣、worker 滚窗、回调激活、校验）、e2e `subscriptions.spec`（接口桩） | 升降级 / 多订阅并存 backlog |
| `teams.rs` | 建团 / 成员限额 / 团 key / 分账 | A B C | 单元；`console_teams` 全生命周期 | — |
| 兑换码（manage + portal） | 批量生成、核销、绑用户、限 IP | A C | `console_redemption`（并发恰一成功、过期拒绝、IP 上限）、e2e `redemptions.spec` | 多次核销 `max_uses` backlog |
| `ssrf.rs` | 上游 URL 校验 | C | 单元；`console_ssrf` | — |
| SPA 托管 / 内容协商 | `/admin/*` 同挂 API 与 SPA | A | `console_spa_navigation` | — |
| 公告 / settings | `site_notice` 公开端点、设置读写 | A | `console_portal_pages::public_notice_whitelists_and_gates`、`console_ops::settings_get_and_leaderboard`、e2e smoke 公告 | — |

### 2.4 worker 角色、mail、migrate

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `worker/chsink.rs` | outbox → CH 批写、去重、DLQ 终态 | A D | `worker_ch`、`worker_nats` | — |
| `worker/nats_relay.rs` | outbox → JetStream → chsink | A D | `worker_nats` | — |
| `worker/notify.rs` | webhook / email 多路、事件过滤、频率闸、余额低扫描 | A | `worker_notify`、`console_smtp::notify_email_channel_and_admin_test_send` | — |
| `worker/mod.rs` | 悬置清理、三方对账、分区维护、冷却恢复、余额有效期、保留策略、订阅滚窗 | A B D | `worker_m2`、`worker_reconcile_repair`、`console_subscriptions::worker_rolls_window_and_expires` | — |
| `mail/` | SMTP 投递、模板 | A | 单元；`console_smtp`（本地 mock SMTP，AUTH PLAIN） | STARTTLS / 隐式 TLS 未在 mock 覆盖 |
| `migrate.rs` | new-api / 老 ok-api JSONL 导入 | A H | 单元；`migrate_newapi`、`migrate_okapi_old`、`schema_shape` | — |

### 2.5 前端（`frontend/src/features`）

| 功能面 | 路由 | 维度 F 子项 | 覆盖 spec | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| 登录 / 会话 / 权限裁剪 | `/`、`/portal/*` 守卫 | 双登录方式、403 不白屏、导航按权限裁剪、登出清服务端 session | `smoke.spec`（4 例） | — |
| 找回 / 重置密码 | `/forgot-password`, `/reset-password` | 登录页链接带邮箱、提交体 email+lang、防枚举成功态、未配 SMTP 501 文案；缺 token 提示、长度与一致性前置校验、成功回登录、失效 token 400 文案 | `write-forms.spec`（2 例，09-06 新增） | — |
| 新手引导 | `/portal` 快速开始卡 + 顶栏入口 + 密钥页页头 | 进度推导、四步抽屉、客户端片段联动、关闭记忆、移动端与深色 | `guide.spec`（4 例，09-06 新增） | — |
| 门户总览 / 日志 / 流水 / 充值 | `/portal`, `/portal/logs`, `/portal/ledger`, `/portal/topup` | KPI 六卡、页签零请求、空态、导出禁用、流水入口 | `smoke.spec`（2 例）、`charts.spec`（门户图表 5 例）、`interactions.spec`（年度日历 / 热力图 / 个人中心 4 例） | 充值下单跳转支付页无 e2e（有后端 `console_pay`） |
| 公开模型广场 / 调用示例 | `/pricing` | 厂商归一、单位切换、深链、阶梯价、模拟器、移动端深色、分页、加载 / 失败 / 空态 | `smoke.spec`（1 例）、`catalog.spec`（10 例）、`request-examples.spec`（4 例） | — |
| 管理端总览 / 日志 / 洞察 / 质量 / 经营 / 审计 / 运维 | `/admin`, `/admin/logs`, `/admin/stats`, `/admin/quality`, `/admin/revenue`, `/admin/audit`, `/admin/ops` | 实时条、健康芯片、深链即状态、三视图、死信签 | `smoke.spec`（管理端 1 大例）、`charts.spec`（管理图表 6 例） | 需演示超管，缺则跳过 |
| 管理端设置 / 高级配置 / 导航 / 分页 | `/admin/settings`, 侧栏, 列表页 | 分组搜索、敏感值不显示、只读无编辑入口、键盘 / 移动端 / IME、URL 即分页状态 | `interactions.spec`（19 例） | — |
| 用户 / 密钥 | `/admin/users`, `/portal/keys` | 搜索回车、抽屉落地签、删除二次确认手输名称 | `smoke.spec`（管理端大例内的用户抽屉段 + 删除二次确认 1 例） | — |
| 用户抽屉写操作 | `/admin/users` 管理抽屉 | 入账 USD → micro 整数（含 0.29 浮点边界）、系数按十进制字符串提交且负数 / 未改动不放行、分组全量覆盖且先出现者优先级高、封禁经确认框且成功后翻成解封 | `write-forms.spec`（1 例，09-06 新增） | 角色 / 订阅 / 余额有效期三段仍无 e2e |
| 模型定价抽屉 | `/admin/pricing` 编辑 / 新建 | 七个倍率轴按十进制字符串提交、空档位行过滤、`tier_expr` 去空格回传且模式提示随之切换、无档位不发 `tier_ratios` 键、降级链原样回传、编辑态模型名只读 | `write-forms.spec`（1 例，09-06 新增） | 发布 epoch 按钮、状态切换无 e2e |
| 兑换码 | `/admin/codes` | 分页 / 筛选复位 / 末页停用 | `redemptions.spec` | 生成抽屉无 e2e |
| 订阅套餐 | `/portal/plans` | 在售 / 已订阅高亮 / 停用说明 / 下单参数 | `subscriptions.spec` | 管理端 `/admin/plans` 无 e2e |
| 渠道抽屉「请求与计费行为」 | `/admin/channels` 编辑抽屉 | 已有 proxy / 额外头回显；注入字段按 JSON 解析（数字 / 带引号字符串）；清空额外头即从 settings 删键；PATCH 体只含有值的键；受保护键 400 → 错误码文案且抽屉不关 | `write-forms.spec`（1 例，09-06 新增） | 接入 / 模型 / 调度三个页签的写操作仍无 e2e |
| 安全页会话卡 | `/portal/security` | 列表 + 当前浏览器徽章、单条吊销打 `DELETE /api/me/sessions/{sid}`、全部吊销打 `DELETE /api/me/sessions`、空态文案 | `write-forms.spec`（1 例，09-06 新增） | TOTP 绑定流程仍无 e2e |
| 套餐抽屉 | `/admin/plans` 编辑 / 新建 | 充值模板与订阅两形态字段互斥（切换即替换字段区）、USD → micro、天数 `Math.trunc`、空值不发键、订阅缺有效期禁用保存、售价空 = 0 不售卖、编辑态代码锁定 | `write-forms.spec`（1 例，09-06 第四轮） | 删除套餐无 e2e |
| 角色抽屉 | `/admin/roles` | 权限点来自 `/admin/permissions`、整组切换、无权限点禁用创建、编辑态 code 锁定且已有权限预勾、删除经确认框、后端 409 `role_in_use` 渲染成文案 | `write-forms.spec`（1 例，09-06 第四轮） | — |
| 池 / 分组 / 规则 / 团队 / SMTP | `/admin/pools`, `/admin/groups`, `/admin/rules`, `/portal/teams`, 设置 SMTP 卡 | — | `screenshots.spec` 非断言 | **写操作表单无 e2e**：分组 / 池 / 规则、团队建团与成员、SMTP 卡 |
| i18n | 全站 | 裸文案零、双语言包键对齐 | `guard-i18n.sh`、`guard-i18n-keys.py`；e2e 断言同时匹配中英正则 | 后端错误码是否全部有 `errors` 命名空间映射：靠 `guard-i18n-keys.py` 的引用键检查，未反向核对后端 `codes::*` 全集 |

### 2.6 部署与性能

| 项 | 维度 | 覆盖 | 缺口 / 备注 |
| --- | --- | --- | --- |
| `okapi all` 单机形态 | I | `scripts/smoke-all.sh` 四断言 | — |
| embed-web 发布构建（`deploy/Dockerfile`） | I | 手工 | 无自动化构建校验 |
| compose 双 profile / k8s manifests / Nginx SSE 模板 | I | 手工 | 无自动化 |
| 缩尺压测 / Linux 复测 | G | `docs/perf-report.md`（2026-08-30） | 裸金属正式复测、10 万 SSE 整数口径待办 |

## 3. 覆盖缺口清单（按风险排序）

1. ~~PG 记账不幂等~~ **已修**（09-06 第五轮）：`docs/database.md` §1.5 定案「每 request_id 恰一行」，`record_settlement` 事务开头 `SELECT EXISTS` 幂等闸，重放整笔跳过并告警；`pg_settlement::replaying_a_settled_request_writes_nothing` 钉住。ledger 的 Lua 与 PG 契约至此都有直测。
2. **前端写操作表单 e2e 尚未收口**：09-06 四轮补了渠道抽屉行为页签、会话吊销卡、忘记 / 重置密码页、用户抽屉、模型定价抽屉、套餐抽屉、角色抽屉；仍缺分组 / 池 / 规则、团队、TOTP 绑定、SMTP 卡、渠道抽屉另三个页签、各页删除动作。
3. ~~SIGTERM 优雅下线无自动化用例~~ **已补且修了实现**（09-06 第三轮，`gateway_shutdown`；见第 4 节发现）。凭证刷新锁按 §4.3 定案不适用于当前 static_key 主线。剩余：SSE 排水无 5min 上限（依赖编排层 grace period）。
4. ~~mid-stream 断流语义无专项用例~~ **已补**（09-06 第三轮，`gateway_midstream`）。
5. **集成测试共享一条 `billing_outbox` 队列**：任一用例的行都可能被别的测试进程 drain 进 CH，因此「drain 后直接读 CH 并断言」天然有竞态。现行约定是走 `poll_until` 且谓词覆盖全部待断言字段（09-06 修了两处漏网的）；新增 CH 用例须照此写，或改为按 user_id 隔离的 drain。
6. 部署形态（embed-web 构建、compose / k8s）与性能维度不在常规回归。
7. OAuth 仅单一 mock IdP；Turnstile 外呼无 mock；SMTP TLS 形态未覆盖。

## 4. 执行记录

### 2026-09-06（new-api / Sub2API 差距补齐后的首轮全量）

环境：Apple Silicon macOS 开发机；PG 16 / Redis 7 / NATS 2 / ClickHouse 24.8 四容器（`scripts/dev-deps.sh`）；dev 库存量 6330 用户 / 3949 渠道 / 演示超管在位；Rust debug 构建；Playwright 1.62 Chromium。待验证的工作树 = 09-05 review 重构 + 6 大项差距 + 5 小项差距 + 并行会话新增的门户引导（portal-guide），全部未提交。

| 层 | 结果 | 数字 | 说明 |
| --- | --- | --- | --- |
| L0 rustfmt | 通过 | 13 文件重排 | 首查 13 个文件不符（09-05 会话遗留），`cargo fmt --all` 后干净，纯格式 |
| L0 clippy | 通过 | 0 警告 | 全工作区含测试目标 |
| L0 sqlx 离线快照 | 通过 | 521 个查询文件 | 昨夜中断的 prepare 补跑；须带 `-- --all-targets` |
| L0 cargo-deny | **部分失败** | advisories / bans / sources 通过；licenses 失败 | 见发现 ③ |
| L0 前端 tsc / oxlint | 通过 | — | 中途一次红是并行会话半成品（`PortalKeysPage.tsx` 未用导入），对方完成后复查通过 |
| L0 四道守卫 | 通过 | i18n 引用键 1377 个双语齐全；9 个权限点在后端清单 | 中途 i18n 键守卫报 39 键缺失，同为并行会话半成品，落地后通过 |
| L1 + L2 Rust 全量 | 通过（复跑） | 93 个测试二进制 / 432 用例：首轮 430 过 2 败，修复后复跑 432 全过，0 忽略 | 两败均在 `console_analytics`，见发现 ① |
| L3 前端交互 e2e | 通过 | 55 / 55（interactions 24 + charts 11 + catalog 10 + request-examples 4 + redemptions 4 + subscriptions 2，含多视口变体） | 沙箱把 `PLAYWRIGHT_BROWSERS_PATH` 指到空缓存，需显式指回 `~/Library/Caches/ms-playwright`；`pnpm test:interactions` 的前置 `pnpm build` 被并行会话半成品打断一次，直接跑 Playwright 通过 |
| L4 前端冒烟 e2e | 通过 | 10 / 10，0 跳过 | 演示超管在位，管理端两例真实执行；含登录页、公开广场、权限分级、session 降级、登出清 session、门户总览 / 日志 / 流水 / 充值、管理端总览 / 日志 / 洞察 / 质量 / 经营 / 审计 / 渠道 / 运维 / 用户抽屉、公告发布下架、删除二次确认 |
| L5 `okapi all` 冒烟 | 通过 | 4 / 4 断言 | 双 healthz、root key 门户可用、SPA 可达、数据面无凭证 401 |
| L6 性能 | 未执行 | — | 非本轮目标，基线见 `docs/perf-report.md` |

发现与处置：

1. **`console_analytics` 并行偶发（已修）**：`breakdown_by_each_dimension` 与 `legacy_aggregate_remainder_is_preserved_once_and_marked_unknown` 在全量并行下失败、单跑通过。根因：`billing_outbox` 是共享 PG 队列，别的测试进程的 chsink 会 `SKIP LOCKED` 抢走本用例的行并写 CH；`mv_analysis_hour` 与 `mv_cube_hour` 两张 MV 先后落地，handler 里先后两条 CH 查询可能夹在中间——所以出现「total_requests = 8 但明细 0 行」「requests = 3 但 cost_known = 0」。两处改为 `poll_until` 且谓词覆盖后续断言的全部字段。产品侧无影响（看板对在途行的瞬时读偏差可接受）。
2. **两处 clippy `-D warnings` 会拦的遗留**：新测试里多余的 `mut`、`sched_redis.rs` 会话列表 `sort_by` 应为 `sort_by_key(Reverse)`。已修。
3. **cargo-deny licenses 失败**：三类。a) `quoted_printable`（0BSD）随 09-05 新增的 `lettre` 进入依赖树，`deny.toml` 未放行——已加 `0BSD`；b) `webpki-roots` / `webpki-root-certs`（CDLA-Permissive-2.0，Mozilla 根证书数据许可）在 init 提交时就在锁文件里，说明 deny 的 licenses 门从未绿过——已加 `CDLA-Permissive-2.0`；c) 7 个本仓库 crate 没有 `license` 字段，**仍失败**，等 IMPLEMENTATION §15「开源协议待拍板」定案后统一补字段（倾向 AGPL-3.0）。
4. **沙箱环境两处重定向**会让人跑到旧产物：`CARGO_TARGET_DIR` 与 `PLAYWRIGHT_BROWSERS_PATH`。已写入第 1 节注意事项。
5. 覆盖缺口未变化，见第 3 节；本轮没有新增自动化用例（只修 flake），下一轮优先补第 3 节第 1、2 条。

本轮改动清单（均为验证配套，不含功能）：`bins/okapi/tests/console_analytics.rs`（两处 poll_until）、`bins/okapi/tests/gateway_invalid_key_rate.rs`（去 `mut`）、`bins/okapi/src/gateway/sched_redis.rs`（`sort_by_key`）、`deny.toml`（两个许可证放行）、`.sqlx/`（重生成）、rustfmt 触及的 13 个文件、本文档。

### 2026-09-06 第二轮：补第 3 节前两条缺口

新增用例（均只加测试与一个 dev-dependency，不动功能代码）：

| 新增 | 用例 | 结果 |
| --- | --- | --- |
| `crates/okapi-ledger/tests/lua_contract.rs` | 7 例：预扣字段形状与多退少补、commit / refund 任意顺序幂等、余额边界 fail-closed 零写入、四限额 which 与零写入、repair 绕开在途且不动另一池且负目标不夹逼、drain 只取正余额、13 步交错序列的不变式 | 7 / 7 通过；随跑 `cargo test -p okapi-domain -p okapi-pricing -p okapi-ledger`：domain 12、pricing 20 + parity 4 + prop 6 全过 |
| `frontend/e2e/write-forms.spec.ts`（加入 interactions 配置） | 4 例：渠道抽屉行为页签 PATCH 体形状 + 400 文案、会话卡单条 / 全部吊销端点与空态、找回密码链接 / 提交体 / 防枚举成功态 / 501 文案、重置密码校验 / 成功 / 400 文案 | 4 / 4 通过 |
| `frontend/e2e/guide.spec.ts`（并行会话新增，纳入本清单） | 4 例：快速开始卡、四步抽屉、密钥页入口、移动端深色 | 4 / 4 通过 |

复核：interactions 配置全量 63 / 63；`cargo fmt --check`、全工作区 clippy `-D warnings`、前端 tsc / oxlint 全部干净。`okapi-ledger` 新增 `dotenvy` dev-dependency（workspace 已有，只为测试读 `.env`）。

发现：ledger 契约用例一次通过，说明 Lua 脚本本身与 §2.2 文档一致、此前只是缺直测；写操作 e2e 也未暴露前端缺陷，价值在于把「settings 里空对象要删键、注入值按 JSON 解析」这类约定钉住，后续改抽屉不会静默破坏后端写校验的前提。

### 2026-09-06 第三轮：断流语义、优雅下线、钱相关表单

| 新增 | 用例 | 结果 |
| --- | --- | --- |
| `bins/okapi/tests/gateway_midstream.rs` | 上游吐两段正文后掐连接：客户端收到两段、无 `[DONE]`、备用渠道零调用、掐流渠道只被打一次；记录 committed、`failover_count = 0`、金额 = (本地估算 prompt + completion) × 2 micro、余额精确收口、无悬置预扣 | 1 / 1 一次通过（实现本就符合 §3.6，此前只是缺用例） |
| `bins/okapi/tests/gateway_shutdown.rs` | 真实二进制子进程 + 慢流 mock：首块后 `kill -TERM`，流完整到 `[DONE]`、5s 内新连接被拒、15s 内退出码 0、记录 committed 金额 240、无悬置预扣 | **首跑失败**（见发现 ①），修复后 3 连跑通过 |
| `frontend/e2e/write-forms.spec.ts` +2 | 用户抽屉：入账 12.34 / 0.29 USD → 12_340_000 / 290_000 micro、系数 `'0.8'` 字符串且 `-1` / 未改动禁用、分组 `[default:2, vip:1]`、封禁经确认框且翻成解封；模型抽屉：七轴字符串、空档位行过滤、`tier_expr` 去空格、无档位不发 `tier_ratios`、降级链回传、编辑态名只读 | 2 / 2 通过；写法改为 `waitForRequest` 等真实 POST，避免并行负载下的竞态 |

全量复核：Rust 96 个测试二进制 441 / 441（含新增 9 例）；interactions 配置 65 / 65；L4 冒烟 10 / 10；L5 `okapi all` 4 / 4（现经 SIGTERM 正常退出）；`cargo fmt --check`、全工作区 clippy `-D warnings`、tsc / oxlint 干净；`.sqlx` 重生成（523 文件）。

发现与处置：

1. **SIGTERM 会把进程直接掐死（已修）**：三角色的 `shutdown_signal` 只听 `ctrl_c()`（SIGINT），而 Docker / K8s stop 发的是 SIGTERM——用例首跑时客户端立刻读到 `unexpected EOF during chunk size line`，账本留下一笔 Redis 预扣。修正：新增 `bins/okapi/src/shutdown.rs`，`signal()` 同时等 SIGINT / SIGTERM，gateway / console / worker 共用；worker 的信号监听建一次挂在循环外。
2. **连接排完不等于账已落（已修）**：chat 流式 / 非流式两条路径都是"响应先行、结算后台"（perf-report 的热路径优化），`axum::serve` 的 graceful shutdown 只等连接关闭，进程随后退出会掐掉正在跑的结算任务。修正：`shutdown::Pending` 计数这两处 spawn，`gateway::run` 在排水后等计数归零（上限 30s，超时告警交对账）。IMPLEMENTATION §14.3 已加实现状态注记（SSE 排水无 5min 上限、无独立 readiness 端点两点如实写明）。
3. **`console_analytics` 再抓到一例同类竞态**：`flow_links_conserve_and_fold_other` 的谓词只等 `total`，`coverage_bp` 来自另一条查询。连同 `advanced_filters…`（previous / cost_known 分属不同查询）与 `flow_names…`（nodes 另查）一起把谓词补全；本轮全量复跑干净。
4. 「双副本并发凭证刷新锁」核实为不适用：§4.3 主线只实现 static_key，没有会刷新的凭证类型；矩阵已改注。

本轮改动清单：新增 `bins/okapi/src/shutdown.rs`；`gateway/mod.rs`（run 等结算归零、去本地 shutdown_signal）、`gateway/state.rs`（`settlements` 字段）、`gateway/chat.rs`（两处 spawn 改经计数）、`console/mod.rs`、`worker/mod.rs`（信号）；新增三个测试文件 / 两条 e2e；`console_analytics.rs` 三处谓词；`IMPLEMENTATION.md` §14.3 注记；本文档。

### 2026-09-06 第四轮：PG 记账契约 + 套餐 / 角色表单

| 新增 | 用例 | 结果 |
| --- | --- | --- |
| `crates/okapi-ledger/tests/pg_settlement.rs` | 5 例：五处同事务落地且四金额列 / pool / 维度三处一致（含 `client_ip` INET 列、`ratio_snapshot` 字符串化）；events 第二条语句超长 `event_type` 失败 → 已插的 records 行随事务回滚、五处零残留；订阅池结算与 `sub_grant` 事件不动 `users.balance_micro`；失败请求零金额 + `error_code` + 成本未知；`admin_refund` 状态翻 30、快照与 key 用量回冲、outbox 负额冲销四金额取反且 token 不冲，重复退款与对失败记录退款均 None 且零写入 | 5 / 5 通过（一处 INET 读法用 `host()` 而非 `::text`，后者带 `/32`） |
| `frontend/e2e/write-forms.spec.ts` +2 | 套餐抽屉：编辑充值模板（代码锁定、USD 回填、有效期 `30.9` → 30、换组）与新建订阅（切形态字段区替换、缺有效期禁用、每周 / 售价 9.99 → 9_990_000 / 描述去空格、不发 `balance_valid_days` / `group_code`）；角色抽屉：整组切换 + 单勾 + 取消、无权限禁用、编辑态 code 锁定预勾、删除确认框、409 `role_in_use` 文案 | 2 / 2 通过（角色用例首跑一次失败：`filter({ has })` 里误用了带 dialog 前缀的定位器，Playwright 会从候选元素内部重新求值；改为从域名 span 上溯父节点） |

全量复核：`cargo test -p okapi-domain -p okapi-pricing -p okapi-ledger` 54 / 54；全工作区 clippy `-D warnings`、`cargo fmt --check`、oxlint 干净；interactions 配置 67 / 67；`.sqlx` 重生成（533 文件）。

发现：`record_settlement` 的 PG 侧不幂等（第 3 节第 1 条），本轮只记录、未改——它触及计费写路径的语义，按项目规则应先在 `docs/database.md` 定案再动代码。

### 2026-09-06 第五轮：PG 记账幂等闸

前四轮已提交（`2ac7c94` 功能 / 收口 / 下线，`34edd2b` 清单 / 测试）。本轮按「先文档后代码」处理第 3 节第 1 条：

- `docs/database.md` §1.5：在 `idx_br_request` 旁写明「每 request_id 恰一行」的语义、为什么分区表给不了唯一约束、以及由 `record_settlement` 承担幂等闸。
- `crates/okapi-ledger/src/pg.rs`：事务开头 `SELECT EXISTS(... WHERE request_id = $1)`，命中即回滚空事务、`warn!` 记录重放、返回 Ok。每笔结算多一次走 `idx_br_request` 的点查，在后台结算路径且受 `settle_gate` 限流，可接受。
- `pg_settlement::replaying_a_settled_request_writes_nothing`：重放同一 request_id（金额还故意改成 999_999）后记录仍一行、事件仍一条、outbox 仍一条、快照与 key 用量只动一次，且第一笔的列值原样。

复核：计费三 crate 55 / 55；全量 Rust 97 个测试二进制 447 / 447（结算路径多一次点查未影响任何既有用例）；clippy `-D warnings` 干净；`.sqlx` 重生成。
