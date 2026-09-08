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
| L0 静态守卫 | rustfmt、clippy `-D warnings`（含测试目标，**用 CI 的 stable 版本**，见注意事项）、sqlx 离线快照完整、cargo-deny（advisories / bans / licenses / sources，09-07 起四项全 ok）、前端 tsc / oxlint、六道守卫（浮点 / i18n 裸文案 / i18n 键对齐 / 前端权限点 / 部署模板 / 后端错误码反向核对） | `cargo fmt --all -- --check` · `SQLX_OFFLINE=true cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny --all-features check` · `cd frontend && pnpm exec tsc -b && pnpm exec oxlint` · `bash scripts/guard-no-float.sh && bash scripts/guard-i18n.sh && python3 scripts/guard-i18n-keys.py && python3 scripts/guard-frontend-permissions.py && python3 scripts/guard-deploy-manifests.py && python3 scripts/guard-error-codes.py` | 部署模板守卫需 PyYAML 或系统 ruby |
| L1 单元 / 性质 | crate 内 `#[cfg(test)]` 与 `crates/*/tests`（pricing 对拍 + proptest、providers 转换 parity、ledger Lua 契约） | `cargo test -p okapi-domain -p okapi-pricing -p okapi-ledger -p okapi-providers -p okapi-api -p okapi-store` | ledger 契约需 Redis；store 部分用例需 PG |
| L2 集成 | `bins/okapi/tests/*.rs` 71 个套件（gateway / console / worker / migrate） | `cargo test --workspace --no-fail-fast` | 四容器；CH / NATS 缺失时相关套件自跳过 |
| L3 前端交互 e2e | 构建产物 + 接口桩，不碰数据库 | `cd frontend && pnpm test:interactions`（= `pnpm build` + `playwright test -c playwright.interactions.config.ts`） | 无（自起 vite preview :4175） |
| L4 前端冒烟 e2e | 打真实 console（API + SPA 同源），注册真实用户；管理端用例需演示超管（`scripts/dev-reset.sh` 灌注），缺则跳过 | `CARGO_TARGET_DIR=target cargo build --bin okapi && cd frontend && pnpm build && pnpm exec playwright test -c playwright.config.ts smoke.spec.ts` | 四容器 + `target/debug/okapi` + `frontend/dist` |
| L5 部署形态 | `okapi all` 一进程三角色：双 healthz、root key 引导、SPA 可达、数据面 fail-closed；embed-web 发布形态：模板守卫 + `--features embed-web` 构建 + 无 dist 目录起 console 服出首页 / 哈希资源 / SPA 深链 / API 不受影响 | `bash scripts/smoke-all.sh` · `bash scripts/verify-deploy.sh` | 四容器；smoke-all 占用 :8080 / :8081；verify-deploy 独立 target 目录 `target/embed-web`，首次约 1 分钟 |
| L6 性能 | loadgen 缩尺 / Linux 容器复测 | `cargo run --release --example loadgen -- ...`、`scripts/linux-bench.sh`（见 `docs/perf-report.md`） | 独占机器，按需 |

注意事项：
- 本机 Cursor 沙箱会把 `CARGO_TARGET_DIR` 重定向到缓存目录，L4 / L5 依赖的 `target/debug/okapi` 必须显式 `CARGO_TARGET_DIR=target` 构建，否则跑的是旧二进制。
- `.sqlx` 快照必须用 `cargo sqlx prepare --workspace -- --all-targets` 生成；不带 `--all-targets` 会漏掉测试里的 `query!`，CI 离线编译即红。
- L4 与 L5 都占 :8081，顺序执行；L4 的 Playwright `webServer` 在 `reuseExistingServer: true` 下会复用已在跑的 console。
- **跑 L2 之前先 `lsof -nP -iTCP:8080 -iTCP:8081 -sTCP:LISTEN` 确认没有常驻的 `okapi all` / `okapi worker`**：它连着同一套 PG / Redis，其 worker 会抢先消费通知、上报在途量表、写 settings 缓存，让 `worker_notify` / `gateway_multipod` / `console_smtp` 等五个套件随机红（第十一轮实测）。
- **L0 的 clippy 与 deny 要按 CI 的口径跑，本机绿不等于 CI 绿**（第十七轮实测）：CI 用 `dtolnay/rust-toolchain@stable`，跟着最新 stable 走（09-07 是 1.98.1，本机 1.95），每个新版本都会带新的默认 lint；推送前用 `rustup toolchain install <CI 的 stable> --profile minimal --component clippy` 后 `cargo +<版本> clippy --workspace --all-targets -- -D warnings`，或干脆 `rustup update`。`cargo-deny-action` 缺省带 `--all-features`，所以要跑 `cargo deny --all-features check`（`embed-web` 特性的依赖树只有这样才进来）。
- 想在 worktree 或第二份 checkout 上独立验证时，`.env` 不够：`dotenvy` 不覆盖已导出的环境变量，shell 里 `set -a; . .env` 过的 `DATABASE_URL` 会让 worktree 连回主库（迁移版本不一致即 `VersionMissing`）；必须显式传 `DATABASE_URL=…`。同时 Redis / CH 也要隔离——两套 PG 的自增 id 会在同一 Redis（`bal:{uid}`）与同一 CH 库里串味，余额与聚合断言随机失败。Redis 用逻辑库号（`redis://…:63790/7` + `redis-cli -n 7 flushdb`）零成本隔离；CH 库名在测试里写死 `okapi`，暂无法隔离，相关两例（`entity_usage_batches_by_ids`、`admin_refund_full_cycle`）以主库结果为准。

## 2. 覆盖矩阵

「维度」列只登记**有自动化用例**的维度；括号内为主要套件（`bins/okapi/tests/<name>.rs` 省略路径）。

### 2.1 领域 crates

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `okapi-domain` | Money newtype、ID newtype、计费状态机、token 计数 | A B | 单元：`money.rs` / `state.rs` / `tokens.rs`；守卫：`guard-no-float.sh` | 状态机转移矩阵穷举在 `state.rs` 单测 |
| `okapi-pricing` | PriceBook 编译、三层倍率 / 按次 / 阶梯 / 缓存双轴、规则栈、快照 | A B | `parity.rs`（new-api 对拍 fixtures）、`prop.rs`（proptest）、单元 `book/engine/handle/model/ratio/rules`；集成 `gateway_m1`（cache ratio）、`gateway_tier`、`gateway_pricing_rules`、`gateway_model_modifiers`、`console_pricing_write`、`console_import` | — |
| `okapi-ledger` | Redis Lua reserve / commit / refund / repair / sub_set + PG 同事务记账 + outbox | A B D | crate 级 `tests/lua_contract.rs`（09-06 新增，7 例）：预扣字段四段 / 多退少补 / 重复 commit 与 refund 任意顺序幂等 / `avail == est` 放行、`avail < est` 拒绝且零写入 / 四个 key 级限额各自 which 且拒绝零写入、并发槽随结算释放 / repair 绕开在途、不动另一池、负目标不夹逼 / drain 只取正余额 / 13 步交错序列逐步验证 `avail + Σ在途 == 入账 − Σ实际`；订阅池选池由 `console_subscriptions::lua_pool_contract` 覆盖；crate 级 `tests/pg_settlement.rs`（09-06 第四轮，5 例）：records / events / users 快照 / api_keys 用量 / outbox 五处同事务且四金额列与 pool 三处一致（含 INET 列真落）、第二条语句失败整体回滚、订阅池结算与订阅事件不动钱包快照、失败请求零金额落 error_code、`admin_refund` 只对 committed 生效一次并逐项回冲；集成：`gateway_m1`、`worker_m2`、`worker_reconcile_repair`、`console_ops`、`console_teams`、`gateway_realtime`；第五轮补 `replaying_a_settled_request_writes_nothing`（重放 request_id 五处零写入） | — |
| `okapi-providers` | openai / anthropic / gemini / azure / bedrock / vertex / custom_pass / responses 客户端，`oauth::{anthropic_max,codex}` 订阅登录（§11.38），`convert/*` 按方向转换，modifiers / reasoning，`http.rs` 代理与额外头，`aws_sigv4` / `aws_eventstream`（§11.35） | A E | `convert_a2o` / `convert_anthropic` / `convert_gemini` / `reasoning_t2c` / `stream_usage`；单元 `azure` / `gemini_to_openai` / `http` / `modifiers` / `reasoning` / `responses` / `aws_sigv4`（AWS 官方派生密钥与 get-vanilla 签名向量、会话令牌、凭证形态、路径编码；09-07 第十五轮加 botocore 生成的含 `%3A` 路径向量——规范 URI 二次编码，此前必签错）/ `aws_eventstream`（跨包切帧、坏 prelude、非字符串头跳过）/ `bedrock`（区域解析、InvokeModel 体、chunk 解码、exception 帧）/ `vertex`（publisher 路由、api_base 形状、rawPredict 体、SA 解析、JWT RS256 三段）/ `oauth`（PKCE S256 派生、两家授权 URL 参数（Anthropic 端点已随 CLI 迁 `claude.com/cai` + `platform.claude.com`）、token 响应形状与缺省、invalid_grant 分类含 codex-rs 的 `refresh_token_*` 细分码、贴回 code 拆分、系统首句前置幂等、beta 头合并去重且透传头里的 `anthropic-beta` 摘掉不发两行、id_token claim 取 account_id、Codex 请求体整形（store=false / stream=true / instructions 键 / system→developer / 不支持字段剥离）、Codex SSE 聚合回 JSON（终态 output 为空按 output_index 拼、error 事件 → 502、无终态 → Stream 错）/ `anthropic`（429 冷却：Retry-After 优先，否则 `anthropic-ratelimit-unified-reset` 推剩余秒，过去的重置点不冷却）；集成见 2.2 chat 行 | `gemini_to_openai` 只有单元 + `gateway_gemini_ingress` 集成，无独立 parity fixture 文件 |
| `okapi-store` | sqlx 查询、迁移、凭证信封 AES-GCM、身份（argon2 / bcrypt 双轨）、分页、CIDR 匹配、CH schema | A C H | 编译期：`.sqlx` 离线校验全部 `query!`；`schema_shape`（迁移形状守卫）、`channel_credential`（密文落库 / 无主密钥 fail-closed）、`console_manage::price_group_pagination_matches_database_pages`、`gateway_ip_allowlist`（netmatch）、`worker_ch`（CH 表与 MV）；单元 `credential` / `identity` / `listing` / `mutate` / `netmatch` / `subscriptions` / `vendor` | — |
| `okapi-api` | DTO、`AppError` 错误码壳、权限点清单 | A C | 单元 `permissions.rs`；`console_m2::permission_point_matrix`；守卫 `guard-frontend-permissions.py`（前端引用的权限点都在后端清单） | 后端自然语言检查（i18n-audit §3）为人工 `rg` |

### 2.2 gateway 角色（`bins/okapi/src/gateway`）

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `auth.rs` | Bearer / x-api-key / x-goog-api-key 鉴权、无效 key 每 IP 限流、key 级 IP 白名单、分组级 `[rpm, rph]`（§11.32，随鉴权缓存下发、全部计费端点 reserve 前检查） | A C | `gateway_invalid_key_rate`、`gateway_ip_allowlist`、`gateway_group_rate`（09-06：同组每用户各自计数、别组不受影响、rph 小时窗、管理面改限额即失效缓存、负数 400、0 归一 null；09-08 第十九轮加 `non_billing_upstream_endpoints_are_rate_limited`：`count_tokens` 与视频任务轮询 / 下载这两个不计费但打上游的端点同样进窗，且限速在任务查找之前）、`smoke-all.sh`（无凭证 401 fail-closed）、e2e smoke（普通用户管理面 403） | 八个计费端点里 chat 与上述两个非计费端点有集成用例；其余共用同一 `check_group_rate`，靠编译期同构 |
| `clients.rs` | 真实 IP 提取（信任代理 / edge key）、client_type 识别 | A C | 单元；`gateway_ip_allowlist::allowlist_enforced_with_cdn_header_and_peer_fallback`；`console_stats` 客户端分布 | — |
| `scheduler.rs` + `sched_redis.rs` | 候选筛选、优先级 / 权重、三层粘性、双层并发、key 状态机、RPM、web 会话、关键接口限流 | A C D | `gateway_m2_sched`、`channel_key_lifecycle`、`channel_pools`、`gateway_capabilities`、`gateway_fallback`、`gateway_routing_prefs`、`gateway_retry_policy`、`gateway_multipod`、`gateway_compat::per_model_rpm_limit`、`console_diagnose`、`worker_m2::cooled_keys_recover_after_deadline`、`console_auth_web::sessions_list_and_revoke`；单元 `scheduler.rs` | 多副本只有 2 例（在途计数汇总、路由失效广播）。「双副本并发凭证刷新锁」按 IMPLEMENTATION §4.3 **主线只实现 static_key**、OAuth refresh 留扩展点——当前没有会刷新的凭证类型，该验收项不适用，OAuth 上游落地时再补 |
| `chat.rs`（+ `openai_dialect` / `extract` / `estimate` / `rule_inputs`） | `/v1/chat/completions`、`/v1/responses`、`/v1/messages`（+ `count_tokens`）、`/v1beta/models/*:generateContent`；SSE 转发器、failover、usage 复核、reasoning 注入、字段剥离 / 注入 | A B D E | `gateway_m1`（流式精确计费、空回复、首字前 failover、余额不足、缓存 ratio、非流式透传）、`gateway_stream_usage`、`gateway_untrusted_usage`、`gateway_reasoning`、`gateway_reasoning_param`、`gateway_model_modifiers`、`gateway_resp_model`、`gateway_tier`、`gateway_pricing_rules`、`gateway_strip_fields`、`gateway_responses`（原生 / 降级 / 404 回退 / 两跳）、`gateway_messages`、`gateway_gemini_ingress`、`gateway_anthropic`、`gateway_gemini`、`gateway_azure`、`gateway_bedrock`（§11.35：mock 用同一 secret 重算 SigV4、模型 ID `%3A`、InvokeModel 体去 model/stream 加版本、event-stream 帧流式 + JSON 两路计费、API key 走 Bearer、Anthropic 入口透传、embeddings 不路由）、`gateway_vertex`（服务账号 JWT-bearer 换 token 且两请求只换一次、Gemini generateContent / streamGenerateContent?alt=sse、Claude rawPredict / streamRawPredict 版本字段为 vertex 值、Anthropic 入口透传）、`gateway_oauth_channels`（§11.38：控制面两步登录建渠道且 credential_kind=1；anthropic_max 请求打 `?beta=true`、Bearer / 三个必备 beta / 系统首句且无 x-api-key、OpenAI 与 Anthropic 双入口、客户端 `user-agent` / `x-app` / `x-stainless-*` 原样透传且其 `anthropic-beta` 合并为单行、用户 token 的 `x-api-key` 不上游、到期后并发两请求只刷一次且 refresh 轮转回写、invalid_grant → key status 6；codex 从 id_token 取 account_id、Responses 带 chatgpt-account-id / `accept: text/event-stream`、`originator` 客户端带了透传否则缺省、上游体 store=false / stream=true / instructions 键 / previous_response_id 剥离 / system→developer、非流式客户端拿到 SSE 聚合出的 JSON 且按其 usage 结算、流式原样透出、chat 与 embeddings 入口不路由；state 一次性、非 OAuth 协议 400）、`gateway_outbound`（代理 + 额外头）、`gateway_upstream_cost`、`gateway_capabilities`、`gateway_midstream`（09-06 新增：首字后上游掐流 → 不同 key 重试、不 failover 到备用渠道、客户端不见 `[DONE]`、按本地估算结算且余额精确收口、无悬置预扣） | — |
| `embeddings.rs` | `/v1/embeddings`、`/v1/rerank` | A B D | `gateway_embeddings`（prompt-only、failover）、`gateway_azure::azure_embeddings_dispatch` | — |
| `images.rs` | generations / edits（multipart 重组），per_call × n | A B | `gateway_images` | variations 未实现 |
| `audio.rs` | speech 字符计费 / transcriptions & translations per_call | A B | `gateway_audio` | — |
| `videos.rs` | 提交 / 轮询 / 下载，per_call × seconds，任务隔离 | A B C D | `gateway_videos`（跨用户隔离、上游失败退款） | — |
| `realtime.rs` | WS 桥接、连接租约、断开结算 | A B C D | `gateway_realtime`（断开计费、零输出全退、第五连接拒绝、子协议鉴权） | 不走渠道 `proxy_url`（backlog） |
| `custom_pass.rs` | `/pass/{channel_id}/*` 白名单透传 | A B C | `gateway_custom_pass` | — |
| `models.rs` | `/v1/models`、`/v1beta/models` | A | `gateway_gemini_ingress::models_list_is_gemini_shaped`、`console_channel_test` | `/v1/models` 无鉴权、列出全部启用模型（目录与公开价格页同为公开信息，`public_pricing_no_auth`）；按 key / 分组过滤属特性待定，非缺口 |
| `dashboard.rs` | new-api 兼容余额端点 | A | `gateway_compat::dashboard_billing_compat` | — |
| `pricing_loader.rs` / `bootstrap.rs` / `state.rs` | PriceBook L1、epoch 订阅热更、`build_state` | A D | `console_m2::pricing_publish_hot_reload_e2e`、`worker_m2::pricebook_hot_reloads_on_new_epoch`、`worker_nats::epoch_broadcast_hot_reload`、`console_ops::cache_flush_pricebook_hotfix`；`gateway_first_epoch`（09-06 第十轮，分支 `settle-backlog`，临时空库：空 `pricing_epochs` 读作 0，首次发布 epoch 1 判得出"更新"） | — |
| 结算积压上界（`state.rs::check_settle_backlog` / `settle_write`） | 排队 + 在写 PG 的结算数超过 `OKAPI_SETTLE_BACKLOG_MAX`（缺省 20000，0 不设限）→ 数据面鉴权前 503 `overloaded`，不预扣不碰上游；回落到 3/4 以下恢复（滞回）；进出各一条 WARN，TraceLayer 不给 503 刷 ERROR | B D G | `gateway_backlog`（09-06 第十轮，分支 `settle-backlog`：占满 `settle_gate` 后第三笔 503、param = 积压数、余额与预扣分文未动、上游未被打到；放开闸后积压归零、三笔全部落账、余额吻合；0 = 不设限）；release loadgen 复测见第 4 节第十轮 | 单进程口径；多副本各自计数（与 `settle_gate` 同为 per-pod） |
| `error.rs` | `AppError` → OpenAI / Anthropic / Google 三种错误壳 | A E | `gateway_messages::messages_json_and_error_envelope`、`gateway_gemini_ingress::errors_are_google_rpc_status_shaped`、`console_manage::malformed_query_string_is_rejected_as_error_code` | — |
| 优雅下线（`shutdown.rs`） | SIGINT / SIGTERM → 关监听 → 在途连接排完 → 等后台结算归零 → 退出 | A B D | `gateway_shutdown`（09-06 新增，真实二进制子进程：首块后 SIGTERM，流完整到 `[DONE]`、新连接被拒、退出码 0、账 committed 且无悬置预扣） | SSE 排水无 5min 上限（交编排层 grace period）；无独立 readiness 端点（关监听即等价） |

### 2.3 console 角色（`bins/okapi/src/console`）

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `auth_web.rs` | 注册 / 登录 / TOTP / 兑 key / 会话列举吊销 / 会话数上限（§11.37，`settings.web_session_limit`，踢最早）/ 邮箱验证码 / 找回密码 / 关键接口限流 | A C | `console_auth_web`（含 `session_limit_evicts_oldest`：上限 2 连登三次，最早 cookie 兑 key 401、后两条有效、列表两条并回 `limit`）、`console_smtp`（验证码、重置、无 SMTP 501）、`console_audit::login_attempts_are_audited`、e2e smoke（登录 / 登出清 session / session 降级） | OAuth 回调走同一 `open_web_session`，靠编译期同构，无 OAuth 路径的上限用例 |
| `oauth.rs` | 通用 OAuth2 / OIDC | A C | `console_oauth`（mock IdP 授权码全流程）；`console_oauth_presets`（09-06 第八轮，独立临时库）：github / discord / linuxdo 三预设的 scopes 进授权跳转、数字 / snowflake `id` 作绑定键、`login` / `username` 作展示名与首登用户名；改名不换账号、同名不同 id 是另一个账号且用户名加盐、缺 `id` 拒绝不落绑定、token 端点 500 → `oauth_upstream_error` param `status_500` | — |
| `registration.rs` + `auth_web::verify_turnstile` | 注册策略、邀请赠送、Turnstile | A C | 单元；`console_auth_web::registration_policy_gates_signup`；`console_turnstile`（09-06 第七轮，独立临时库 + 本地 siteverify mock：缺 token / 校验失败 / 端点不可达三种 param、表单体 `secret=…&response=…`、撤掉秘钥即关闭） | — |
| `setup.rs` | 空库首启向导 | A | `console_setup`（独立临时库） | — |
| `portal.rs` | `/api/me/*`（key、日志、流水、订单、公开价格、公告） | A C | `console_portal`、`console_portal_pages`、`console_stats::personal_activity_covers_calendar_year_and_isolates_owners`、e2e smoke 门户页 | — |
| `playground.rs` | Playground 同源流式中继 `/api/me/playground/chat`（进程内调数据面处理器、强制 stream、1MB 上限）+ 站点预设公开读（§11.39） | A B C | `console_playground`（SSE 与直打数据面一致且账落同一把 key、无 key 401、超限 413、预设白名单收口）；单元 `force_stream` / `sanitize_presets` | — |
| `channel_oauth.rs` | 订阅 OAuth 登录两步（`/admin/channels/oauth/start` / `exchange`，§11.38）：PKCE 状态 Redis 一次性、换码、建渠道 / 追加 key、审计 `channel.oauth_login` | A C | `gateway_oauth_channels`（三例均经此建渠道） | 无真实上游端到端（mock 授权服务器）；Antigravity / Grok 不在范围 |
| `manage.rs` / `admin.rs` / `query.rs` / `cloud_probe.rs` | 六类管理面 CRUD、批量、写校验（azure / bedrock / vertex 地址、aws_region、出站 / 注入字段）、路由诊断、bedrock / vertex / anthropic_max / codex 测活与模型发现 | A C | `console_manage`（含 `cloud_channel_write_validation`：两家缺地址 400、vertex 地址形状、aws_region 形状、只改地址仍校验）、`console_m2`、`console_users`、`console_visibility`（属主范围 / 分组矩阵）、`console_pricing_write`、`console_channel_test`、`console_import`、`console_diagnose`、`gateway_pricing_rules::console_rule_crud_and_validation`、`console_cloud_probe`（09-08 第十九轮，3 例）：vertex 两种探测范围各换一次 token（model 范围还真发 generateContent）、token 端点 401 带出上游状态与原文、留痕回填"最近测试"；bedrock 按凭证形态分流（Bearer 列兼容模型 / SigV4 签 InvokeModel 且模型 ID 冒号编 `%3A`）；anthropic_max 与 codex 的"未到期零往返 / 过期先刷新 / refresh 轮转回写 / model 范围真发补全"；三家的 `fetch_models` 各自回 `fetch_models_unsupported` 或 `fetch_models_requires_sigv4` | bedrock 的 SigV4 验凭证与拉模型走 `ListFoundationModels`，控制面主机在 `bedrock.rs` 里固定成 `bedrock.<region>.amazonaws.com`（有意为之），mock 接不上——那条分支只有 `aws_sigv4` 单测与真实凭证能验 |
| `channel_balance.rs` | 上游余额查询（§11.33）：按主机选探针、定点解析、`ch:balance` 留痕 | A | `console_channel_test::channel_balance_probe`（dashboard 口径额度 − 美分用量、凭证错 502 `status_401`、anthropic 400 `balance_unsupported`、列表 `last_balance` 回填）；单元：探针选择 / URL / 四家官方响应形状 / 十进制解析 | DeepSeek / SiliconFlow / OpenRouter / Moonshot 四家按 `api_base` **主机名**硬匹配选探针，mock 只能起在 `127.0.0.1` 上、永远落进 `OpenAiDashboard` 分支——结构上做不了 mock 端到端（09-08 第十九轮核实），不是疏忽 |
| `margin.rs`（+ `crate::margin`） | 负毛利熔断列出 / 解除（§11.34） | A C | `worker_margin_breaker`（列出含渠道名与 active、lift 后同进程立即放行且审计 `margin.lift`、解除期评估器跳过） | — |
| `ratio_sync.rs` | 上游倍率在线同步（§11.36）：三种源形状识别、逐模型逐轴差异、择项应用 | A B C | `console_ratio_sync`（ratio_config / new-api pricing 两源 + 非 JSON + 不可达：`current` / `same` / 缺失三态、按次与倍率不混比、单源失败不阻塞、源数与重名 400；apply 只改选中轴其余保持本地值、按次价 → micro、审计 `pricing.sync_apply`、非法轴 / 负值 400）；单元：三种形状解析、micro ↔ USD 字面量、`1.250000 == 1.25` 规范化、十进制不经浮点 | Okapi `/api/pricing` 形状只有单测；出站走 `ssrf::validate_api_base` 同一把闸 |
| `analytics.rs` / `stats.rs` / `logs.rs` / `usage_details.rs` / `activity.rs` / `analysis_*` | CH 立方体三端点、看板、日志检索、实时 KPI、毛利 | A B | `console_analytics`、`console_stats`、`console_logs`、`gateway_upstream_cost`；单元 `activity` / `analysis_freshness` / `usage_details` | `console_analytics` 两例曾在全量并行下偶发（outbox 行被别的进程 drain、两张 MV 先后落地），09-06 改为 `poll_until` 全字段谓词，见第 4 节发现 ① |
| `audit.rs` | 管理写操作 + 登录审计 | C | `console_audit`、`console_ops::assist_overview_scoped_and_audited`、`console_mcp_write`（`mcp:{key_id}` 落痕） | — |
| `dlq.rs` | 死信列表 / 重投 / 丢弃 | A D | `console_logs::dlq_list_requeue_and_discard`、`worker_ch::chsink_pipeline_then_dlq`、e2e smoke 运维页 | — |
| `mcp.rs` | MCP Streamable HTTP 只读 + 写工具三道闸 | A C | `console_mcp`、`console_mcp_write` | — |
| `pay.rs` | epay / Stripe 下单、回调验签、重放幂等、返利 | A B C | `console_pay`、`console_subscriptions::checkout_callback_and_redeem_activate` | Creem / 官方支付宝微信不在范围 |
| `subscriptions.rs` | 套餐 CRUD、购买、配额窗、订阅池优先扣 | A B | `console_subscriptions`（Lua 契约、网关优先扣、worker 滚窗、回调激活、校验）、e2e `subscriptions.spec`（接口桩） | 升降级 / 多订阅并存 backlog |
| `teams.rs` | 建团 / 成员限额 / 团 key / 分账 | A B C | 单元；`console_teams` 全生命周期 | — |
| 兑换码（manage + portal） | 批量生成、核销、绑用户、限 IP | A C | `console_redemption`（并发恰一成功、过期拒绝、IP 上限）、e2e `redemptions.spec` | 多次核销 `max_uses` backlog |
| `ssrf.rs` | 上游 URL 校验：`api_base`、`settings.oauth_token_url`、Vertex 服务账号 `token_uri`（凭证三个写入口） | C | 单元；`console_ssrf`（三种地址各自的 400 参数与放行） | — |
| SPA 托管 / 内容协商 | `/admin/*` 同挂 API 与 SPA | A | `console_spa_navigation` | — |
| 公告 / settings | `site_notice` 公开端点、设置读写 | A | `console_portal_pages::public_notice_whitelists_and_gates`、`console_ops::settings_get_and_leaderboard`、e2e smoke 公告 | — |
| `playground.rs` | Playground 同源流式中继 + 站点预设（§11.39） | A B C | `console_playground`（中继强制流式回 SSE、逐块透出与记账落同一 key、无 key 401、超 1MB 413；`GET /api/playground/presets` 白名单收口）；单元 `force_stream` / `sanitize_presets` | 中继直调数据面处理器，鉴权 / 限流 / 计费全部复用 gateway，不另测 |

### 2.4 worker 角色、mail、migrate

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `worker/chsink.rs` | outbox → CH 批写、去重、DLQ 终态 | A D | `worker_ch`、`worker_nats` | — |
| `worker/nats_relay.rs` | outbox → JetStream → chsink | A D | `worker_nats` | — |
| `worker/notify.rs` | webhook / email 多路、事件过滤、频率闸、余额低扫描 | A | `worker_notify`、`console_smtp::notify_email_channel_and_admin_test_send` | — |
| `worker/mod.rs` | 悬置清理、三方对账、分区维护、冷却恢复、余额有效期、保留策略、订阅滚窗 | A B D | `worker_m2`、`worker_reconcile_repair`、`console_subscriptions::worker_rolls_window_and_expires` | — |
| `worker/margin_breaker.rs` | 负毛利熔断评估（§11.34）：CH 成本已知行按分组×渠道聚合 → `mb:blocks` | A B D | `worker_margin_breaker`（有 CH 才跑：25 笔亏损样本 → tripped 含金额 / 毛利率、网关 503 `margin_blocked`、续期不重复通知、关闭功能清表）；单元 `margin::tests`（阈值边界：样本不足 / 成本过小 / 收入 0 / 负阈值容忍 / 正阈值要求毛利、配置夹取、字段往返） | 通知载荷 09-08 从 worker 主循环收进 `margin_breaker::evaluate_and_notify`（生产与用例同一段代码），但投递断言仍缺：`settings.notify_channels` 是全局键，在共享开发库上写它会与 `worker_notify` 互相覆盖，要补得先给该用例一套临时库 |
| `mail/` | SMTP 投递、模板 | A | 单元；`console_smtp`（本地 mock SMTP，AUTH PLAIN） | STARTTLS / 隐式 TLS 未在 mock 覆盖 |
| `migrate.rs` | new-api / 老 ok-api JSONL 导入 | A H | 单元；`migrate_newapi`、`migrate_okapi_old`、`schema_shape` | — |

### 2.5 前端（`frontend/src/features`）

| 功能面 | 路由 | 维度 F 子项 | 覆盖 spec | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| 登录 / 会话 / 权限裁剪 | `/`、`/portal/*` 守卫 | 双登录方式、403 不白屏、导航按权限裁剪、登出清服务端 session | `smoke.spec`（4 例） | — |
| 找回 / 重置密码 | `/forgot-password`, `/reset-password` | 登录页链接带邮箱、提交体 email+lang、防枚举成功态、未配 SMTP 501 文案；缺 token 提示、长度与一致性前置校验、成功回登录、失效 token 400 文案 | `write-forms.spec`（2 例，09-06 新增） | — |
| 新手引导 | `/portal` 快速开始卡 + 顶栏入口 + 密钥页页头 | 进度推导、四步抽屉、客户端片段联动、关闭记忆、移动端与深色 | `guide.spec`（4 例，09-06 新增） | — |
| 试用台 / 聊天客户端一键导入 | `/portal/playground`、密钥回执 → 指南 | 模型只列本分组可用、发送经同源中继且强制流式、流式内容 + usage / 模型脚注、停止按钮中断、预设保存 / 载入 / 站点预设导入、cc-switch（Claude 不带 /v1、Codex 带 /v1）/ NextChat / Cherry Studio 链接形状 | `playground.spec`（4 例，09-06 新增） | 助手正文纯文本渲染，无 markdown |
| 门户总览 / 日志 / 流水 / 充值 | `/portal`, `/portal/logs`, `/portal/ledger`, `/portal/topup` | KPI 六卡、页签零请求、空态、导出禁用、流水入口 | `smoke.spec`（2 例）、`charts.spec`（门户图表 5 例）、`interactions.spec`（年度日历 / 热力图 / 个人中心 4 例） | 充值下单跳转支付页无 e2e（有后端 `console_pay`） |
| 公开模型广场 / 调用示例 | `/pricing` | 厂商归一、单位切换、深链、阶梯价、模拟器、移动端深色、分页、加载 / 失败 / 空态 | `smoke.spec`（1 例）、`catalog.spec`（10 例）、`request-examples.spec`（4 例） | — |
| 管理端总览 / 日志 / 洞察 / 质量 / 经营 / 审计 / 运维 | `/admin`, `/admin/logs`, `/admin/stats`, `/admin/quality`, `/admin/revenue`, `/admin/audit`, `/admin/ops` | 实时条、健康芯片、深链即状态、三视图、死信签 | `smoke.spec`（管理端 1 大例）、`charts.spec`（管理图表 6 例） | 需演示超管，缺则跳过 |
| 管理端设置 / 高级配置 / 导航 / 分页 | `/admin/settings`, 侧栏, 列表页 | 分组搜索、敏感值不显示、只读无编辑入口、键盘 / 移动端 / IME、URL 即分页状态 | `interactions.spec`（19 例） | — |
| 用户 / 密钥 | `/admin/users`, `/portal/keys` | 搜索回车、抽屉落地签、删除二次确认手输名称 | `smoke.spec`（管理端大例内的用户抽屉段 + 删除二次确认 1 例） | — |
| 用户抽屉写操作 | `/admin/users` 管理抽屉 | 入账 USD → micro 整数（含 0.29 浮点边界）、系数按十进制字符串提交且负数 / 未改动不放行、分组全量覆盖且先出现者优先级高、封禁经确认框且成功后翻成解封；角色只发改动的那一项、订阅下拉只列在售订阅套餐且发放 / 立即结束各打端点、余额有效期日期 → UTC 零点 RFC3339 且清空发 null | `write-forms.spec`（2 例，09-06 新增 / 第八轮） | — |
| 模型定价抽屉与发布 | `/admin/pricing` 编辑 / 新建 / 发布 | 七个倍率轴按十进制字符串提交、空档位行过滤、`tier_expr` 去空格回传且模式提示随之切换、无档位不发 `tier_ratios` 键、降级链原样回传、编辑态模型名只读；发布按钮 POST `/admin/pricing/publish` 并提示新 epoch | `write-forms.spec`（09-06 新增 / 第八轮） | 模型没有状态切换 UI（`status` 只随导入 / 删除变化），此前备注有误 |
| 兑换码 | `/admin/codes` | 分页 / 筛选复位 / 末页停用；生成抽屉：面值 USD → micro（0.29 边界）、绑定用户去空格转数字、空限额不发键、过期时间按浏览器本地换 UTC、面值 0 禁提交、400 错误码文案且可重发、成功态明文一次性 + 复制全部 | `redemptions.spec`；`write-forms.spec`（1 例，09-07 第十二轮） | — |
| Playground 试用台 + 一键导入 | `/portal/playground`、密钥回执 | 模型下拉只列本分组可用、发送 → 流式内容 + usage 脚注、停止按钮中断、预设保存 / 载入 / 站点预设导入、密钥回执四个客户端导入链接形状 | `playground.spec`（4 例，SSE 桩） | 流式桩为一次性回包（Playwright 限制），逐字动画不逐块验证 |
| 订阅套餐 | `/portal/plans` | 在售 / 已订阅高亮 / 停用说明 / 下单参数 | `subscriptions.spec`；管理端见下「套餐抽屉」「套餐删除」两行 | — |
| 渠道抽屉「请求与计费行为」 | `/admin/channels` 编辑抽屉 | 已有 proxy / 额外头回显；注入字段按 JSON 解析（数字 / 带引号字符串）；清空额外头即从 settings 删键；PATCH 体只含有值的键；受保护键 400 → 错误码文案且抽屉不关 | `write-forms.spec`（1 例，09-06 新增） | — |
| 渠道抽屉接入 / 模型 / 调度 + 新建 | `/admin/channels` | 协议只读；凭证轮换独立端点且成功后清空；拉上游模型覆盖清单并提示数量；成本倍数 → 千分比、留存声明、优先级随 PATCH；池成员单独保存、覆盖值整数化、非整数归 null；新建三件必答事齐才放行、池成员随建渠道提交；key 级参数行权重 / 并发各自 PATCH（空并发 = null）、失效 key 重新启用 | `write-forms.spec`（2 例，09-06 第七 / 八轮） | — |
| 套餐删除 | `/admin/plans` | 确认框 → `DELETE /admin/plans/{code}` | `write-forms.spec`（09-06 第八轮） | — |
| 安全页会话卡 | `/portal/security` | 列表 + 当前浏览器徽章、单条吊销打 `DELETE /api/me/sessions/{sid}`、全部吊销打 `DELETE /api/me/sessions`、空态文案 | `write-forms.spec`（1 例，09-06 新增） | —（TOTP 绑定见下行，第六轮已覆盖） |
| 套餐抽屉 | `/admin/plans` 编辑 / 新建 | 充值模板与订阅两形态字段互斥（切换即替换字段区）、USD → micro、天数 `Math.trunc`、空值不发键、订阅缺有效期禁用保存、售价空 = 0 不售卖、编辑态代码锁定 | `write-forms.spec`（1 例，09-06 第四轮） | —（删除见上「套餐删除」行，第八轮已覆盖） |
| 角色抽屉 | `/admin/roles` | 权限点来自 `/admin/permissions`、整组切换、无权限点禁用创建、编辑态 code 锁定且已有权限预勾、删除经确认框、后端 409 `role_in_use` 渲染成文案 | `write-forms.spec`（1 例，09-06 第四轮） | — |
| 价格分组抽屉 | `/admin/groups` | 倍率字符串去空格、池从 `/admin/pools` 选、`PoolReach` 就地可达、自选开关、编辑态分组码只读、内置默认组删除禁用、新建缺省倍率 1 / 池 default | `write-forms.spec`（1 例，09-06 第六轮；09-07 第十五轮补限流字段：回显、负数 / 小数 aria-invalid 禁保存、清空发 null、整数原样，列表列 `60 / 分 · ∞ / 时` 与两边不限 `—`） | — |
| 渠道列表余额按钮 / 运维页毛利熔断卡 | `/admin/channels`, `/admin/ops` 毛利熔断页签 | 钱包按钮只对 openai / openai_compat 显示、结果 toast 按上游货币 Intl 格式化、"最近测试"列下回填余额、`balance_shape` 等错误码文案；熔断卡配置表单（美元 → micro、百分比 → 万分比含负号、分 → 秒、时 → 秒，非法数字禁保存、未改动禁保存）、负毛利行标红、解除只带分组 × 渠道定位对并提示到期 | `write-forms.spec`（2 例，09-07 第十四轮） | — |
| 计费规则抽屉与列表 | `/admin/rules` | 编辑态四类字段回填与 code 锁定、按类型只发该类型字段、阈值 USD → micro、星期勾选升序、空范围不发键、上下线打 toggle 且提示需发布、删除经确认框 | `write-forms.spec`（1 例，09-06 第六轮） | — |
| 设置 SMTP 卡 | `/admin/settings` 邮件页签 | 单键回显、去空格、`reply_to` 空转 null、端口越界归零、加密方式分段、未保存前测试禁用、测试信按已保存配置发且收件人须含 @、有草稿时禁发 | `write-forms.spec`（1 例，09-06 第六轮） | — |
| TOTP 绑定 | `/portal/security` | 开始绑定拿 otpauth / pending、码不足 6 位禁用、错码 `totp_invalid` 文案可重试、成功切已开启态、无会话 401 降级提示 | `write-forms.spec`（1 例，09-06 第六轮） | — |
| 渠道池抽屉与列表 | `/admin/pools` | 策略 / 降级目标回填、降级目标排除自己、不降级发 null、编辑态池码只读、内置池与被引用池删除禁用、删除经确认框 | `write-forms.spec`（1 例，09-06 第七轮） | — |
| 团队 | `/portal/teams` | 建团名字去空格、成员上限 USD → micro 且空即 null、提交后表单复位、发团 key 明文只展示一次、列表 401 整页降级且隐藏创建入口 | `write-forms.spec`（1 例，09-06 第七轮） | — |
| i18n | 全站 | 裸文案零、双语言包键对齐 | `guard-i18n.sh`、`guard-i18n-keys.py`；`guard-error-codes.py`（09-07 第十二轮：后端 `codes::*` + `AppError::new / unauthorized` + `StoreError::Conflict` + 模块级 const 的字面量全集 → 两语言包 `errors` 命名空间反向核对，进 CI）；e2e 断言同时匹配中英正则 | — |

### 2.6 部署与性能

| 项 | 维度 | 覆盖 | 缺口 / 备注 |
| --- | --- | --- | --- |
| `okapi all` 单机形态 | I | `scripts/smoke-all.sh` 四断言 | — |
| embed-web 发布构建（`deploy/Dockerfile` 的运行时前提） | I | `scripts/verify-deploy.sh`（09-06 第六轮）：`--features embed-web` 构建；在没有 `frontend/dist` 的目录起 console，首页、`/assets/*.js`（text/javascript）、`/admin/users` 深链（Accept: text/html）都从二进制服出，同路径 JSON 请求仍是 API 401 | — |
| 发布镜像（多阶段 Dockerfile） | I | `OKAPI_VERIFY_IMAGE=1 bash scripts/verify-deploy.sh`（09-06 第七轮）：从 `git archive HEAD` 干净快照 `docker build`，`okapi --version` 可执行，对临时空库起 console：healthz、内嵌前端、首启迁移 + Setup 向导、以 65534 运行 | 构建约 5 分钟，不进默认路径；需本机 docker |
| compose 双 profile / k8s manifests | I | `scripts/guard-deploy-manifests.py`（09-06 第六轮）：文档结构、Service selector ↔ Deployment、容器 image / resources、对外容器 `/healthz` readinessProbe、gateway `terminationGracePeriodSeconds` 与应用服务 `stop_grace_period` ≥ 330s（§14.3 排水口径）、Σ(副本上限 × OKAPI_PG_POOL) ≤ 200、依赖镜像来源与 healthcheck | 无 kubectl / compose 插件，不做 schema 级校验 |
| Nginx SSE 模板 | I | 手工 | — |
| 缩尺压测 / Linux 复测 | G | `docs/perf-report.md`（2026-08-30）；09-06 第九轮在 HEAD `018d963` release 上复跑 baseline / json / stream / c=1 四档，无回归，同时暴露结算积压问题（第 3 节第 8 条） | 裸金属正式复测、10 万 SSE 整数口径待办；本机负载高时同档两跑相差 2.5 倍，只能看方向 |

## 3. 覆盖缺口清单（按风险排序）

1. ~~PG 记账不幂等~~ **已修**（09-06 第五轮）：`docs/database.md` §1.5 定案「每 request_id 恰一行」，`record_settlement` 事务开头 `SELECT EXISTS` 幂等闸，重放整笔跳过并告警；`pg_settlement::replaying_a_settled_request_writes_nothing` 钉住。ledger 的 Lua 与 PG 契约至此都有直测。
2. ~~前端写操作表单 e2e~~ **已收口**（09-06 八轮，`write-forms.spec` 共 17 例覆盖全部管理面与门户写表单，含各段零碎）。
3. ~~SIGTERM 优雅下线无自动化用例~~ **已补且修了实现**（09-06 第三轮，`gateway_shutdown`；见第 4 节发现）。凭证刷新锁按 §4.3 定案不适用于当前 static_key 主线。剩余：SSE 排水无 5min 上限（依赖编排层 grace period）。
4. ~~mid-stream 断流语义无专项用例~~ **已补**（09-06 第三轮，`gateway_midstream`）。
5. **集成测试共享一条 `billing_outbox` 队列**：任一用例的行都可能被别的测试进程 drain 进 CH，因此「drain 后直接读 CH 并断言」天然有竞态。现行约定是走 `poll_until` 且谓词覆盖全部待断言字段（09-06 修了两处漏网的）；新增 CH 用例须照此写，或改为按 user_id 隔离的 drain。
6. ~~部署形态不在常规回归~~ **已补**（09-06 第六、七轮，`verify-deploy.sh` + `guard-deploy-manifests.py` + 可选镜像阶段）。性能维度 09-06 第九轮已按需跑过一次（无回归），仍不进默认路径。
7. ~~OAuth 仅单一 mock IdP~~ **已补**（09-06 第八轮 `console_oauth_presets`）；~~Turnstile 外呼无 mock~~ **已补**（第七轮 `console_turnstile`）；SMTP TLS 形态未覆盖（mock SMTP 只走明文 AUTH PLAIN，STARTTLS / 隐式 TLS 需要带证书的 mock，且客户端得有可配的信任锚——暂列不做）。
8. ~~后台结算积压无上界~~ **已修并合入 main（09-06 第十轮，`4e411ce`）**。第九轮压测发现：`settle_gate` 只钳制同时碰 PG 的任务数不限排队深度，本机 PG 落账约 1000 笔 / 秒而进量 4.7k–11.7k RPS，8 分钟后堆了 260,101 笔"Redis 已扣、PG 未记"的结算，SIGTERM 等满 30s 即放弃，PG 只落 166k / 426k 笔。定案走方案 A（有界 + 数据面拒绝）：`OKAPI_SETTLE_BACKLOG_MAX`（缺省 20000 ≈ 记账速率 × 30s 下线窗口，0 不设限）超界后鉴权前 503 `overloaded`，滞回恢复，IMPLEMENTATION §12.2 / §12.3 / §14.3 与 `docs/perf-report.md` 先行改定，压测口径分"网关自身开销（=0）"与"可持续吞吐（缺省）"两种。方案 B（持久结算队列 / 微批组提交）仍是 §11.23 挂着的终态方向。对账 1.5s 双采样窗口的误判风险随之收敛到最长约 20s，积压期间不要手动修复（已写进 §12.3）。
9. ~~临时库套件从不删库~~ **已修并合入 main（09-06 第十轮，`575f630`）**：`console_setup` / `console_ssrf` / `console_oauth_presets` / `console_turnstile` 各建独立库不清，开发 PG 里一天攒了 157 个；用例末尾 `DROP DATABASE … WITH (FORCE)`，本机残留已手工清空。新写临时库用例照 `schema_shape` / 上述四个的收尾。
10. ~~出站请求跟随重定向绕过 SSRF 闸~~ **已修（09-07 第十四轮，`f737892` + `8a92e2a`）**：`ssrf::validate_api_base` 只校验管理员填进来的那个 URL，而所有出站 reqwest client（`okapi_providers::http::build_client` 共享池 + `ratio_sync` 自建）都按缺省跟随 30x，公网地址一跳重定向就能把请求引到私网 / 云元数据地址（DNS rebinding 文档里已列 backlog，重定向此前没人提）。`ratio_sync::fetch_one` 自建 client 改 `Policy::none()`（`f737892`）；`HttpPool` 加一族不跟随重定向的探针 client，`PassUpstream::probe` 走它，测活 / 拉模型 / 余额 / Turnstile / OAuth userinfo / 支付七处管理面调用点换过去（`8a92e2a`，`console_channel_test::channel_test_does_not_follow_redirects` 钉住：上游 302 → 拿到 302 本身、目标零命中）；数据面透传保留缺省（下载类端点依赖上游 302 到 CDN）。~~未换的：订阅 OAuth 换 token / 刷新、Bedrock 列模型、Vertex 换 token~~ **第十八轮统一换成探针 client**，顺带发现 Vertex 服务账号 JSON 的 `token_uri` 从未过闸（见第十八轮）。仍走数据面 client 的只剩 bedrock / vertex 的按模型测活（16 token 补全，与真实请求同一条路）。
11. **自用订阅凭证（anthropic_max / codex）的合规边界（09-07 第十四轮 review 备注，不改代码）**：出向会前置 Claude Code 系统提示首句、合并 `claude-code-20250219` 等 beta、转发客户端身份头，本质是让上游把网关流量当成 Claude Code / Codex CLI。README 已标"实验性 / 自用"、"明确不做"里写了不做订阅账号池转售；但一旦这类渠道被放进对外分组，就是拿订阅额度转售，违反两家的使用条款且会被封号。建议在渠道抽屉与文档里把"仅限本人 / 内部分组"写成硬约束（例如 OAuth 渠道不允许绑定可注册用户可见的分组），至少在清单里挂着。另注：系统提示前置会改变非 Claude Code 客户端拿到的模型行为，属该 provider 的已知语义。

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

复核：计费三 crate 55 / 55；全量 Rust 97 个测试二进制 447 / 447（结算路径多一次点查未影响任何既有用例）；clippy `-D warnings` 干净；`.sqlx` 重生成。已提交 `02ee40b`。

### 2026-09-06 第六轮：部署形态进回归 + 四块写操作表单

| 新增 | 内容 | 结果 |
| --- | --- | --- |
| `scripts/guard-deploy-manifests.py`（第五道守卫） | compose / K8s 结构断言：文档三要素、Service selector 对得上 Deployment、容器 image 与 resources、对外容器 `/healthz` readinessProbe、gateway `terminationGracePeriodSeconds` 与应用服务 `stop_grace_period` ≥ 330s、Σ(副本上限 × OKAPI_PG_POOL) ≤ 200（当前 152）、依赖镜像来源与 healthcheck | 通过（PyYAML 缺失时借系统 ruby 解析） |
| `scripts/verify-deploy.sh` | 模板守卫 + `cargo build --features embed-web`（独立 `target/embed-web`）+ 在无 `frontend/dist` 的临时目录起 console：首页 / `/assets/*.js` text/javascript / `/admin/users` 深链回 SPA / 同路径 JSON 仍 401 | 通过；首跑约 1 分钟，增量 13s |
| `deploy/k8s/okapi.yaml`、`deploy/docker-compose.yml` | gateway 加 `terminationGracePeriodSeconds: 330`，应用服务锚点加 `stop_grace_period: 5m30s` | 第三轮让进程真的响应 SIGTERM 之后，编排层缺省的 30s / 10s 宽限期会把排水又变回硬杀——这是那次修复的配套，此前遗漏 |
| `frontend/e2e/write-forms.spec.ts` +4 | 价格分组抽屉、计费规则抽屉与列表（含 toggle / 删除）、SMTP 卡、TOTP 绑定（含 401 降级） | 4 / 4 通过；分组用例首跑因桩缺 `/admin/pools/{code}` 详情形状触发前端错误边界，补桩后通过——不是产品缺陷，真实接口有该形状 |

复核：interactions 配置 71 / 71；oxlint / tsc 干净；五道守卫全过。已提交 `2117b4a`（首次提交误把并行会话的分组限流半成品 `git add -A` 进来，软回退后只重新暂存本轮 7 个文件）。

### 2026-09-06 第七轮：发布镜像真跑 + Turnstile mock + 写表单收口

| 新增 / 修正 | 内容 | 结果 |
| --- | --- | --- |
| 发布镜像构建（`git archive HEAD` → `docker build`） | 首跑在前端阶段失败：`@/features/logs/LogsPage` 不存在——`frontend/.gitignore` 的 `logs` 规则把 `src/features/logs/` 整个目录挡在版本库外，门户日志页从未提交过，本机能跑、fresh clone / CI / 镜像全挂。改为 `/logs`（只忽略根目录日志）并把 `LogsPage.tsx` 入库；顺手扫了全部 `@/` 导入，无第二处 | **修复** |
| 同上，第二跑 | 镜像能建，`okapi --version` 报 `GLIBC_2.38 not found`：`rust:1-slim` 已跟到 Debian trixie，运行阶段是 bookworm-slim。builder 钉到 `rust:1-slim-bookworm` | **修复**；重建后 `okapi 0.1.0`、console 对临时空库起立、内嵌前端、Setup 向导、uid 65534 |
| `scripts/verify-deploy.sh` 增 `OKAPI_VERIFY_IMAGE=1` 阶段 | 上面两步自动化：干净快照构建、`--version`、临时库 console 冒烟（healthz / SPA / setup 状态 / 非 root）、用完删库 | 通过 |
| `bins/okapi/tests/console_turnstile.rs` | `settings.turnstile_verify_url` 覆写 siteverify 地址（写入 `docs/database.md` 注册表）；独立临时库 + 本地 mock：缺 token → `turnstile_token`、校验失败 → `turnstile_failed`、端点不可达 → `turnstile_unreachable`、表单体 `secret=…&response=…`、撤秘钥即关且不外呼 | 1 / 1 一次通过 |
| `frontend/e2e/write-forms.spec.ts` +3 | 团队（建团 / 成员上限换算 / 发 key 一次性 / 401 降级）、渠道池抽屉与列表、渠道抽屉接入 / 模型 / 调度页签 + 新建 | 3 / 3 通过；渠道池用例首跑把 `/admin/pools` 导航请求也当接口回了 JSON，路由桩加 `isNavigationRequest` 放行；渠道抽屉用例被右下角 toast 堆叠盖住"取消"，改 Esc 关抽屉 |

复核：interactions 配置 74 / 74；oxlint 干净；Rust 侧因并行会话的半成品（`console/admin.rs` `similar_names`、新文件 `channel_balance.rs` `struct_field_names`）全工作区 clippy 暂不能绿，豁免这两条后本轮改动干净；两条 lint 属对方待收口项。已提交 `4497934`，并从 HEAD 快照重建镜像复核通过。

### 2026-09-06 第八轮：OAuth 预设 + 写表单零碎收口

| 新增 | 内容 | 结果 |
| --- | --- | --- |
| `bins/okapi/tests/console_oauth_presets.rs`（独立临时库） | 三预设只配 client 凭证 + 指向 mock 的 URL，字段名与 scopes 走 `preset()`：授权跳转带预设 scopes；GitHub 数字 `id`、Discord snowflake、LinuxDO 数字都转成字符串作绑定键，`login` / `username` 作展示名与首登用户名；**身份稳定性**：改 handle 不换账号（展示名保留首登审计值）、同 handle 不同 `id` 是另一个账号且用户名撞了加盐、userinfo 缺 `id` 拒绝且不落绑定、token 端点 500 → `oauth_upstream_error` / `status_500` | 2 / 2 通过（一处断言按 RFC 3986 修正：`:` 留在 query 里不转义） |
| `frontend/e2e/write-forms.spec.ts` +2 | 用户抽屉角色 / 订阅 / 余额有效期三段；渠道 key 级参数、套餐删除、模型页发布 | 2 / 2 通过；`write-forms` 累计 17 例 |

复核：oxlint 干净；interactions 配置 75 / 76——唯一失败是既有的 `interactions.spec 列表分页` 用例：并行会话给渠道行新增的 `LastBalance` 组件在夹具缺 `last_balance` 字段时崩掉整行，属对方特性引入的回归，应随其特性一起把夹具补上或让组件容忍 `undefined`；本文件的 17 例均通过。SMTP TLS 与性能维度维持"按需 / 不做"。

### 2026-09-06 第九轮：已提交 HEAD 的隔离全量 + 性能对照

目的：工作树混着并行会话约 84 个未提交文件（含一处编译错误），前八轮的 Rust 侧全工作区 clippy / 测试都是带豁免跑的。本轮用 `git worktree add --detach /tmp/okapi-head HEAD`（`018d963`）+ 独立库 `okapi_head` + Redis 逻辑库 7，对**已提交状态**做一次干净验证，再用 release 构建对照 `docs/perf-report.md` 基线看今天的热路径改动（结算幂等点查、后台结算计数）有没有拖慢。

| 层 | 结果 | 数字 | 说明 |
| --- | --- | --- | --- |
| L0 rustfmt / 五道守卫 | 通过 | i18n 键 1377、前端权限点 9、部署模板 Σ 池 152 / 200 | — |
| L0 cargo-deny | licenses 仅 7 个自有 crate `unlicensed` | — | 与前几轮一致，等 §15 许可证定案 |
| L0 clippy `-D warnings --all-targets` | **通过（全工作区、无豁免）** | — | 并行会话的两条 lint 与 `hmac::KeyInit` 编译错误都不在 HEAD 里，证实是对方未提交半成品 |
| L1 + L2 全量 | 首跑 197 / 450，253 失败全是 `Migrate(VersionMissing(5))` | — | 环境问题：shell 早前导出的 `DATABASE_URL` 盖住了 worktree `.env`，测试连到被并行会话迁到 0005 的主库。已写进第 1 节注意事项 |
| L1 + L2 全量（显式 `DATABASE_URL`） | 436 / 450 | 14 失败 | 全部是 Redis 串味：`okapi_head` 的小整数 uid 与主库用户共用 `bal:{uid}`（如 `images_insufficient_for_batch` 期望 300000 实得 299460；`newapi_sample_migration_full_check` 换算多出 7000） |
| 14 例所在的 13 个套件（Redis 逻辑库 7） | 12 / 14 恢复 | 余 2 | `entity_usage_batches_by_ids`、`admin_refund_full_cycle` 只剩 CH 串味（CH 库名写死 `okapi`，两套 PG 的 api_key / user id 在同一张聚合表里相加）；两例今日早先在主库上均通过，代码未变 |
| L6 性能（release，同机 Docker 四容器，负载均值 15、Docker VM 340% CPU） | 无回归 | 见下 | 机器很吵，同档两跑相差 2.5 倍，只做方向判断 |

性能数字（loadgen，与 08-30 基线同机同口径）：

| 档位 | 08-30 基线 | 09-06 HEAD | 判读 |
| --- | --- | --- | --- |
| baseline（mock 直连，c=64） | 未记录（Linux 容器 101–106k） | 77,858 RPS · P50 0.60ms · P99 3.71ms | 本机 HTTP 栈本底 |
| c=64 json | 4,016 RPS · P50 6.65 · P99 16.8ms | 4,663 RPS · P50 12.4 · P99 36.6ms；复跑 11,708 RPS · P50 5.39 · P99 7.19ms | 两跑都 ≥ 基线；差 2.5 倍说明数字被机器负载主导 |
| c=64 stream | 3,098 RPS · P99 18.7ms | 11,447 RPS · P50 5.46 · P99 8.60ms | 3.7 倍于基线 |
| c=1 json（纯开销口径） | 580 RPS · P50 1.52 · P99 5.04ms | 420 / 427 RPS · P50 2.34 / 2.33 · P99 3.86 / 3.39ms | P50 慢 0.8ms、P99 快 1.2ms；两跑一致。今天两处改动都不在响应路径上（幂等点查在后台结算事务里、计数是原子加减），更像负载差异，待安静机器复测才能下结论 |

压测全程 gateway 0 条 ERROR / WARN；请求错误 0。SIGTERM 下线走完整流程（"收到退出信号" → 30s 上限 → "已下线"），但暴露了第 3 节第 8 条：**结算积压 260,101 笔被放弃**，PG 只落了 166k / 426k 笔，Redis 侧扣款全部完成（`bal:{uid}` 无残留 `r:*` 预扣字段），即整段差额将由对账按账本回填。这是过载态下"响应先行"设计的代价，本轮只记录不改（计费语义变更须先改文档）。

其他：release 网关若用 `( nohup … & )` 子 shell 起，随工具调用结束一起被收走，必须作为常驻后台任务起。收尾已 `git worktree remove` 并删除 `okapi_head` 库、清空 Redis 逻辑库 7。

### 2026-09-06 第十轮：结算积压上界（分支 `settle-backlog`，已 ff 合入）

主工作树里并行会话正改着 `gateway/{auth,chat,state,mod}.rs`、`IMPLEMENTATION.md`、`okapi-api/error.rs`，恰是本项要碰的文件，故在 `git worktree add -b settle-backlog`（基于 `8a7074c`）+ 独立库 `okapi_backlog` + Redis 逻辑库 7 上做，三笔提交。当日并行会话提交 `e4dfbf1` 后 rebase（仅 `okapi-api/error.rs` 常量区一处相邻冲突）、隔离库全量复验，再 `git merge --ff-only` 合入 main，落地哈希 `4e411ce` / `58803c3` / `575f630`（下表按落地哈希）。

| 提交 | 内容 | 验证 |
| --- | --- | --- |
| `4e411ce` 结算积压上界 | 先改 IMPLEMENTATION §12.2（故障模式表新行）/ §12.3（第 4 条：PG 记账速率是单进程可持续吞吐硬上限，压测口径分两种）/ §14.3（30s 与上界配套）与 `docs/perf-report.md`（修正 #4 + 复现命令），再动代码：`settle_write` 进出计数、`OKAPI_SETTLE_BACKLOG_MAX`（缺省 20000，0 不设限）、`authenticate_data_plane` 最前面 `check_settle_backlog` → 503 `overloaded`（param = 积压数），滞回 3/4 恢复、进出各一条 WARN；gateway TraceLayer 的 `on_failure` 跳过 503；错误码进 `okapi_api::codes` 与中英语言包；`linux-bench.sh` 显式 `=0` 保持网关自身开销口径 | `gateway_backlog` 1 / 1；鉴权路径相关 15 个 gateway 套件 44 / 44；全工作区 clippy 无豁免通过；i18n 键守卫 1377 对齐 |
| `58803c3` 空表 epoch | 在隔离库跑 loadgen 撞到：`pricing_epochs` 为空时价簿与 30s 自校验都把 epoch 读作 1，首次发布拿到的正是 1，`swap_if_newer` 判"不比当前新"永不装载——**全新安装的第一次定价发布对在跑的 gateway 无效**，直到第二次发布或重启；共享开发库 epochs 从不为空所以此前无用例能碰到。两处 `COALESCE(MAX(epoch), 0)` | `gateway_first_epoch`（临时空库：epoch 0 → 发布得 1 → 热更 true → 同 epoch 不重复）1 / 1 |
| `575f630` 临时库用完即删 | 四个临时库套件收尾 `DROP DATABASE … WITH (FORCE)`；本机 157 个残留库手工清空 | 五个临时库套件 7 / 7，跑前跑后库数不变 |

release 复测（同机，缺省上界 20000）：json 档 15s **25,905 成功 / 37,888 被拒（503）**，可持续 1727 RPS ≈ PG 记账速率；日志全程 **3 条 WARN、0 条 ERROR**（首版无滞回时阈值附近每秒翻转十几次、TraceLayer 每个 503 一条 ERROR 共 7109 行，修掉后才是这个数）；压完**立即 SIGTERM，25s 退出**，PG 新增 25,906 = 成功数 + 预热 1 笔，**零丢账**（第九轮同场景放弃 26 万笔）。`OKAPI_SETTLE_BACKLOG_MAX=0`：5s 59,550 成功 0 拒绝、11.8k RPS，与第九轮口径一致。

分支顶端全量（隔离库 + Redis 逻辑库 7）：448 / 452，4 例失败（`admin_refund_full_cycle`、`partner_employee_keys_see_own_usage`、`margin_report_sums_amount_and_discount`、`client_distribution_groups_by_client_type`）全部是 CH 聚合把主库同 id 实体的行加了进来（如"实收 4038 ≠ 4000"、"CH 口径应被冲平 720 ≠ 0"），与第九轮同因，CH 库名写死无法隔离；四例代码未动、在主库上均通过。收尾：删 `okapi_backlog` 库、清 Redis 逻辑库 7、`git worktree remove`（分支保留）。

### 2026-09-07 第十一轮：当日全部特性落地后的主工作树全量

待验证的工作树 = 09-06 全天：分组限流 / 上游余额 / 负毛利熔断 / Bedrock + Vertex / 倍率同步 / 会话上限 / 自用订阅凭证 / Playground + 客户端一键导入（均未提交）。另一个会话已停止改动，工作区安静。

| 层 | 结果 | 数字 | 说明 |
| --- | --- | --- | --- |
| L0 rustfmt | 通过 | 0 文件 | 并行会话已把它那批 OAuth 文件格式化 |
| L0 clippy `-D warnings --all-targets` | **通过（全工作区、无豁免）** | — | 第七轮记的两条对方 lint 均已收口 |
| L0 sqlx 离线快照 | 通过 | 0 条缺失 | `SQLX_OFFLINE=true cargo check --all-targets` |
| L0 cargo-deny | licenses 仅 7 个自有 crate `unlicensed` | 12 个重复 crate 警告 | 与前几轮一致，等 §15 定案；重复 crate 为 `sha2 0.10/0.11`、`base64`、`hmac` 等两代并存，非新增 |
| L0 前端 tsc / oxlint / 五道守卫 | 通过 | i18n 键 1479 双语对齐；权限点 9；部署模板 Σ 池 152 / 200 | — |
| L1 + L2 Rust 全量 | 首跑 512 / 518；停掉残留进程后干净复跑 **518 / 518** | 106 个测试二进制 | 见发现 ① ② |
| L3 前端交互 e2e | 通过 | 80 / 80（新增 `playground.spec` 4 例） | — |
| L4 前端冒烟 e2e（真实 console） | 通过 | 10 / 10 | 演示超管在位，管理端用例真实执行 |
| L5 `okapi all` 冒烟 | 通过 | 4 断言（root 已存在跳过 key 断言） | 需先停掉并行会话残留的 `okapi all`（占 :8080 / :8081 近 15 小时） |
| L5 embed-web 发布形态 | 通过 | 首页 / 哈希资源 / SPA 深链 / API 401 | `verify-deploy.sh` 62s |
| L6 性能 | 未执行 | — | 第九、十轮已做，本轮无热路径改动 |

发现与处置：

1. **五例失败的根因是残留的常驻进程，不是测试互相干扰**：首跑时并行会话留下的 `okapi all`（含 worker）仍连着同一套 PG / Redis——worker 消费 `notify:mute` 频率闸、往 `inflight:gauge` 上报在途数、按 `settings.notify_channels` 往 mock SMTP 投递，正好对应 `worker_notify::notify_dispatch_and_mute`（订阅事件 0 ≠ 1，被 worker 先消费）、`gateway_multipod::inflight_gauge_sums_across_instances`（24 ≠ 12，多一个实例在上报）、`console_smtp::notify_email_channel_and_admin_test_send`（多出一个收件人）三例；`console_portal_pages` 公告与 `console_oauth` 的 RowNotFound 同属该进程的 settings 缓存 / 会话写入串味。**停掉该进程后干净复跑：106 个测试二进制 518 / 518，0 失败**。结论：全量测试前必须确认 8080 / 8081 无常驻 okapi 进程（已写进第 1 节注意事项）。
2. **`console_stats::client_distribution_groups_by_client_type` 单跑也失败（已修用例）**：断言 `share_bp > 0`，但长期 dev 库近一日已有 7 万多笔请求日志，本用例的 5 笔不足万分之一，整数基点截断为 0——用例假设"库是空的"。改为断言基点在 `0..=10000` 且全表各行之和 ≤ 10000（截断只会少不会多）。产品行为正确，是用例对共享库的假设过时。
3. 并行会话留下的 `RUST_LOG=info ./target/debug/okapi all` 常驻进程占着 8080 / 8081，`smoke-all.sh` 会因端口冲突起不来；本轮 SIGTERM 后正常退出再跑。以后长驻进程应在收尾时停掉。
4. **同一个常驻 `okapi all` 还会让 `console_stats` 的 `poll_until` 超时**（另一会话串行复跑时 `portal_charts_expose_cache_writes_performance_and_exact_date_window` 报"轮询超时"，其余 7 例通过；停掉进程后 8 / 8）。根因：它配了 NATS，worker 的 `nats_relay::relay_once` 用 `SKIP LOCKED` 抢到测试刚播进 `billing_outbox` 的 `request_log` 行，但 BILLING 流只收 `billing.>`，发布失败后把该行推进 5s → 10s → … 的退避（进程日志里 `NATS 发布失败 count=1` 与该行 `retry_count` 逐次对应），`chsink::process_once` 的 `next_retry_at <= now()` 过滤随即看不到它，用例 5 秒轮询必然超时。`request_log` 主题只有测试在播、生产代码只写 `billing.completed / refunded`，故不改代码；只是再一次说明测试期间不能有共用 dev 库的常驻进程。

### 2026-09-07 第十二轮：错误码反向守卫 + 兑换码生成 e2e + 第十轮落地

| 提交 | 内容 | 验证 |
| --- | --- | --- |
| `88cb85c` `scripts/guard-error-codes.py` | 第 2.5 节 i18n 行挂了一天的缺口：`guard-i18n-keys` 只核对前端引用到的键，后端能返回却没文案的 error_code 会掉进 `describeError` 的"未知错误 (code)"。新守卫解析 `okapi_api::codes` 常量、`AppError::new / unauthorized`、`StoreError::Conflict`、`ErrorBody::new`、`Self::new(StatusCode…)` 与模块级 `const` 的字面量（12 处变量 code 无法静态核对，均为 `codes::*` 二选一或透传），首跑抓到 **11 个**缺失：`redemption_invalid`、`payment_not_configured`、`payment_gateway_error`（门户用户直接可见）、`oauth_token_exchange_failed` / `oauth_upstream_{error,not_json,unreachable}` / `oauth_userinfo_missing_subject`（第三方登录失败全部显示成未知错误）、`no_zero_retention_channel`、`price_above_max`、`record_not_found`。两语言包补齐；守卫与第六轮的部署模板守卫一并接入 `ci.yml` | 56 个 error_code 全部有文案；`guard-i18n-keys` 1479 对齐；tsc / oxlint 干净 |
| `dad7845` `write-forms.spec` +1 | 兑换码生成抽屉（第 2.5 节唯一还标"无 e2e"的钱相关表单）：0.29 → 290000 micro、面值 0 禁提交、绑定用户 ` 77 ` → 77、空限额不发键、`datetime-local` 按浏览器本地换 UTC（期望值在同一浏览器里算，与运行机时区无关）、400 `bad_request` param 渲染成文案且表单留在原地、成功态批次 / 明文 / 复制全部进剪贴板 / 取消变关闭 | 单跑通过；interactions 配置两跑 80 / 81 → 81 / 81，唯一一次失败是并行会话新增的 `playground.spec 一键导入链接` 在并行下抖动（单跑与复跑均过） |
| 第十轮落地 | `settle-backlog` 三笔 rebase 到 `e4dfbf1` 后 ff 合入（见第十轮记录）；分支上原有第四笔（`client_distribution` 占比断言改为对合计核算）因并行会话已在主树做了等价修正而撤下，以对方的为准 | rebase 后全工作区 clippy 通过；隔离库全量 505 / 520，15 例失败全是 CH 串味（改到共享库跑 `console_analytics` 11 / 11、`console_portal` 1 / 1、`console_logs` 12 / 12、`gateway_upstream_cost` 2 / 2、`console_stats` 8 / 8） |

顺手修正第 2.5 节四条过时备注（模型状态切换本无 UI、`/admin/plans` 与套餐删除、TOTP 绑定早已覆盖）与第 2.2 节 `/v1/models` 的描述（无鉴权列全部启用模型是与公开价格页一致的既定行为，按 key 过滤属特性待定）。并行会话本机残留的 10 个临时库（旧代码跑出来的）再清一次，之后不会再长。

### 2026-09-07 第十三轮：CI 其实从未跑过 Rust 这一路

顺手查 GitHub 上 `main` 的 ci 结论：连续 6 次（09-05 起）全红，`frontend` job 绿、`deny` 与 `check` 红。
`check` 每次都死在 **Initialize containers**（约 3 分钟 = 30 次 × 5s 探活超时），也就是 fmt / clippy /
`cargo test --workspace` 一次都没在 CI 上执行过——本清单前十二轮的 L0–L2 全部只在本机跑过。

| 发现 | 取证 | 处置 |
| --- | --- | --- |
| ClickHouse service 永远不 healthy | 本地用 CI 同一镜像（`clickhouse-server:24.8-alpine`）+ 同一 `--health-cmd` 起容器：服务 40s 后已在 `0.0.0.0:8123` 监听，探活却一直 `Connection refused`。容器内 `/etc/hosts` 把 `localhost` 同时映到 `127.0.0.1` 与 `::1`，busybox wget 先走 `::1`；而容器网络没有 IPv6，clickhouse 日志 `Listen [::]:8123 failed … Address family not supported`，只绑了 IPv4。`wget http://127.0.0.1:8123/ping` 立即 `Ok.` | `ci.yml` 探活改 `127.0.0.1`，本地同参数复现 10s 内 `healthy`（`435ae59`） |
| `deny` job 常红 | 7 个自有 crate 无 `license` 字段 → `error[unlicensed]`；第三方依赖的 advisories / bans / sources 三项其实一直是 ok，被这一项压成整体红 | 自有 crate 标 `publish = false`（单二进制产品，事实如此），`deny.toml` `private = { ignore = true }`；项目许可证仍留 §15 定案。`cargo deny check` 四项全 ok，12 个 duplicate 仍是 warn |
| CI 环境与本机的差异会不会再挂一批 | 用干净 HEAD worktree（无 `.env`）、`env -i` 只给 `DATABASE_URL` / `OKAPI_REDIS_URL` / `OKAPI_CLICKHOUSE_URL` + `SQLX_OFFLINE`（无 NATS、无主密钥，与 `ci.yml` 一致）跑 `cargo test --workspace --no-fail-fast` | **523 / 523**，108 个测试二进制；依赖 NATS 的套件按设计软跳过，主密钥各用例自生成 |

三条都不改产品代码。`check` job 的下一次运行才是这条流水线第一次真正的 L0–L2 结论，推送后要回头看一眼。

### 2026-09-07 第十四轮：review 当日落地的特性（约 1.2 万行）

按 A–E 维度过一遍并行会话 `e4dfbf1` / `07c1fa8` / `349fdb6` 三笔的后端：倍率在线同步、上游余额、负毛利熔断、Bedrock / Vertex 传输层、订阅 OAuth 凭证（四步锁刷新、方言分派、身份头透传）、Playground 中继、分组限流、会话上限。前端与 e2e 部分沿用第十一轮结论。

| 模块 | 结论 | 处置 |
| --- | --- | --- |
| `console/channel_oauth.rs` `exchange` | **越权（C）**：带 `channel_id` 追加 key 时把 `guard_scoped` 给的范围丢了，own 范围渠道管理员能往别人的渠道塞订阅凭证；且属主 / provider 校验都在换码之后，被拒也先把一次性授权码烧掉 | **已修** `f737892`：`ensure_channel_owner` + 一致性检查前移到换码前；`gateway_oauth_channels::own_scope_cannot_attach_oauth_key_to_foreign_channel`（403 owner、token 端点零调用、key 数不变） |
| 出站重定向 | **SSRF 闸可绕（C）**：见第 3 节第 10 条 | **已修** `f737892` + `8a92e2a`（探针 client 族 + 七处调用点 + 用例）；订阅 OAuth / Bedrock / Vertex 的三处外呼留待统一 |
| `console/ratio_sync.rs` | 数值全程十进制字面量经 `RatioFp`，不经浮点 ✅；RBAC（fetch=`pricing.read`、apply=`pricing.write`）+ 审计 ✅；不自动发布 epoch ✅。小问题：同一模型同时勾了倍率轴与按次价时按次价被静默丢弃但仍计入 `applied`；`fetch` 只要 `pricing.read` 就能让服务器对任意（过闸的）URL 发 GET，出站副作用与只读权限不太匹配 | 记录，不改 |
| `console/channel_balance.rs` | 金额经 `parse_scaled_1e6` 定点、美分 ÷ 100 整数 ✅；own 范围 `ensure_channel_owner` ✅；凭证从密文解封、走渠道自己的代理 / 额外头 ✅；8KB 体上限 ✅；`ch:balance:*` 已登记 database.md ✅ | — |
| `margin.rs` / `worker/margin_breaker.rs` / `console/margin.rs` | 万分比全整数、i128 防溢出、配置越界夹取、缺省关 ✅；`mb:blocks` 契约已登记 ✅；console list=`channel.read` / lift=`channel.write` + 审计 ✅ | — |
| `gateway/oauth_cred.rs` | 四步锁：进程内单飞 → `lock:cred` `SET NX EX 30`（自愈）→ 加锁重读 → 刷新回写；无锁方就用未过期旧 token；`invalid_grant` 二次重读识别他副本轮转，否则 key 置 invalid ✅。边角：回写 PG 失败时本次用新 token 但不落库，下次会拿旧 refresh token 再刷——对 refresh token 轮转的提供方等于把 key 刷成 invalid（只在 PG 写失败时发生） | 记录，不改 |
| `gateway/dialect.rs` | `(provider, model)` → 三种方言的分派干净，bedrock / vertex 缺 `api_base` 是构造错误不回退公网 ✅ | — |
| `console/playground.rs` | 直调 `gateway::chat::chat_completions`，鉴权 / 限流 / 计费 / 结算积压准入与真实 SDK 一致 ✅；1MB 体上限、强制流式 ✅；预设白名单收口 ✅。两点备注：① 拆分部署时 console 进程因此成为数据面节点（要有上游出网、同一 `OKAPI_MASTER_KEY`、结算积压计数各算各的）；② `/api/playground/presets` 无鉴权公开，站点预设的 `system` 提示词对匿名可见，按设计但站长得知道 | 记录 |
| `gateway/sched_redis.rs` 分组限流 / `auth_web.rs` 会话上限 | 固定窗计数、TTL 与键名与 database.md 一致 ✅；会话上限踢最早、新建那条永不被踢 ✅ | — |
| Bedrock / Vertex 传输层 | 无浮点；服务账号 JWT 用 `aws-lc-rs`（已在依赖树），`hmac` / `sha2` 做 SigV4，IMPLEMENTATION §11.35 已登记依赖 ✅。签名正确性当时无真实上游可验——第十五轮用 botocore 当参考实现对拍，**抓到规范 URI 少一次编码**（见第十五轮） | **已修** `8e0ee4c` |
| 合规边界 | 见第 3 节第 11 条 | 记录 |

另：`console_stats.rs` 上一笔提交漏了 `cargo fmt`，CI 的格式检查会红，随 `f737892` 一并格式化。

**续（同日）**：第 3 节第 10 条落地 `8a92e2a`（`HttpPool` 探针 client 族 + `PassUpstream::probe` + 七处管理面调用点 + `channel_test_does_not_follow_redirects`），IMPLEMENTATION §14.4 记录定案。第 2.5 节最后一条"尚无 e2e"补齐（`2578eab`：毛利熔断卡、查上游余额）。顺带把 interactions 配置里两处并行抖动挖到根：① 抽屉打开 30ms 后 `use-modal-focus` 才把焦点送进第一个输入框，并行负载下 `fill` 卡在这之前就被打断、字打进别的框——第十一轮以来偶发的 `套餐抽屉` / `价格分组抽屉` 失败（"display_name: Starter30.9"）就是它，`write-forms.spec` 加 `openedDialog` 助手等焦点进对话框再填，21 处统一换用；② `playground.spec 一键导入` 的 "New key" 页头与空态各一个，列表桩回空后才出现第二个，strict 模式随时序偶发 2 元素，取第一个。interactions 连跑三遍 83 / 83，一键导入重复 12 次全过。

### 2026-09-07 第十五轮：review 续——SigV4 对拍、oauth_token_url 闸、分组限流 e2e

| 模块 | 结论 | 处置 |
| --- | --- | --- |
| `okapi-providers/aws_sigv4.rs` | **签名必错（A / D）**：非 S3 服务的规范 URI 要在请求路径（已按段编码一次）之上再编码一次——AWS 文档「Each path segment must be URI-encoded twice」，botocore / JS / Go SDK 同此。此前直接用单次编码的路径，`/model/anthropic.claude-3-haiku-20240307-v1%3A0/invoke` 签成 `%3A` 而非 `%253A`。`gateway_bedrock` 的 mock 用同一实现重算签名所以一直绿，真实 AWS 会回 SignatureDoesNotMatch；Bedrock 上的 Anthropic 模型 ID 全部带 `:0`，等于 SigV4 凭证形态整条路不通（Bearer API key 形态不受影响）。取证方式：临时 venv 装 botocore 1.42.97，同一请求同一时间戳生成参考签名 | **已修** `8e0ee4c`：`canonical_uri` 按段再编码；单测钉住 botocore 向量（签名逐字节相等），官方 get-vanilla 向量照旧 |
| `console/admin.rs` 渠道 settings 写入口 | **SSRF 后门（C）**：`settings.oauth_token_url` 是网关刷新时 POST refresh token 的地址，OAuth 登录端点写它时过闸，但渠道创建 / 更新的通用 settings 写入口没有，改一下 JSON 就能把刷新请求指向私网 / 元数据地址 | **已修** `cfce5ff`：两处写入口过同一道 `validate_api_base`；`console_ssrf`（临时库缺省策略）钉住 http / 私网 / 非字符串各回对应 param、公网 https 放行 |
| `okapi-providers/vertex.rs` | 服务账号 JWT：RS256 / PKCS#8、`iss` / `scope` / `aud`=token_uri / `iat` / `exp` 齐、jwt-bearer 表单手拼正确、base64url 无填充 ✅。备注：token 缓存的 mutex 跨过网络请求且是全 map 一把锁，多条 Vertex 渠道时一家 token 端点慢会串行拖住其它家（刷新是小时级事件，影响有限） | 记录 |
| `okapi-providers/bedrock.rs` | 签名头（content-type / accept / x-amz-content-sha256）与实际发送头逐一对齐 ✅；`aws_region` 写入限定 `[a-z0-9-]`，无主机名逃逸 ✅；控制面列模型与运行时同用签名服务名 `bedrock` ✅ | — |
| `gateway/chat.rs` 订阅接线 | codex 只在 Responses 入口进候选（其它入口 `retain` 掉）✅；`responses_native` 撞 404 / 405 就地改走降级链不计 failover ✅；订阅 provider 的出向带客户端身份头 ✅ | — |
| `okapi-store/credential.rs` | OAuth 凭证以 `{"kind":"oauth",…}` JSON 明文形态经 `seal_or_plain` 密封（配了主密钥即密文），刷新回写同一入口 ✅ | — |
| 前端 | `价格分组抽屉` 用例补分组级限流字段与列表限流列（第 2.5 节最后一条备注收口，`e316f80`）；interactions 83 / 83 | — |

至此第 2.5 节前端覆盖表不再有"无 e2e"备注；第 3 节只剩第 11 条（订阅凭证合规边界）待定。

**收尾复核（同日，无新发现）**：`sched_redis.rs` 新增键（`sess:web/idx/meta`、`lock:cred`、`oauth:cred`、`mb:blocks`、`ch:balance`）全部登记在 database.md，`oauth:cred:<state>` 用 GETDEL 一次性取走、`lock:cred` 30s 自愈 ✅；worker 每 5 分钟评估熔断，多副本各评各的但写同一 HASH、通知经 `notify:mute` 去重，幂等 ✅；`cloud_probe` 只被 `test_channel` / `fetch_channel_models` 调用，守卫与属主校验在调用方 ✅；`RatioSyncPanel` 的应用按钮不按 `pricing.write` 隐藏——与全站"路由按读权限进、写动作靠后端 403 + 错误码文案"的约定一致，不算缺口。当日特性 review 至此完成：三处已修（越权、SSRF 重定向 / token URL 后门、SigV4 双重编码），一处待定（第 3 节第 11 条）。

### 2026-09-07 第十六轮：review 修完后的 HEAD 全量，外加一次 CI 等价的"三件全新"

`9a7fcca`（review 全部修复落地后）在干净 worktree、`env -i` 只给 CI 那三个连接串下：

| 层 | 结果 | 说明 |
| --- | --- | --- |
| L0 rustfmt / clippy `-D warnings --all-targets` / cargo-deny | **全部通过**（deny 四项 ok，第十三轮起） | — |
| L0 六道守卫 | 全过：i18n 键 1479、权限点 9、部署模板 Σ 池 152 / 200、56 个 error_code 双语齐 | — |
| L1 + L2 全量（共享开发库 + 共享 Redis / CH） | **526 / 526**，108 个二进制 | 与第十三轮 523 相比多的三例是 review 期间新增的用例 |
| L1 + L2 全量（**三件全新**：空 PG 库 `okapi_fresh`、空 Redis 逻辑库 8、按 `ci.yml` 同一镜像与探活参数新起的 ClickHouse 容器） | 525 / 526 → 修一例后 11 / 11 × 3 | 唯一失败 `console_analytics::advanced_filters_calendar_quality_and_partial_cost_are_consistent`：`window.freshness.last_ingested_at` 读 `mv_analysis_hour`，全新 CH 上它比 `total` 所在的 MV 晚一拍，而轮询谓词没等它——共享库里该 MV 总有历史行所以从未暴露。**CI 的 ClickHouse 正是全新的**，不修 check job 第一次真正跑就会红。把该字段并进 `poll_until` 谓词（`0b971bb`，第 3 节第 5 条同一套约定） |

这轮的意义：第十三轮的 CI 仿真是在共享开发库上做的，"库不是空的"这个隐含前提没有被挑战过；三件全新的一跑把它挑出来了。以后要对 CI 结论负责的验证，按这个口径跑（`docker run … clickhouse-server:24.8-alpine` 一台新 CH 只要十秒）。第 3 节第 11 条仍是唯一待定项；第 2 / 3 节其余全部收口。

### 2026-09-07 第十七轮：CI 第一次真正跑完 Rust 这一路

`75a8325` 推送后的 [run 34161541108](https://github.com/qiaojinxia/okapi/actions/runs/34161541108)：`frontend` 绿，`deny` 红，`check` 过了容器探活、第一次跑到 clippy 就红。第十三轮的仿真只复现了"环境"（无 `.env` / 无 NATS / 空库），没复现"工具链"——两处都是本机绿、CI 红。

| 发现 | 取证 | 处置 |
| --- | --- | --- |
| clippy 工具链漂移 | CI 的 `dtolnay/rust-toolchain@stable` 当天解析到 **1.98.1**，本机 1.95。装同版本后 `cargo +1.98.1 clippy --workspace --all-targets -- -D warnings` 本地复现 8 处：`unused_async_trait_impl` ×2（`gateway/extract.rs` 的 `Query<T>` 与 `console/auth_web.rs` 的 `MaybeConnectInfo`，两个提取器的 `async fn from_request_parts` 体内没有 `.await`）；`result_large_err` ×4（1.98 起默认开，`chat.rs` 返回 `ForwardFailure` / `AttemptError` 的四个函数，错误约 160 字节 > 缺省阈值 128）；`map_or_identity`（新默认 lint）与 `manual_is_variant_and`（pedantic，1.98 起认得 `.ok().is_some_and`）各 1 | `c8d3e53`：提取器改为直接返回 `std::future::ready(..)`（`Query<T>` 因返回 `impl Future + Send` 需补 `T: Send`，语义不变，`console_manage::malformed_query_string_is_rejected_as_error_code` 仍钉住拒绝路径）；`result_large_err` 不逐处 allow，在 `clippy.toml` 把阈值放到 256——Err 只在失败路径出现、Ok 侧的 `Response` 本身更大，装箱只是给失败路径多一次分配；两处表达式直接简化 |
| `deny` job 红 | `cargo-deny-action@v2` 缺省 `--all-features`，`embed-web` 特性经 `rust-embed-impl → shellexpand → dirs → dirs-sys` 拉进 `option-ext`（MPL-2.0）；本机 `cargo deny check` 不带特性所以一直 ok | `deny.toml` 按 crate 放行 `option-ext` 的 MPL-2.0（只在编译期帮 proc-macro 找配置目录，不进产物），不把 MPL 加进全局 allow；`cargo deny check` 与 `cargo deny --all-features check` 四项全 ok |
| 提交前复核 | 1.98.1 与 1.95 两个版本的 `cargo fmt --check` 与 clippy 全绿；1.98.1 全量 `cargo test --workspace`（CI 同款 `env -i` 三连接串）108 个二进制，唯一失败 `console_manage::price_group_pagination_matches_database_pages`（API 总数 991 ≠ DB 计数 986）——当时机器上并行会话另有三个 `cargo test --workspace` 和一个常驻 `okapi all` 打着同一套开发库，两次计数之间被别人插了 5 条价格分组；单独重跑 8 / 8 | 不改代码；这正是第 1 节"跑 L2 之前先确认没有常驻进程"那条注意事项的又一次注脚，并行会话同时跑全量时也一样 |

推送 `c8d3e53` 后 [run 34167474171](https://github.com/qiaojinxia/okapi/actions/runs/34167474171) **三个 job 全绿**：`check` 690s（clippy 97s、`cargo test --workspace` 534s，全新 PG / Redis / ClickHouse 容器，与第十六轮"三件全新"的本地结论一致）、`deny` 38s、`frontend` 26s。这是这条流水线自 09-05 建立以来第一次整体通过，也是 L0–L2 第一次拿到 CI 侧结论。第 1 节 L0 行与注意事项据此补上"用 CI 的 stable 跑 clippy、deny 带 `--all-features`"。第 3 节第 11 条仍是唯一待定项。

### 2026-09-07 第十八轮：第 3 节第 10 条的尾巴——三处外呼统一，顺手抓到 Vertex `token_uri` 没过闸

第十四轮把七处管理面外呼换成不跟随重定向的探针 client 时，把订阅 OAuth 换码 / 刷新、Vertex 服务账号换 token、Bedrock 列模型三处留作"下一步"。这轮把它们收掉，过程中按"网关会往哪些管理员给的地址发请求"逐个对照 SSRF 闸的覆盖面，多出一条。

| 模块 | 结论 | 处置 |
| --- | --- | --- |
| `providers/oauth/{codex,anthropic_max}.rs` 换码 / 刷新、`vertex.rs` 换 token、`bedrock.rs` 列基础模型 / 列兼容模型 | 五处都是"POST / GET 一次、拿 JSON 回来解析"，没有跟随重定向的理由；其中 OAuth 的 `token_url` 与 Vertex 的 `token_uri` 都是管理员可控地址 | 全部换 `HttpPool::probe`；`gateway_oauth_channels::token_endpoint_redirect_is_not_followed`（两家 × 换码 / 刷新四条路：302 原样成 `UpstreamError::Status`，目标零命中）、`gateway_vertex::vertex_token_endpoint_redirect_is_not_followed` 钉住 |
| Vertex 服务账号 JSON 的 `token_uri` | **SSRF 后门（C），与第十五轮 `oauth_token_url` 同类**：网关按它 POST JWT 断言换 access token；`api_base` 与 `oauth_token_url` 都过闸，这个字段藏在凭证里从未过。有 `channel.write` 的人贴一份 `token_uri` 指向私网 / 元数据地址的服务账号 JSON，点"测活（凭证）"就能拿回目标非 2xx 响应体的前 300 字（`cloud_probe` 的 `upstream_body`）；凭证密封存储，读侧看不见，写入口是唯一能校验的地方 | `console::ssrf::validate_credential`：能解析成服务账号 JSON 就把 `token_uri` 过 `validate_api_base`，违规回 `credential_token_uri`（管理员没动 api_base，不该看到 api_base 的参数名）；接在建渠道、轮换凭证、MCP 建渠道三处。按凭证形状而不按 provider 判断——先以 openai 存下再 PATCH 成 vertex 也绕不过。`console_ssrf::vertex_token_uri_goes_through_the_same_gate`（元数据地址 / 私网 / 换 provider 三种 400、缺省地址放行、轮换入口同样拒放） |
| 顺带核对 | Bedrock 列模型的主机由 `aws_region`（写入时限 `[a-z0-9-]`）拼成 `bedrock.<region>.amazonaws.com`，无逃逸 ✅；订阅 OAuth 换码 / 刷新用 `Outbound::default()`，即**不走渠道自己的 `proxy_url`**（A / D）——配了代理的订阅渠道，API 请求从代理出去、刷新却直连：订阅账号对出口 IP 敏感，两个出口会触发上游的异常检测；只有代理能出网的部署则根本刷不动，token 到期即整条渠道失效 | 先记录，**同日续修**（见下） |

复核：1.98.1 与 1.95 两版 fmt / clippy 全绿；受影响八个套件（`console_ssrf` / `gateway_vertex` / `gateway_oauth_channels` / `gateway_bedrock` / `console_channel_test` / `console_mcp_write` / `console_manage` / `channel_credential`）29 / 29。第 3 节第 10 条至此没有尾巴；仍走数据面 client 的只剩 bedrock / vertex 的按模型测活（16 token 补全，和真实请求同一条路，api_base 已过闸）。

**续（同日）：刷新走渠道代理。** `oauth::{anthropic_max,codex}` 的 `exchange` / `refresh` 多一个 `proxy_url`，token 端点的出站只带渠道代理、不带渠道给上游 API 配的额外头（`token_outbound`）；`gateway::oauth_cred::OAuthKey` 从候选行带上 `proxy_url`，`cloud_probe` 从渠道设置带，追加 key 的换码用目标渠道的代理（登录新建的渠道此刻还没有设置，直连）。`gateway_oauth_channels::token_refresh_and_attach_exchange_use_channel_proxy` 用一个记录 URI 的最小正向代理钉住：渠道配上代理后到期刷新与 API 请求都经代理各一次，给该渠道追加 key 的换码再经一次。IMPLEMENTATION §11.38 补一句定案。

### 2026-09-08 第十九轮：按 §2 矩阵逐模块审计 + 六层全量（隔离环境）

这轮的题目是"所有现有功能都要有端到端覆盖"，所以不是照着既有套件再跑一遍，而是**先按 §2 矩阵逐模块对照实际测试代码**，把"矩阵里写着有、实际没有"和"矩阵里根本没登记"的功能点挖出来，再补用例、再全量。

环境按第十六轮的口径：`HEAD 47bee01` 的干净 worktree（`/tmp/okapi-r19`，完全绕开并行会话的工作树），三件全新——空 PG 库 `okapi_r19`、空 Redis 逻辑库 9、按 `ci.yml` 同镜像新起的 ClickHouse 容器（:28123）；Rust 用 CI 的 stable 1.98.1。

| 层 | 结果 |
| --- | --- |
| L0 静态 | `cargo fmt --check` 1.98.1 与 1.95 双绿；`clippy --workspace --all-targets -- -D warnings` 零告警；`cargo deny check` 与 `cargo deny --all-features check` 各四项 ok；前端 `tsc -b` + `oxlint` 干净；六道守卫全过（i18n 引用键 1479、前端权限点 9、部署模板 Σ 池 152 / 200、error_code 56 个双语齐） |
| L1 + L2 | 审计前 **529 / 529**（108 个二进制）；补完本轮四个用例后 **533 / 533**（109 个二进制）。两跑都是 `env -i` 只给 CI 那三个连接串 |
| L3 前端交互（接口桩） | **83 / 83** |
| L4 前端冒烟（打真实 console） | **93 通过 / 4 跳过**；跳过的四例全在 `screenshots.spec`（截图工具，非功能用例） |
| L5 部署形态 | `smoke-all.sh` 四断言过；`verify-deploy.sh` embed-web 四断言过；`OKAPI_VERIFY_IMAGE=1` 的发布镜像五断言过（`git archive HEAD` 快照构建，229MB，65534 运行，空库首启迁移 + Setup 向导） |

审计发现与处置：

| 发现 | 取证 | 处置 |
| --- | --- | --- |
| **两个会打上游的数据面端点没有任何限流准入（D）** | 把 `gateway/` 下 `authenticate_data_plane` / `check_member_limit` / `check_group_rate` 三个调用点排在一起看：八个计费端点都是"鉴权 → 成员限额 → 分组限流"三连，只有三个端点鉴权完就直接往下走——`/v1/messages/count_tokens`（`chat.rs:488`）有 anthropic 候选时会拿渠道凭证代理上游 tokenizer，`/v1/videos/{task_id}` 与 `/content`（`videos.rs` 的 `relay_task`）拿渠道凭证轮询 / 下载，两者都不计费所以不在 §11.32 的"全部计费端点"口径里，可用户能拿一把 key 无限打、无限消耗渠道配额与上游速率。第三个 `/v1/dashboard/billing/*` 只读本地库，不碰上游，维持不设限 | **已修**：两处补 `check_group_rate`，**不补** `check_member_limit`——后者是团成员的月度**消费**上限，用它挡住不花钱的轮询与下载，会让刚好花超的成员连已经付过费的视频都取不回来。限流该按"会不会打上游"划界而不是"扣不扣钱"，IMPLEMENTATION §11.32 先补定案再改码。`gateway_group_rate::non_billing_upstream_endpoints_are_rate_limited` 钉住（`count_tokens` 前两笔 200 第三笔 429 且是 Anthropic 错误壳、视频轮询在任务查找**之前**就 429 所以拿到的是 429 而非 404、下载入口同）。撤掉修复复跑确认用例会红（第三笔回 200），不是摆设 |
| **`console/cloud_probe.rs` 整个模块零集成覆盖** | 该模块 234 行全是分派：bedrock / vertex / anthropic_max / codex 四家 × credential / model 两种探测范围 + `fetch_models`，签名、换 token、刷新都在这条路上。`bins/okapi/tests` 里请求过 `/admin/channels/{id}/test` 与 `/fetch-models` 的只有 `console_channel_test`，而它建的全是 openai 渠道——四家云渠道的测活按钮与拉模型按钮从来没被端到端走过 | **已补** `console_cloud_probe.rs`（3 例）：vertex 两种范围各走一遍换 token（credential 只换 token、model 还真发一次 generateContent）、token 端点 401 时把上游状态与原文带出、`fetch_models_unsupported`、留痕回填列表页"最近测试"；bedrock 两种凭证形态分流（Bearer 走 `/openai/v1/models`、SigV4 走签过名的 InvokeModel 且模型 ID 冒号编成 `%3A`、Bearer 拉模型回 `fetch_models_requires_sigv4`）；订阅两家的"没到期零网络往返 / 过期先刷新再探 / 轮转出的 refresh_token 回写 / model 范围真发一次补全（codex 非流式靠 SSE 聚合成 JSON）" |
| 熔断通知的载荷拼在 worker 主循环的 `select!` 臂里 | §2.4 早就记着"未在 mock sink 上断言载荷"。载荷拼在循环里的话，测试只能照抄一份 `json!`，改坏了照样绿 | 把那 20 行收进 `margin_breaker::evaluate_and_notify`（生产与用例走同一段代码），主循环只剩一次调用与错误分支。**投递断言仍未补**：`settings.notify_channels` 是全局键，在共享开发库上写它会和 `worker_notify::notify_dispatch_and_mute` 互相覆盖，要补得先给这个用例一套临时库——列为下一步，不硬塞一个会抖的用例进来 |

两条**结构上就做不了 e2e** 的，把 §2 里含糊的备注改写成确切原因：

- `channel_balance.rs` 的 DeepSeek / SiliconFlow / OpenRouter / Moonshot 四个官方探针按 **`api_base` 的主机名**选（`api.deepseek.com` 等硬匹配），mock 只能起在 `127.0.0.1` 上，永远落进 `OpenAiDashboard` 分支。除非给探针选择开一个测试后门，否则这四家只能停在响应形状单测——不是疏忽。
- `bedrock.rs` 的 `list_foundation_models` 把控制面主机固定成 `https://bedrock.{region}.amazonaws.com`（有意为之，VPC 端点用户也走公网控制面），mock 接不上，所以 SigV4 形态的"验凭证"与"拉模型"两条分支只有 `aws_sigv4` 单测和真实凭证能验；本轮补的 bedrock 用例覆盖的是 Bearer 形态与 InvokeModel 那条。

另：开始前发现 8080 / 8081 上又挂着一个跑了 **23 小时**的 `okapi all`（前几轮验证留下的）。这轮 L1–L3 走隔离环境不受影响，L4 / L5 要用这两个端口，停掉后跑的。第 1 节那条注意事项到此已是第三次被同一个东西验证，跑全量前请务必先看一眼。
