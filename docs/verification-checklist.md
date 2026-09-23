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
| `okapi-pricing` | PriceBook 编译、三层倍率 / 按次 / 阶梯 / 缓存双轴、规则栈、快照 | A B | `parity.rs`（new-api 对拍 fixtures）、`prop.rs`（proptest）、单元 `book/engine/handle/model/ratio/rules`；集成 `gateway_m1`（cache ratio）、`gateway_tier`、`gateway_pricing_rules`、`gateway_model_modifiers`、`console_pricing_write`、`console_import`；09-08 第十九轮补 `gateway_pricing_rules::surge_rule_reads_cluster_inflight_and_marks_up_the_bill`——surge 规则此前只有引擎单测与在途量单测，中间"读 `settings.surge_inflight_threshold` → 取集群在途量 → 送 `ctx.surge_active`"没人接，现在从账单金额倒着验（未配 240 / 阈值 0 关闭 240 / 阈值 5 且邻居报 5 个在途 → 360 且规则进快照 / 邻居空下来回 240） | — |
| `okapi-ledger` | Redis Lua reserve / commit / refund / repair / sub_set + PG 同事务记账 + outbox | A B D | crate 级 `tests/lua_contract.rs`（09-06 新增，7 例）：预扣字段四段 / 多退少补 / 重复 commit 与 refund 任意顺序幂等 / `avail == est` 放行、`avail < est` 拒绝且零写入 / 四个 key 级限额各自 which 且拒绝零写入、并发槽随结算释放 / repair 绕开在途、不动另一池、负目标不夹逼 / drain 只取正余额 / 13 步交错序列逐步验证 `avail + Σ在途 == 入账 − Σ实际`；订阅池选池由 `console_subscriptions::lua_pool_contract` 覆盖；crate 级 `tests/pg_settlement.rs`（09-06 第四轮，5 例）：records / events / users 快照 / api_keys 用量 / outbox 五处同事务且四金额列与 pool 三处一致（含 INET 列真落）、第二条语句失败整体回滚、订阅池结算与订阅事件不动钱包快照、失败请求零金额落 error_code、`admin_refund` 只对 committed 生效一次并逐项回冲；集成：`gateway_m1`、`worker_m2`、`worker_reconcile_repair`、`console_ops`、`console_teams`、`gateway_realtime`；第五轮补 `replaying_a_settled_request_writes_nothing`（重放 request_id 五处零写入） | — |
| `okapi-providers` | openai / anthropic / gemini / azure / bedrock / vertex / custom_pass / responses 客户端，`oauth::{anthropic_max,codex}` 订阅登录（§11.38），`convert/*` 按方向转换，modifiers / reasoning，`http.rs` 代理与额外头，`aws_sigv4` / `aws_eventstream`（§11.35） | A E | `convert_a2o` / `convert_anthropic` / `convert_gemini` / `reasoning_t2c` / `stream_usage`；单元 `azure` / `gemini_to_openai` / `http` / `modifiers` / `reasoning` / `responses` / `aws_sigv4`（AWS 官方派生密钥与 get-vanilla 签名向量、会话令牌、凭证形态、路径编码；09-07 第十五轮加 botocore 生成的含 `%3A` 路径向量——规范 URI 二次编码，此前必签错）/ `aws_eventstream`（跨包切帧、坏 prelude、非字符串头跳过）/ `bedrock`（区域解析、InvokeModel 体、chunk 解码、exception 帧）/ `vertex`（publisher 路由、api_base 形状、rawPredict 体、SA 解析、JWT RS256 三段）/ `oauth`（PKCE S256 派生、两家授权 URL 参数（Anthropic 端点已随 CLI 迁 `claude.com/cai` + `platform.claude.com`）、token 响应形状与缺省、invalid_grant 分类含 codex-rs 的 `refresh_token_*` 细分码、贴回 code 拆分、系统首句前置幂等、beta 头合并去重且透传头里的 `anthropic-beta` 摘掉不发两行、id_token claim 取 account_id、Codex 请求体整形（store=false / stream=true / instructions 键 / system→developer / 不支持字段剥离）、Codex SSE 聚合回 JSON（终态 output 为空按 output_index 拼、error 事件 → 502、无终态 → Stream 错）/ `anthropic`（429 冷却：Retry-After 优先，否则 `anthropic-ratelimit-unified-reset` 推剩余秒，过去的重置点不冷却）；集成见 2.2 chat 行 | `gemini_to_openai` 只有单元 + `gateway_gemini_ingress` 集成，无独立 parity fixture 文件 |
| `okapi-store` | sqlx 查询、迁移、凭证信封 AES-GCM、身份（argon2 / bcrypt 双轨）、分页、CIDR 匹配、CH schema | A C H | 编译期：`.sqlx` 离线校验全部 `query!`；`schema_shape`（迁移形状守卫）、`channel_credential`（密文落库 / 无主密钥 fail-closed）、`console_manage::price_group_pagination_matches_database_pages`、`gateway_ip_allowlist`（netmatch）、`worker_ch`（CH 表与 MV）；单元 `credential` / `identity` / `listing` / `mutate` / `netmatch` / `subscriptions` / `vendor`；`channel_pools::deleting_group_or_pool_ignores_soft_deleted_key_overrides`（09-08：软删令牌的 `group_override` / `pool_override` 死引用不再拦住删分组 / 删池，活令牌仍 409） | 软删外键死引用已按 `0001_init.sql` 的 `REFERENCES` 列逐个排查完毕，三处全修（第 4 节第十九轮） |
| `okapi-api` | DTO、`AppError` 错误码壳、权限点清单 | A C | 单元 `permissions.rs`；`console_m2::permission_point_matrix`；守卫 `guard-frontend-permissions.py`（前端引用的权限点都在后端清单） | 后端自然语言检查（i18n-audit §3）为人工 `rg` |

### 2.2 gateway 角色（`bins/okapi/src/gateway`）

| 模块 | 职责 | 维度 | 覆盖套件 | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| `auth.rs` | Bearer / x-api-key / x-goog-api-key 鉴权、无效 key 每 IP 限流、key 级 IP 白名单、分组级 `[rpm, rph]`（§11.32，随鉴权缓存下发、全部计费端点 reserve 前检查） | A C | `gateway_invalid_key_rate`、`gateway_ip_allowlist`、`gateway_group_rate`（09-06：同组每用户各自计数、别组不受影响、rph 小时窗、管理面改限额即失效缓存、负数 400、0 归一 null；09-08 第十九轮加 `non_billing_upstream_endpoints_are_rate_limited`：`count_tokens` 与视频任务轮询 / 下载这两个不计费但打上游的端点同样进窗，且限速在任务查找之前）、`smoke-all.sh`（无凭证 401 fail-closed）、e2e smoke（普通用户管理面 403）、`gateway_key_admission`（09-08 第十九轮，2 例，每段都先打一发成功请求把鉴权缓存焐热再改状态）：手动停用 → 401 `key_disabled` 且改回立刻复活、`expires_at` 设到过去 → 401 且库里 status 仍是 1、封禁属主 → 名下 key 全 401 且解封不连带复活令牌、白名单外模型 403 `model_not_allowed` 而不存在的模型仍 404 `model_not_found`、`[]` 归一成 null=不限 | 八个计费端点里 chat 与上述两个非计费端点有集成用例；其余共用同一 `check_group_rate`，靠编译期同构 |
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
| `auth_web.rs` | 注册 / 登录 / TOTP / 兑 key / 会话列举吊销 / 会话数上限（§11.37，`settings.web_session_limit`，踢最早）/ 邮箱验证码 / 找回密码 / 关键接口限流 | A C | `console_auth_web`（含 `session_limit_evicts_oldest`：上限 2 连登三次，最早 cookie 兑 key 401、后两条有效、列表两条并回 `limit`；09-08 第十九轮 `register_login_key_totp_full_flow` 补三种错码——纯错码 / 位数不对 / **一小时前那个时间窗的码**——都得 401 `totp_invalid`，另加"密码错 + 码对"仍报 `invalid_credentials`）、`console_smtp`（验证码、重置、无 SMTP 501；09-08 补 `site_url`：配了就压过请求 Host、尾斜杠吃掉、空白串不算配置回落 Host）、`console_audit::login_attempts_are_audited`、e2e smoke（登录 / 登出清 session / session 降级） | OAuth 回调走同一 `open_web_session`，靠编译期同构，无 OAuth 路径的上限用例 |
| `oauth.rs` | 通用 OAuth2 / OIDC | A C | `console_oauth`（mock IdP 授权码全流程）；`console_oauth_presets`（09-06 第八轮，独立临时库）：github / discord / linuxdo 三预设的 scopes 进授权跳转、数字 / snowflake `id` 作绑定键、`login` / `username` 作展示名与首登用户名；改名不换账号、同名不同 id 是另一个账号且用户名加盐、缺 `id` 拒绝不落绑定、token 端点 500 → `oauth_upstream_error` param `status_500`；`provider_list_is_public_and_leaks_no_credentials`（09-08 第十九轮）：`GET /auth/oauth-providers` 无需鉴权、只出 code 且保持配置顺序、响应里搜不到 client_id / client_secret / token_url，设置形状不认或整条缺失都回空列表而非 500 | — |
| `registration.rs` + `auth_web::verify_turnstile` | 注册策略、邀请赠送、Turnstile | A C | 单元；`console_auth_web::registration_policy_gates_signup`；`console_turnstile`（09-06 第七轮，独立临时库 + 本地 siteverify mock：缺 token / 校验失败 / 端点不可达三种 param、表单体 `secret=…&response=…`、撤掉秘钥即关闭） | — |
| `setup.rs` | 空库首启向导 | A | `console_setup`（独立临时库） | — |
| `portal.rs` | `/api/me/*`（key、日志、流水、订单、公开价格、公告） | A C | `console_portal`、`console_portal_pages`、`console_stats::personal_activity_covers_calendar_year_and_isolates_owners`、e2e smoke 门户页 | — |
| `playground.rs` | Playground 同源流式中继 `/api/me/playground/chat`（进程内调数据面处理器、强制 stream、1MB 上限）+ 站点预设公开读（§11.39） | A B C | `console_playground`（SSE 与直打数据面一致且账落同一把 key、无 key 401、超限 413、预设白名单收口）；单元 `force_stream` / `sanitize_presets` | — |
| `channel_oauth.rs` | 订阅 OAuth 登录两步（`/admin/channels/oauth/start` / `exchange`，§11.38）：PKCE 状态 Redis 一次性、换码、建渠道 / 追加 key、审计 `channel.oauth_login` | A C | `gateway_oauth_channels`（三例均经此建渠道） | 无真实上游端到端（mock 授权服务器）；Antigravity / Grok 不在范围 |
| `manage.rs` / `admin.rs` / `query.rs` / `cloud_probe.rs` | 六类管理面 CRUD、批量、写校验（azure / bedrock / vertex 地址、aws_region、出站 / 注入字段）、路由诊断、bedrock / vertex / anthropic_max / codex 测活与模型发现 | A C | `console_manage`（含 `cloud_channel_write_validation`：两家缺地址 400、vertex 地址形状、aws_region 形状、只改地址仍校验）、`console_m2`、`console_users`、`console_visibility`（属主范围 / 分组矩阵）、`console_pricing_write`、`console_channel_test`、`console_import`、`console_diagnose`、`gateway_pricing_rules::console_rule_crud_and_validation` + `rule_toggle_needs_publish_then_stops_and_resumes_discount`（09-08：下线未发布仍按老价、发布后回标价且不进快照、参数保留可复用、404、审计）、`console_users::role_delete_guards_live_bindings_and_ignores_deleted_users`（09-08：只认超管、活人绑着 409、解绑可删并留痕、软删用户的死引用不再撞外键 500）、`console_users::balance_expiry_endpoint_feeds_the_worker_sweep`（09-08：管理面设的有效期被 worker 扫得到，未到期不动 / 到期清零重置 / null 取消 / 404）、`console_redemption::disable_batch_stops_only_unredeemed_codes`（09-08：只停未核销的、幂等、不泄露批次存在性）、`console_cloud_probe`（09-08 第十九轮，3 例）：vertex 两种探测范围各换一次 token（model 范围还真发 generateContent）、token 端点 401 带出上游状态与原文、留痕回填"最近测试"；bedrock 按凭证形态分流（Bearer 列兼容模型 / SigV4 签 InvokeModel 且模型 ID 冒号编 `%3A`）；anthropic_max 与 codex 的"未到期零往返 / 过期先刷新 / refresh 轮转回写 / model 范围真发补全"；三家的 `fetch_models` 各自回 `fetch_models_unsupported` 或 `fetch_models_requires_sigv4` | bedrock 的 SigV4 验凭证与拉模型走 `ListFoundationModels`，控制面主机在 `bedrock.rs` 里固定成 `bedrock.<region>.amazonaws.com`（有意为之），mock 接不上——那条分支只有 `aws_sigv4` 单测与真实凭证能验 |
| `channel_balance.rs` | 上游余额查询（§11.33）：按主机选探针、定点解析、`ch:balance` 留痕 | A | `console_channel_test::channel_balance_probe`（dashboard 口径额度 − 美分用量、凭证错 502 `status_401`、anthropic 400 `balance_unsupported`、列表 `last_balance` 回填）；单元：探针选择 / URL / 四家官方响应形状 / 十进制解析 | DeepSeek / SiliconFlow / OpenRouter / Moonshot 四家按 `api_base` **主机名**硬匹配选探针，mock 只能起在 `127.0.0.1` 上、永远落进 `OpenAiDashboard` 分支——结构上做不了 mock 端到端（09-08 第十九轮核实），不是疏忽 |
| `margin.rs`（+ `crate::margin`） | 负毛利熔断列出 / 解除（§11.34） | A C | `worker_margin_breaker`（列出含渠道名与 active、lift 后同进程立即放行且审计 `margin.lift`、解除期评估器跳过） | — |
| `ratio_sync.rs` | 上游倍率在线同步（§11.36）：三种源形状识别、逐模型逐轴差异、择项应用 | A B C | `console_ratio_sync`（ratio_config / new-api pricing 两源 + 非 JSON + 不可达：`current` / `same` / 缺失三态、按次与倍率不混比、单源失败不阻塞、源数与重名 400；apply 只改选中轴其余保持本地值、按次价 → micro、审计 `pricing.sync_apply`、非法轴 / 负值 400）；单元：三种形状解析、micro ↔ USD 字面量、`1.250000 == 1.25` 规范化、十进制不经浮点。09-08 第十九轮把第三种源形状（另一台 Okapi 的 `/api/pricing`，`{models:[...]}`、按次价是 micro 整数）也接成真源：形状认得、`1.250000` 规范化后判 same、micro 60000 取回 `0.06`、源里没有的轴整个键缺席而不糊成 same | 出站走 `ssrf::validate_api_base` 同一把闸 |
| `analytics.rs` / `stats.rs` / `logs.rs` / `usage_details.rs` / `activity.rs` / `analysis_*` | CH 立方体三端点、看板、日志检索、实时 KPI、毛利 | A B | `console_analytics`、`console_stats`、`console_logs`、`gateway_upstream_cost`；单元 `activity` / `analysis_freshness` / `usage_details` | `console_analytics` 两例曾在全量并行下偶发（outbox 行被别的进程 drain、两张 MV 先后落地），09-06 改为 `poll_until` 全字段谓词，见第 4 节发现 ① |
| `audit.rs` | 管理写操作 + 登录审计 | C | `console_audit`、`console_ops::assist_overview_scoped_and_audited`、`console_mcp_write`（`mcp:{key_id}` 落痕） | — |
| `dlq.rs` | 死信列表 / 重投 / 丢弃 | A D | `console_logs::dlq_list_requeue_and_discard`、`worker_ch::chsink_pipeline_then_dlq`、e2e smoke 运维页 | — |
| `mcp.rs` | MCP Streamable HTTP 只读 + 写工具三道闸 | A C | `console_mcp`、`console_mcp_write`。09-08 第十九轮把 `const TOOLS` 抽出来对表，补齐当时零调用的 7 个（`query_usage` / `list_my_keys` / `list_models_pricing` / `platform_kpi` / `channel_health` / `redemption_create` / `cache_flush`）——**22 / 22 工具现在都被真调过**；同轮发现 `console_mcp` 的 state 没接 CH（两个 CH 工具在里面必然 `stats_disabled`），已接上 | — |
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
| `worker/notify.rs` | webhook / email 多路、事件过滤、频率闸、余额低扫描 | A | `worker_notify`、`console_smtp::notify_email_channel_and_admin_test_send`；09-08 第十九轮补 `worker_alerts_carry_actionable_payloads`（跑在用完即删的临时库上，绕开 `notify_channels` 这个全局键与同二进制并行的老用例）：`drift` / `channel_cooldown` / `balance_low` 三条告警的**真实载荷**在 mock webhook 上逐字段核对（drift 带 `user_ids` 而不只是 count、balance_low 的 `users[]` 带 `balance_micro`、cooldown 的 count 与库里冷却 key 数一致），外加"没事不吵"与频率闸 | 四类告警的载荷现已全部从 `select!` 臂收进可测函数 |
| `worker/mod.rs` | 悬置清理、三方对账、分区维护、冷却恢复、余额有效期、保留策略、订阅滚窗 | A B D | `worker_m2`、`worker_reconcile_repair`、`console_subscriptions::worker_rolls_window_and_expires`、`console_users::balance_expiry_endpoint_feeds_the_worker_sweep`（09-08：管理面设的有效期被 worker 扫得到，两半接上）。09-08 第十九轮按 `select!` 的 8 条 `tick()` 臂逐条对表——chsink / 订阅滚窗 / 悬置清理 / 对账 / 分区 / 冷却恢复 / 熔断 / 余额有效期**全覆盖** | — |
| `worker/margin_breaker.rs` | 负毛利熔断评估（§11.34）：CH 成本已知行按分组×渠道聚合 → `mb:blocks` | A B D | `worker_margin_breaker`（有 CH 才跑：25 笔亏损样本 → tripped 含金额 / 毛利率、网关 503 `margin_blocked`、续期不重复通知、关闭功能清表）；单元 `margin::tests`（阈值边界：样本不足 / 成本过小 / 收入 0 / 负阈值容忍 / 正阈值要求毛利、配置夹取、字段往返）。09-08 第十九轮：通知载荷从 worker 主循环收进 `margin_breaker::evaluate_and_notify`（生产与用例同一段代码），**投递断言也补上了**——`Notifier` 的订阅配置读它自己那个池，于是单挂一个用完即删的临时库绕开 `settings.notify_channels` 这个全局键，在 mock webhook 上核对 `event` / `at` / `blocked_total` / `tripped[]` 五个字段，并验到续期轮不发第二条 | — |
| `mail/` | SMTP 投递、模板 | A | 单元；`console_smtp`（本地 mock SMTP，AUTH PLAIN） | STARTTLS / 隐式 TLS 未在 mock 覆盖 |
| `migrate.rs` | new-api / 老 ok-api JSONL 导入 | A H | 单元；`migrate_newapi`、`migrate_okapi_old`、`schema_shape` | — |

### 2.5 前端（`frontend/src/features`）

| 功能面 | 路由 | 维度 F 子项 | 覆盖 spec | 缺口 / 备注 |
| --- | --- | --- | --- | --- |
| 登录 / 会话 / 权限裁剪 | `/`、`/portal/*` 守卫 | 双登录方式、403 不白屏、导航按权限裁剪、登出清服务端 session；注册关闭不摆表单、邀请制必填 aff、`?aff=` 不手填、验证码 `{email,lang}`、OAuth 只跳 `/auth/oauth/{code}`；邮箱登录 `totp_required` 后才带 `totp_code`；API Key 登录 trim 后作 Bearer；`needs_setup` 才出首启向导；登出 POST `/auth/logout` 空体；`?oauth=done` 兑 key `{name:oauth}`；注册赠送 `new_user_credit_micro` + 邀请码后叠加 `invitee_credit_micro`；首启明文 Key 可复制 | `smoke.spec`（4 例）；`missing-surfaces.spec`（注册 / OAuth / TOTP / API Key / 向导 / 登出 / OAuth 着陆 / 注册赠送，09-08 第二十一轮续） | — |
| 找回 / 重置密码 | `/forgot-password`, `/reset-password` | 登录页链接带邮箱、提交体 email+lang、防枚举成功态、未配 SMTP 501 文案；缺 token 提示、长度与一致性前置校验、成功回登录、失效 token 400 文案；找回 / 重置 500 走 `internal_error` 文案 | `write-forms.spec`（2 例，09-06 新增） | — |
| 新手引导 | `/portal` 快速开始卡 + 顶栏入口 + 密钥页页头 | 进度推导、四步抽屉、客户端片段联动、关闭记忆、移动端与深色 | `guide.spec`（4 例，09-06 新增） | — |
| 试用台 / 聊天客户端一键导入 | `/portal/playground`、密钥回执 → 指南 | 模型只列本分组可用、发送经同源中继且强制流式、流式内容 + usage / 模型脚注、停止按钮中断、预设保存 / 载入 / 站点预设导入、cc-switch（Claude 不带 /v1、Codex 带 /v1）/ NextChat / Cherry Studio 链接形状；发送 500 在助手气泡内走英文 `internal_error`（Playground 固定 `en`） | `playground.spec`（4 例，09-06 新增） | 助手正文纯文本渲染，无 markdown |
| 门户总览 / 日志 / 流水 / 充值 | `/portal`, `/portal/logs`, `/portal/ledger`, `/portal/topup` | KPI 六卡、页签零请求、空态、导出禁用、流水入口；充值下单 + 兑换卡；下单 / 核销 500 走 `internal_error`；门户日志 `scope` / 模型 trim / `errors_only` 进查询、展开账单快照、展开行复制 `request_id`、加载更多 `before`、空表禁 CSV；CSV UTF-8 BOM、金额六位 USD、`=` 公式注入前缀、失败行 `error_code`、仅全账户 scope 带 key 列；流水 micro→USD、标签、退款深链、`before` 翻页；订单 `order_no` 可复制进剪贴板、原币文本、空态与 500；流水 500 与订单 500 均走 ErrorState；总览 `scope`/`days`、key 视角本分钟 RPM、全账户平均 TPM、订阅剩余与到期清零；趋势空窗 `emptyUsageHint`；breakdown 500 走 ErrorState + 重试；门户日志 500 走 ErrorState + 重试；流水空表 hint | `smoke.spec`（2 例）、`charts.spec`（门户图表 5 例）、`interactions.spec`（年度日历 / 热力图 / 个人中心 4 例）、`write-forms.spec`（充值下单 1 例）、`list-writes.spec`（兑换卡：trim、micro 入账、套餐/分组/有效期、`redemption_invalid` 文案，09-08 第二十一轮）、`missing-surfaces.spec`（门户日志 + CSV + 流水 + 总览查询，09-08 第二十一轮续） | — |
| 公开模型广场 / 调用示例 | `/pricing` | 厂商归一、单位切换、深链、阶梯价、模拟器、移动端深色、分页、加载 / 失败 / 空态；未知 `?model=` 走「此模型未发布或已下架」并可关掉 | `smoke.spec`（1 例）、`catalog.spec`（10 例）、`request-examples.spec`（4 例） | — |
| 管理端总览 / 日志 / 洞察 / 质量 / 经营 / 审计 / 运维 | `/admin`, `/admin/logs`, `/admin/stats`, `/admin/quality`, `/admin/revenue`, `/admin/audit`, `/admin/ops` | 实时条、健康芯片、深链即状态、三视图、死信签；运维写：退款先查后退、DLQ 重投/丢弃、对账校准、缩短保留期确认；退款 / 重投 / 清缓存 / 缩短保留 500 走 toast；审计过滤进 URL、行展开 detail、加载更多 `before` 游标、接口 500 走 ErrorState；管理日志过滤进 URL / `hours` / 展开 request_id / 空表禁导出、CSV UTF-8 BOM 与六位 USD；总览待办芯片与深链；组件宕机（PG/Redis/CH）fail-closed 文案、outbox ≥1000 积压、冷却 key；全清「没有待办」；diagnose 500 收起健康芯片、不伪装成组件宕机；消耗排行 micro→USD 且链到日志 `hours=天数×24`，空表 / 500 不伪装成零；日志行内退款仅成功扣费行，`already_refunded` 走幂等文案；质量卡 `days`+`limit` 与高错误率 `errors_only` 深链；错误占比 `formatBp`、top 渠道/模型；空错误码 `(empty)` 走 `errors_only`、upstream 0 与无名渠道显示 `—` / `#id`；空表「窗口内没有失败请求」；错误/客户端 500 走 ErrorState（无重试）；渠道健康 / 模型时延 500 走 destructive 文案（无 ErrorState）；客户端空表走 `trendEmptyHint`；站点规模自动停用 / 未定价深链；站点规模 500 收起整条；实时条 500 收起（不占版面）；KPI `overview?days=` 与实时条 `window=60`，切窗同步 `margin?days=`，趋势卡切「实际消费」；用量分析过滤条深链 `user_id` 进 trend 查询、芯片回填名、加模型、点 × 移除、KPI 环比 `▲ +N%` / 持平与 token mix、万元以上紧凑记法与已采集毛利、有让利时「含让利」；拆分 `by`/`limit=50`、日志 `hours=天数×24`、聚焦后下一层 `by=channel`、名次 `▲`/`▼`/`新`/`—`（名次不变）与环比 `+N%`/`-N%`、切「按」select 保留已有 filter、成本覆盖列、负毛利红字；拆分 500 走 ErrorState（无重试）；空 `data` 走 `trendEmptyHint`；趋势空窗 `trendEmptyHint`；流向空 `nodes`/`links` 走 `trendEmptyHint`；经营资金流入四桶 + 分组表 `groups?days=` + 已采集毛利徽章；cashflow 500 收起资金流入行；groups 500 收起分组表；经营 margin 500 走 ErrorState + 重试；空 `data` 走 `trendEmptyHint`；渠道健康 / 模型时延空表不挂渠道/模型链；死信 500 走 ErrorState（无重试）；对账零差异文案；对账 500 走 ErrorState（无重试）；流向 500 走 ErrorState（无重试）；运维退款查无此单 / 未扣费禁退 / 幂等预览翻已退款；质量趋势缺省 `metric=error_rate`，切平均时延 / 首 Token / 吞吐量与 stack，换天数因 `key={days}` 重挂回默认 metric，高级筛选 `granularity=hour` 进 trend 查询并可重置；只填开始日期不填结束日期会拦下并提示无效区间；按小时且区间超过 31 天不发查询；空窗提示无调用记录；trend 500 走 ErrorState；管理日志 CSV 对 `=…` 单元格加 `'` 前缀，展开行复制 `request_id`；总览趋势卡有 margin 序列时数据表请求数 / 实际消费（micro→USD）；趋势卡 500 走 ErrorState + 重试 | `smoke.spec`（管理端 1 大例）、`charts.spec`（管理图表 6 例）、`write-forms.spec`（毛利熔断）、`list-writes.spec`（DLQ / 退款 / 对账 / 保留，09-08 第二十一轮）、`missing-surfaces.spec`（审计 + 管理日志 / 待办 / 排行 / 行内退款 / 质量卡 / 规模条 / KPI / 实时条 / 排行空态 / 退款幂等 / 过滤条 / 经营分组 / 运维退款三结局 / 拆分 / 质量趋势，09-08 第二十一轮续） | 需演示超管，缺则跳过 |
| 管理端设置 / 高级配置 / 导航 / 分页 | `/admin/settings`, 侧栏, 列表页 | 分组搜索、敏感值不显示、只读无编辑入口、键盘 / 移动端 / IME、URL 即分页状态；公告发布、注册 USD→micro、隐私开关、MCP 写入抽屉、通知多路 Webhook/邮件；高级设置列表 GET `/admin/settings` 500 走 ErrorState + 重试；无筛选空表「暂无数据」+ `settingEmptyHint`；筛选无匹配走同一 hint；站点公告横幅 warning 按 `updated_at` 关掉、换版再出、critical 不可关；通知保存 / 公告再发 500 走 toast | `interactions.spec`（19 例）；`write-forms.spec`（SMTP）；`list-writes.spec`（公告 / 注册 / 隐私 / MCP / 通知多路，09-08 第二十一轮）；`missing-surfaces.spec`（公告横幅，09-08 第二十一轮续） | 隐私 / 公告 / SMTP 单键 GET 失败不走 ErrorState（表单按缺省空值渲染） |
| 用户 / 密钥 | `/admin/users`, `/portal/keys`, `/admin/keys` | 搜索回车、抽屉落地签、删除二次确认手输名称；用户列表 `q` trim 进 URL 与查询、空结果、停用态、倍率原样；无筛选空表「暂无数据」；列表 500 走 ErrorState + 重试；令牌管理检索进 URL 与查询串、空检索「暂无数据」、到期日、限模型/IP 徽章、累计用量、日志深链 `api_key_id`、停用 PATCH `status`、停用后列表翻成启用再 PATCH `status: 1`、删除经确认框；列表 500 换检索后走 ErrorState + 重试；门户新建 `/auth/keys` 带分组与 IP；401 关抽屉并提示需邮箱密码会话；明文 Key 可复制；停用后列表翻成启用再 PATCH `{status:1}`；列表钉住档位 / 来源数 / 用量 / RPM；重命名 PATCH `{name, group_code, ip_allowlist}`（空名禁保存、trim、跟随分组发 null）；行内用量 `entity-usage?kind=&ids=&days=7`，501 显示 — 不伪装成零；空表 hint 与空态「新建密钥」打开抽屉；列表 500 走 ErrorState + 重试 | `smoke.spec`（门户删除二次确认）；`missing-surfaces.spec`（`/admin/keys` + 行内用量 + 用户搜索，09-08 第二十一轮续）；`list-writes.spec`（门户新建 / 401 会话 / 重命名 / 停用 / 启用 / 删除 / 列表钉住，09-08 第二十一轮续） | — |
| 用户抽屉写操作 | `/admin/users` 管理抽屉 | 入账 USD → micro 整数（含 0.29 浮点边界）、系数按十进制字符串提交且负数 / 未改动不放行、分组全量覆盖且先出现者优先级高、封禁经确认框且成功后翻成解封；解封无确认框、提为管理员 / 降为普通用户直接 POST `{action}`、软删除经确认框；入账 / 系数 / 分组 / 封禁 / 发放订阅 500 走 toast；角色只发改动的那一项、订阅下拉只列在售订阅套餐且发放 / 立即结束各打端点、余额有效期日期 → UTC 零点 RFC3339 且清空发 null；用量签 `usage?days=7`、近 7 天消费 micro→USD、流水操作者、日志深链 `hours=168`；空 daily / ledger 走默认 EmptyState；`stats_available: false` 提示 `stats_disabled`；overview / usage 500 走 ErrorState；订阅 GET 500 走 ErrorState | `write-forms.spec`（2 例，09-06 新增 / 第八轮）；`missing-surfaces.spec`（用量签，09-08 第二十一轮续） | — |
| 模型定价抽屉与发布 | `/admin/pricing` 编辑 / 新建 / 发布 | 七个倍率轴按十进制字符串提交、空档位行过滤、`tier_expr` 去空格回传且模式提示随之切换、无档位不发 `tier_ratios` 键、降级链原样回传、编辑态模型名只读；发布按钮 POST `/admin/pricing/publish` 并提示新 epoch；发布 500 走 toast；列表「仅看未定价」进 URL 与 `unpriced=true`，搜索 `q` 一并带上，空检索「没有匹配的结果」可清空；已定价行列倍率 / `$ / 1M` / 音频轴 / 无渠道 vs 渠道深链；删除手输模型名打 `DELETE /admin/models/{name}`，`requires_publish` 提示需发布；无筛选空表 hint；列表 500 走 ErrorState + 重试 | `write-forms.spec`（09-06 新增 / 第八轮）；`missing-surfaces.spec`（仅看未定价 / 删除，09-08 第二十一轮续） | 模型没有状态切换 UI（`status` 只随导入 / 删除变化），此前备注有误 |
| 兑换码 | `/admin/codes` | 分页 / 筛选复位 / 末页停用；生成抽屉：面值 USD → micro（0.29 边界）、绑定用户去空格转数字、空限额不发键、过期时间按浏览器本地换 UTC、面值 0 禁提交、400 错误码文案且可重发、生成 500 走 toast、成功态明文一次性 + 复制全部；列表「停用整批」仅未使用可点，确认后 `DELETE /admin/redemptions/{batch}`，toast `affected` 张数，已核销禁点；空表 hint 与空态「生成」打开抽屉；列表 500 走 ErrorState + 重试 | `redemptions.spec`；`write-forms.spec`（1 例，09-07 第十二轮）；`missing-surfaces.spec`（停用整批，09-08 第二十一轮续） | — |
| Playground 试用台 + 一键导入 | `/portal/playground`、密钥回执 | 模型下拉只列本分组可用、发送 → 流式内容 + usage 脚注、停止按钮中断、预设保存 / 载入 / 站点预设导入、密钥回执四个客户端导入链接形状 | `playground.spec`（4 例，SSE 桩） | 流式桩为一次性回包（Playwright 限制），逐字动画不逐块验证 |
| 订阅套餐 | `/portal/plans` | 在售 / 已订阅高亮 / 停用说明 / 下单参数；空表「暂无在售套餐」；列表 500 走 ErrorState + 重试；我的订阅 500 走 ErrorState（无重试） | `subscriptions.spec`；管理端见下「套餐抽屉」「套餐删除」两行 | — |
| 渠道抽屉「请求与计费行为」 | `/admin/channels` 编辑抽屉 | 已有 proxy / 额外头回显；注入字段按 JSON 解析（数字 / 带引号字符串）；清空额外头即从 settings 删键；PATCH 体只含有值的键；受保护键 400 → 错误码文案且抽屉不关；保存 500 走 toast | `write-forms.spec`（1 例，09-06 新增） | — |
| 渠道抽屉接入 / 模型 / 调度 + 新建 | `/admin/channels` | 协议只读；凭证轮换独立端点且成功后清空；拉上游模型覆盖清单并提示数量；成本倍数 → 千分比、留存声明、优先级随 PATCH；池成员单独保存、覆盖值整数化、非整数归 null；清空全部池成员就地红字并 toast 孤儿不可达；新建三件必答事齐才放行、池成员随建渠道提交；新建抽屉 ModelPicker 空表「尚无模型」；`GET /admin/models` 500 走 ErrorState；key 级参数行权重 / 并发各自 PATCH（空并发 = null）、失效 key 重新启用；拉上游模型 / 凭证轮换 / 池成员保存 / 新建渠道 / key PATCH 500 走 toast | `write-forms.spec`（2 例，09-06 第七 / 八轮） | — |
| 套餐删除 | `/admin/plans` | 确认框 → `DELETE /admin/plans/{code}` | `write-forms.spec`（09-06 第八轮） | — |
| 安全页会话卡 | `/portal/security` | 列表 + 当前浏览器徽章、单条吊销打 `DELETE /api/me/sessions/{sid}`、全部吊销打 `DELETE /api/me/sessions`、空态文案；吊销 500 走 toast | `write-forms.spec`（1 例，09-06 新增） | —（TOTP 绑定见下行，第六轮已覆盖） |
| 安全页最近登录 | `/portal/security` | 预览 8 行、失败原因、仅失败筛选、展开其余；接口 500 走空态文案、不伪装成零 | `missing-surfaces.spec`（1 例，09-08 第二十一轮续） | — |
| 套餐抽屉 | `/admin/plans` 编辑 / 新建 | 充值模板与订阅两形态字段互斥（切换即替换字段区）、USD → micro、天数 `Math.trunc`、空值不发键、订阅缺有效期禁用保存、售价空 = 0 不售卖、编辑态代码锁定；列表充值模板 vs 订阅列（每窗额度 / 周期、不售卖、订阅人数）；空表 hint 与空态「新建套餐」打开抽屉；列表 500 走 ErrorState + 重试 | `write-forms.spec`（1 例，09-06 第四轮）；`missing-surfaces.spec`（列表列，09-08 第二十一轮续） | —（删除见上「套餐删除」行，第八轮已覆盖） |
| 角色抽屉 | `/admin/roles` | 权限点来自 `/admin/permissions`、整组切换、无权限点禁用创建、编辑态 code 锁定且已有权限预勾、删除经确认框、后端 409 `role_in_use` 渲染成文案；权限清单 500 走 ErrorState（无重试）；列表前 4 个权限点 + `+N 项`；空表提示内置三档无需配置，空态按钮打开新建抽屉；列表 500 走 ErrorState + 重试 | `write-forms.spec`（1 例，09-06 第四轮）；`missing-surfaces.spec`（列表截断，09-08 第二十一轮续） | — |
| 价格分组抽屉 | `/admin/groups` | 倍率字符串去空格、池从 `/admin/pools` 选、`PoolReach` 就地可达；池详情 500 时摘要收起；自选开关、编辑态分组码只读、内置默认组删除禁用、新建缺省倍率 1 / 池 default；列表「可自选」徽章、零渠道「空池」、限流列 `60 / 分 · ∞ / 时` 与两边不限 `—`；空表 hint 与空态「新建分组」打开抽屉；列表 500 走 ErrorState + 重试 | `write-forms.spec`（1 例，09-06 第六轮；09-07 第十五轮补限流字段：回显、负数 / 小数 aria-invalid 禁保存、清空发 null、整数原样；09-08 第二十一轮续补列表徽章） | — |
| 渠道列表余额按钮 / 运维页毛利熔断卡 | `/admin/channels`, `/admin/ops` 毛利熔断页签 | 钱包按钮只对 openai / openai_compat 显示、结果 toast 按上游货币 Intl 格式化、"最近测试"列下回填余额、`balance_shape` 等错误码文案；熔断卡配置表单（美元 → micro、百分比 → 万分比含负号、分 → 秒、时 → 秒，非法数字禁保存、未改动禁保存）、负毛利行标红、解除只带分组 × 渠道定位对并提示到期；启用且无暂停对时空表 hint；未启用走 disabled hint；列表 500 走 ErrorState（无重试） | `write-forms.spec`（2 例，09-07 第十四轮） | — |
| 计费规则抽屉与列表 | `/admin/rules` | 编辑态四类字段回填与 code 锁定、按类型只发该类型字段、阈值 USD → micro、星期勾选升序、空范围不发键、上下线打 toggle 且提示需发布、删除经确认框；列表阶梯/时段人话化、独占/最优叠加标签、范围「全部」vs 分组·模型·用户；空表 hint 与空态「新建规则」打开抽屉；列表 500 走 ErrorState + 重试 | `write-forms.spec`（1 例，09-06 第六轮）；`missing-surfaces.spec`（列表人话化，09-08 第二十一轮续） | — |
| 设置 SMTP 卡 | `/admin/settings` 邮件页签 | 单键回显、去空格、`reply_to` 空转 null、端口越界归零、加密方式分段、未保存前测试禁用、测试信按已保存配置发且收件人须含 @、有草稿时禁发；测试信 / 保存 500 走 toast | `write-forms.spec`（1 例，09-06 第六轮） | 单键 GET 失败不走 ErrorState（表单按缺省空值渲染） |
| TOTP 绑定 | `/portal/security` | 开始绑定拿 otpauth / pending、码不足 6 位禁用、错码 `totp_invalid` 文案可重试、成功切已开启态、无会话 401 降级提示；otpauth 链接可复制；enroll / confirm 500 走 destructive 文案 | `write-forms.spec`（1 例，09-06 第六轮） | — |
| 渠道池抽屉与列表 | `/admin/pools` | 策略 / 降级目标回填、降级目标排除自己、不降级发 null、编辑态池码只读、内置池与被引用池删除禁用、删除经确认框；列表「内置」徽章、空池标红、策略文案、引用 `N 个分组 / M 个令牌` 与降级目标计数；空表 hint 与空态「新建池」打开抽屉；列表 500 走 ErrorState + 重试 | `write-forms.spec`（1 例，09-06 第七轮）；`missing-surfaces.spec`（列表徽章，09-08 第二十一轮续） | — |
| 团队 | `/portal/teams` | 建团名字去空格、成员上限 USD → micro 且空即 null、提交后表单复位、发团 key 明文只展示一次且可复制、列表 401 整页降级且隐藏创建入口；列表角色「所有者」与钱包 micro→USD；详情抽屉钱包 / 本月 / 累计 micro→USD，空上限显示「不限」；空表 hint；列表 500 走 ErrorState（无重试）；usage 500 成员表 ErrorState | `write-forms.spec`（1 例，09-06 第七轮）；`missing-surfaces.spec`（列表 + 详情用量，09-08 第二十一轮续） | usage 失败时成员表走 ErrorState，钱包仍 `?? 0` 显示 $0（未改产品） |
| 邀请返利 | `/portal/aff` | 链接带 `?aff=`、人数与累计返利按 micro 格式化、接口失败不伪装零；复制邀请链接写入剪贴板 | `missing-surfaces.spec`（1 例，09-08 第二十轮 / 第二十一轮续） | — |
| 导入定价 / 在线同步 | `/admin/pricing` 导入抽屉 | 粘贴 JSON 整段 POST `/admin/pricing/import-newapi`；非法 JSON 原地报错；在线同步空源不拉、拉取体是去空白的源列表、默认不选、点源值才 POST `/admin/pricing/sync/apply`（`changes: [{model,axis,value}]`）；无差异走 `syncNoDiff`；粘贴导入 / 拉取 / 应用 500 走 toast | `missing-surfaces.spec`（1 例，09-08 第二十轮） | — |
| 渠道列表批量 / 复制 / 测活 | `/admin/channels` 列表 | 复制 POST `{name:-copy}`；行测活带第一个模型；测全部只打 `status=1` 且空体；测全部失败汇总 toast（`role=status`）；行测活 `scope=model` 走 `testModelFail`（`role=alert`）；无 scope 走 `testFail`（`测活失败：{{code}}`）；批量 `enable/disable/delete`；单删手输名称打 `DELETE /admin/channels/{id}`；搜索 `q` trim 与协议 `provider` 进 URL，空结果可清空；无筛选空表 hint 与空态「新建渠道」打开抽屉；列表 500 走 ErrorState + 重试；供应商控制台链（openai / anthropic 固定站、`openai_compat` 取 `api_base` origin、非法 `api_base` 不显示）；列表自带 `last_balance` 按上游币种格式化（含余额 0） | `list-writes.spec`（1 例，09-08 第二十一轮）；`missing-surfaces.spec`（搜索 / 控制台链 / 列表余额，09-08 第二十一轮续） | — |
| 路由诊断抽屉 | `/admin/channels` | 缺模型禁用诊断；查询 `model` trim + `group` / `pool`；结论文案、渠道淘汰原因、经降级池标记；接口 500 走 toast | `missing-surfaces.spec`（1 例，09-08 第二十一轮续）；此前 `interactions.spec` 只验模型联想 | — |
| 渠道健康时间线 | `/admin/channels` 近 24h 列 | 点开会抽屉；`hours` 进查询；无流量空态；日志深链带 `channel_id` / `hours` / `errors_only`，分析深链带 `days`；列表级 `days=1&limit=100`、部分可用 / 冷却 / 未入池、错误率 `formatBp`；OAuth token 已过期灰字；无流量行 `—`；测活成功 `N ms` / 失败 `HTTP 429` / 无 key「没有 key」；全部可用文案；渠道级停用显示「停用」；`last_test` 空显示未测过；时间线 500 走 ErrorState | `missing-surfaces.spec`（时间线抽屉 + 列表 key 状态 / 近 24h / 测活，09-08 第二十一轮续）；此前只有 `screenshots.spec` | — |
| 订阅 OAuth 登录卡 | `/admin/channels` 抽屉 | 实验性提示；`start` 只带 `provider`；缺名 / 模型不换码；新建发 `name`+`models`，追加发 `channel_id`；成功换码后须再点「打开登录页」才回到粘贴区；追加失败后按钮翻成「重新打开登录页」；`start` / `exchange` 500 走 toast | `missing-surfaces.spec`（1 例，09-08 第二十轮） | — |
| i18n | 全站 | 裸文案零、双语言包键对齐；顶栏语言菜单切 `en` 写 `okapi.lang`；主题菜单深色挂 `html.dark` 并写 `okapi.theme`，跟随系统则清除 | `guard-i18n.sh`、`guard-i18n-keys.py`；`guard-error-codes.py`（09-07 第十二轮：后端 `codes::*` + `AppError::new / unauthorized` + `StoreError::Conflict` + 模块级 const 的字面量全集 → 两语言包 `errors` 命名空间反向核对，进 CI）；e2e 断言同时匹配中英正则；`missing-surfaces.spec`（语言 / 主题菜单，09-08 第二十一轮续） | — |

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
12. ~~软删留下的外键死引用未逐一排查~~ **已排查完毕并全部修掉（09-08 第十九轮）**：形状是"占用检查滤掉软删行 + 外键无 `ON DELETE`"两个前提同时成立，墓碑上的死引用就既不算占用、又拦得住硬删，撞出 500 且那个配置项**永远删不掉**。按 `0001_init.sql` 的 `REFERENCES` 列逐个对谓词，三处命中全修：`delete_role`（`users.admin_role_id`）、`delete_price_group` 与 `delete_channel_pool`（软删 `api_keys` 的 `group_override` / `pool_override`，后两处比角色更容易撞——令牌删得比用户勤）。修法统一为"确认无活引用后在同一事务里把软删主体的引用列置 NULL 再硬删"，回归见 `console_users::role_delete_guards_live_bindings_and_ignores_deleted_users` 与 `channel_pools::deleting_group_or_pool_ignores_soft_deleted_key_overrides`。通则已写进 IMPLEMENTATION §删除语义定案：**新加配置类硬删时，占用检查滤掉软删行的就必须在同一事务里清掉那些行的引用列**。
13. ~~列表级写操作与少量 HTTP 路由仍无前端 / 直打覆盖~~ **已补（09-08 第二十一轮）**：前端 `list-writes.spec` 覆盖渠道列表批量 / 复制 / 测活、运维 DLQ / 退款 / 对账 / 保留、充值兑换卡、设置公告 / 注册 / 隐私 / MCP / 通知多路、门户密钥新建。后端 HTTP：`GET /admin/pools/{code}`、`GET /v1/models`、`DELETE /admin/channels/{id}`（只盖 tombstone，与 batch 的停用+停 key 不同）、`DELETE /admin/keys/{id}`、`DELETE /api/me/keys/{id}`。审计里标成零覆盖、但第十九轮已有集成的不要重做：规则 toggle、余额有效期、角色删除、OAuth provider 列表、兑换批次停用、令牌 PATCH。

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
| L1 + L2 | 审计前 **529 / 529**（108 个二进制）→ 第一批补完 **533 / 533** → 路由×方法对表后 **539 / 539** → 再换三条轴（worker 分支 / 错误码 / 设置键）补完 **544 / 544**（110 个二进制）。每跑都是 `env -i` 只给 CI 那三个连接串。**踩过一次自己挖的坑**：CH 连接串的变量名是 `OKAPI_CLICKHOUSE_URL` 不是 `OKAPI_CH_URL`，写错时 `dotenvy` 静默回落到 `.env` 里那台两天没清过的共享 dev CH，`console_ops` 的两个 CH 断言拿到别的会话的历史数据、报出看不懂的 `left: 5000`。跑隔离环境时值得先 `rg` 一眼代码读的到底是哪个变量名 |
| L3 前端交互（接口桩） | 审计前 **83 / 83**；补完充值下单一例后 **84 / 84** |
| L4 前端冒烟（打真实 console） | **94 通过 / 4 跳过**；跳过的四例全在 `screenshots.spec`（截图工具，非功能用例） |
| L5 部署形态 | `smoke-all.sh` 三断言过 + root 已存在故跳过 key 断言；`verify-deploy.sh` embed-web 四断言过（含 `guard-deploy-manifests`）；`OKAPI_VERIFY_IMAGE=1` 的发布镜像五断言过（`git archive HEAD` 快照构建，229MB，65534 运行，空库首启迁移 + Setup 向导） |

审计发现与处置：

| 发现 | 取证 | 处置 |
| --- | --- | --- |
| **两个会打上游的数据面端点没有任何限流准入（D）** | 把 `gateway/` 下 `authenticate_data_plane` / `check_member_limit` / `check_group_rate` 三个调用点排在一起看：八个计费端点都是"鉴权 → 成员限额 → 分组限流"三连，只有三个端点鉴权完就直接往下走——`/v1/messages/count_tokens`（`chat.rs:488`）有 anthropic 候选时会拿渠道凭证代理上游 tokenizer，`/v1/videos/{task_id}` 与 `/content`（`videos.rs` 的 `relay_task`）拿渠道凭证轮询 / 下载，两者都不计费所以不在 §11.32 的"全部计费端点"口径里，可用户能拿一把 key 无限打、无限消耗渠道配额与上游速率。第三个 `/v1/dashboard/billing/*` 只读本地库，不碰上游，维持不设限 | **已修**：两处补 `check_group_rate`，**不补** `check_member_limit`——后者是团成员的月度**消费**上限，用它挡住不花钱的轮询与下载，会让刚好花超的成员连已经付过费的视频都取不回来。限流该按"会不会打上游"划界而不是"扣不扣钱"，IMPLEMENTATION §11.32 先补定案再改码。`gateway_group_rate::non_billing_upstream_endpoints_are_rate_limited` 钉住（`count_tokens` 前两笔 200 第三笔 429 且是 Anthropic 错误壳、视频轮询在任务查找**之前**就 429 所以拿到的是 429 而非 404、下载入口同）。撤掉修复复跑确认用例会红（第三笔回 200），不是摆设 |
| **`console/cloud_probe.rs` 整个模块零集成覆盖** | 该模块 234 行全是分派：bedrock / vertex / anthropic_max / codex 四家 × credential / model 两种探测范围 + `fetch_models`，签名、换 token、刷新都在这条路上。`bins/okapi/tests` 里请求过 `/admin/channels/{id}/test` 与 `/fetch-models` 的只有 `console_channel_test`，而它建的全是 openai 渠道——四家云渠道的测活按钮与拉模型按钮从来没被端到端走过 | **已补** `console_cloud_probe.rs`（3 例）：vertex 两种范围各走一遍换 token（credential 只换 token、model 还真发一次 generateContent）、token 端点 401 时把上游状态与原文带出、`fetch_models_unsupported`、留痕回填列表页"最近测试"；bedrock 两种凭证形态分流（Bearer 走 `/openai/v1/models`、SigV4 走签过名的 InvokeModel 且模型 ID 冒号编成 `%3A`、Bearer 拉模型回 `fetch_models_requires_sigv4`）；订阅两家的"没到期零网络往返 / 过期先刷新再探 / 轮转出的 refresh_token 回写 / model 范围真发一次补全（codex 非流式靠 SSE 聚合成 JSON）" |
| **倍率同步的第三种源形状只有单测，从没当真源拉过** | `ratio_sync.rs` 认三种上游：`ratio_config`、new-api 的 `/api/pricing`、另一台 Okapi 的 `/api/pricing`。前两种在 `console_ratio_sync` 里都有真起 mock 源、走 `/admin/pricing/sync/fetch` 的用例，第三种只有一个解析单测——而这一种恰好是唯一"按次价用 micro 整数"的形状，字面量换算错了单测未必拦得住（源里没有的轴该整个键缺席，不能糊成 `same`） | **已补**：mock 源加 `/api/okapi-pricing` 端点，三源同拉一次。钉住形状认得、`1.250000` 与本地 `1.25` 规范化后判 `same`、`per_call_micro: 60000` 取回字面量 `0.06`（不是 `0.060000` 也不是浮点尾巴）、源里缺的 `cache_write` 轴整个键不出现 |
| **门户充值从下单到跳支付页没有前端 e2e** | §2.5 备注写着"充值下单跳转支付页无 e2e（有后端 `console_pay`）"。后端只管到下单接口回 `pay_url` + `params`；从金额输入框到真的跳走这一段（USD 文本 → `amount_micro` 整数、epay 要拼隐藏域表单 POST、Stripe 直接跳链接）全在前端，错了钱就付不出去 | **已补** `write-forms.spec` 一例：快捷档回填输入框并置 `aria-pressed`；低于最低额时提示 + 禁提交 + 一个请求都不发；`12.34` 提交成 `amount_micro: 12_340_000`（不是 `12339999.99…`）；epay 拿到的是 POST 到 `pay_url`、`pid` / `sign` 原样进表单体；Stripe 是 GET 直跳 checkout 链接；网关回 `pay_url: null` 时原地报错、不跳走 |
| 熔断通知从没在线上验过：载荷拼在 worker 主循环的 `select!` 臂里，且投递无断言 | §2.4 早就记着"未在 mock sink 上断言载荷"。拼在循环里的话，测试只能照抄一份 `json!`，改坏了照样绿 | **两步都做完了**。① 把那 20 行收进 `margin_breaker::evaluate_and_notify`，生产与用例走同一段代码，主循环只剩一次调用与错误分支。② 投递断言此前卡在"`settings.notify_channels` 是全局键，写它会和 `worker_notify::notify_dispatch_and_mute` 互相覆盖"——**绕开的办法是 `Notifier` 的订阅配置读的是它自己那个池**，于是给它单挂一个用完即删的临时库，报告仍来自共享库与 CH，两边互不干扰。`worker_margin_breaker` 现在在 mock webhook 上核对真实包络：`event=margin_breaker`、带 `at` 时间戳、`blocked_total` 与报告一致、`tripped[]` 里本用例那对的五个字段（分组 / 渠道 id / 请求数 / 收入 / 成本 / 毛利万分比）逐个对得上；续期那轮**在线上验到"没有第二条告警"**（此前只在报告对象里验） |

#### 换个角度再扫一遍：路由 × 方法机械对表

按 §2 矩阵逐行读是"照着清单找漏"，会漏掉**清单本身就没登记**的东西。所以又做了一遍不依赖清单的机械审计：
从 `bins/okapi/src` 把 `.route()` 全量抽出来得到 **154 个 (方法, 路径) 对**，逐对在 `bins/okapi/tests` 里搜路径正则（占位符按 `{var}` 与真实值两种写法都匹配）并在命中行的上下文窗口里找请求方法，零命中的挑出来人工复核。同法对前端 36 个路由 × `frontend/e2e` 对了一遍——**前端零缺口**。后端挑出五条真空白，另外把 MCP 的 22 个工具单独对了一次表：

| 发现 | 取证 | 处置 |
| --- | --- | --- |
| **删角色遇到软删用户就 500，而且从此永远删不掉（D，真缺陷）** | `DELETE /admin/roles/{code}` 后端零集成覆盖——前端 e2e 只桩过 409 的**渲染**，后端有没有真发 409 没人验过。补用例时撞出真问题：`users.admin_role_id` 是**无 `ON DELETE`** 的外键，而用户是软删（只置 `deleted_at`，绑定原样留着）；占用检查却带 `deleted_at IS NULL`，于是墓碑上的死引用既不算占用、又拦得住 `DELETE FROM admin_roles`——`internal_error` 500，且这个角色再也删不掉 | **已修**（先改 IMPLEMENTATION §删除语义定案再改码）："占用"只算活着的引用；`mutate::delete_role` 改成事务，先把软删用户的 `admin_role_id` 置 NULL 再删角色。这不违背"不静默级联"——那条规矩防的是活人被悄悄改了计费口径，而软删用户已经 `status=2`、令牌全停，绑定不产生任何权限或计费效果。`console_users::role_delete_guards_live_bindings_and_ignores_deleted_users` 钉住四件事：带 `role.manage` 的管理员也删不了（只认超管，防提权链）、活人绑着 409 `role_in_use`、解绑后 200 且落审计 `role.delete`、不认识的 code 404；最后一段是回归本身（改前 500，改后 200 且墓碑上的死引用被清掉） |
| **`/auth/oauth-providers` 零覆盖，而它读的正是装着 `client_secret` 的那条设置** | 登录页据以决定露出哪些第三方按钮的公开端点，前后端都没有用例。它 `SELECT value FROM settings WHERE key='oauth_providers'` 后只映射 `p.code`——只要哪天顺手把整行 value 回出去，秘钥就跟着到了**未登录**的登录页上 | **已补** `console_oauth_presets::provider_list_is_public_and_leaks_no_credentials`（该套件本就跑在独立临时库上）：不带任何凭证 200、只出三个 code 且保持配置顺序、整个响应文本里搜不到 `sec-` / `cid-` / `client_secret` / `client_id` / `token_url`；再把设置改成不认识的形状、以及整条删掉，两种都回空列表而不是 500（否则登录页整页挂掉） |
| **计费规则上下线零覆盖，"下线到底停不停得掉"从没验过** | `POST /admin/pricing/rules/{code}/toggle` 在后端零命中。这条链路有个容易踩空的地方：PriceBook 是编译期快照，toggle 只改库并回 `requires_publish`，不发布的话在跑的网关照旧打折 | **已补** `gateway_pricing_rules::rule_toggle_needs_publish_then_stops_and_resumes_discount`：8 折在线时 240 → 192；无 `pricing.write` 的用户 403；下线后**未发布仍是 192**（这是定案语义，不是 bug，一并钉住）；发布 + 热更后回 240 且快照里不再有这条规则；列表回显 `enabled=false` 而参数 `0.8` 原样保留（下线不删配置就是为了复用）；重新上线 + 发布回到 192；不认识的 code 404；两次上下线各落一条审计且 detail 带目标状态 |
| **兑换码批量停用零覆盖——印错一批码之后唯一的补救手段** | `DELETE /admin/redemptions/{batch}` 后端零命中，前端只桩过按钮 | **已补** `console_redemption::disable_batch_stops_only_unredeemed_codes`：三张码先核销一张，停用只 `affected=2`，库里状态是 `[2,3,3]`——**已核销的那张不许被改成"已停用"**（改了的话那笔入账在对账时就成了无源之水）；停用后的码核销 404 且余额分文未动；重复停用幂等 `affected=0`；不存在的批次 200+0（不泄露存在性），批次号不是 uuid 则 400；普通用户 403；两次留痕 detail 带 affected |
| **余额有效期只测了 worker 那一半** | `worker_m2::balance_expiry_drains_and_records` 验的是清零扫描，但它是拿**裸 SQL** 写的 `balance_expires_at`——真正给用户设有效期的 `POST /admin/users/{id}/balance-expiry` 从没被打过，两半之间是否接得上没人验 | **已补** `console_users::balance_expiry_endpoint_feeds_the_worker_sweep`，把两半接上：无 `user.balance_adjust` 403；设到未来 → 落库时间与请求一致、扫一轮不碰它、余额纹丝不动；改成已过期 → 同一轮扫描清零 7000 micro、到期时间重置防重扫；传 `null` 取消；不存在的用户 404；三次调用各留一条 `user.balance_expiry` 审计且取消那次 detail 为 null |
| **22 个 MCP 工具里 7 个从没被任何用例调用过** | 把 `mcp.rs` 的 `const TOOLS` 抽出来对 `console_mcp` + `console_mcp_write` 搜工具名，零命中的是 `query_usage` / `list_my_keys` / `list_models_pricing` / `platform_kpi` / `channel_health` / `redemption_create` / `cache_flush`。后两个是**写**工具——`redemption_create` 是唯一能凭空造出可入账凭证的 MCP 工具 | **已补两处，7 个全覆盖**。只读五个进 `console_mcp::readonly_tools_cover_keys_pricing_health_and_ch_backed_usage`（顺带发现该套件的 state 压根没接 CH，`query_usage` / `platform_kpi` 在里面必然 `stats_disabled`，已接上）：`list_my_keys` 数量与本人 key 数**相等**（不多列别人的）且只回前缀；`list_models_pricing` 三个轴都是十进制**字符串**（漏成 JSON number 的话 0.1 类倍率过一趟就带二进制尾巴）；`channel_health` 带出渠道下 key 的 id / status / 冷却时间；`platform_kpi` 与 `channel_health` 对普通用户既不在 `tools/list` 里也调不动；`query_usage` 的 `scope=key/user` 两张物化视图各走一遍且 `days=999` 夹到 90。写的两个进 `console_mcp_write` 阶段 6：`redemption_create` 预览不落库不吐明文、确认后三张 $2 码入库且**库里存的是 sha256 不是明文**、参数非法走 `result.isError` 而不是 RPC `error` 通道（顺手把这两条错误通道的区别写成 `assert_tool_error` 固定下来）；`cache_flush` 三种范围各刷一次、范围不认得报错；两者都以 `mcp:{key_id}` 留痕 |

#### 顺着删角色那个 500 把同类缺陷一次挖干净

删角色撞外键是"占用检查滤掉软删行 + 外键无 `ON DELETE`"这一对前提凑出来的，而这两个前提在别处也可能同时成立——补一个用例修一处、把同形状的坑留在库里等下一次线上撞，等于没修。于是按 `crates/okapi-store/migrations/0001_init.sql` 把所有 `REFERENCES` 列抽出来，逐列问两个问题：**指向的表会不会被硬删？软删主体的这一列会不会被占用检查漏掉？**两个都是"是"的，就是同一个坑。

结果：三处命中，除已修的 `delete_role` 外还有两处，都在软删的 `api_keys` 上——`group_override → price_groups`（`delete_price_group`）与 `pool_override → channel_pools`（`delete_channel_pool`），两处的占用检查都写着 `AND deleted_at IS NULL`。写用例先复现，两处都是 `23503` 外键违约（`api_keys_pool_override_fkey` / `api_keys_group_override_fkey`），也就是说**软删过一把带覆盖的令牌之后，那个分组或渠道池就再也删不掉了**——比角色那处更容易撞上，因为令牌的删除比用户频繁得多。按 `delete_role` 同一形状修：两个函数都改成事务，确认没有活着的引用之后先把软删令牌上的覆盖列置 NULL 再硬删。`channel_pools::deleting_group_or_pool_ignores_soft_deleted_key_overrides` 一例盖住两处，且每处都先验"活令牌绑着仍是 409"再验"软删之后能删且墓碑上的死引用被清掉"——不能为了让删除通过就把 409 一起放宽了。

其余 `REFERENCES` 列不在此列，各有各的理由：指向 `plans` / `models` / `channel_pools` 之间的那些，来源表本身不软删（硬删就会真触发外键，占用检查拦得住）；`user_groups` / `team_members` / `api_keys.user_id` 这些已经带 `ON DELETE CASCADE` 或 `SET NULL`。IMPLEMENTATION §删除语义定案里把这条从"某个函数的特例"提成了通则，并写明新加配置类硬删时照此办理。

#### 再换三条轴：worker 定时分支 / 错误码 / 设置键

路由对完了不等于对完了——网关只是三个角色里的一个，还有一整套**没有 URL** 的东西：worker
的定时任务、告警事件、以及一堆"配了才生效"的设置键。这些都不在路由表里，按路由审计一次也照不到。
于是又机械对了三张表：

| 轴 | 口径 | 结果 |
| --- | --- | --- |
| worker 定时分支 | `worker/mod.rs` 的 `select!` 里 8 条 `tick()` 臂，逐条抽出入口函数搜测试 | 补完通知那三处后**全覆盖**（chsink / 订阅滚窗 / 悬置清理 / 对账 / 分区 / 冷却恢复 / 熔断 / 余额有效期） |
| 通知事件 | `notifier.dispatch()` 的事件名全集 = `drift` / `channel_cooldown` / `balance_low` / `margin_breaker` | 只有 `margin_breaker` 验过（还是本轮刚补的），另外三个的**载荷**零断言 → 已补 |
| error_code | 按 `guard-error-codes.py` 同一套抽取（`AppError::new` / `unauthorized` / `StoreError::Conflict` / 本地常量），65 个 | 19 个在用例里零字面断言，逐个复核后补 5 处、判 14 处不值当（见下） |
| settings 键 | 代码里 `setting_cached(...)` 与 `WHERE key = '...'` 的全集，21 个 | 2 个后端零覆盖：`surge_inflight_threshold`、`site_url` → 都已补 |

| 发现 | 取证 | 处置 |
| --- | --- | --- |
| **三条告警的载荷从没被看过一眼** | 上一段刚把 `margin_breaker` 的载荷从 `select!` 臂里收进 `evaluate_and_notify`，可另外三条还原样躺在循环里：`drift` / `channel_cooldown` / `balance_low`。`worker_notify` 里那两个用例一个用的是合成事件名 `drift_<uuid>`（验的是包络与频率闸），一个只验扫描函数的返回值——**中间那段"扫出来的东西怎么拼成告警"谁都够不着** | **已补**：三处照 `evaluate_and_notify` 收进 `reconcile_and_notify` / `notify::channel_cooldown_and_notify` / `notify::balance_low_and_notify`，主循环各剩一次调用。`worker_notify::worker_alerts_carry_actionable_payloads` 在 mock webhook 上核对真实载荷：`drift` 带 `user_ids`（只给 count 的话运维不知道去查谁）、`balance_low` 的 `users[]` 带 `balance_micro`（据此判断先给谁打电话）、`channel_cooldown` 的 count 与库里冷却 key 数一致；外加"没事不吵"（空库三次调用零投递）与频率闸。整个用例跑在**用完即删的临时库**上——`notify_channels` 与 `balance_low_threshold_micro` 都是全局键，写共享库会和同二进制并行的两个老用例互相覆盖。摘掉 `drift` 载荷里的 `user_ids` 复跑确认变红 |
| **停用 key / 封用户 / 改模型白名单到底"立刻"生效吗（`key_disabled`、`model_not_allowed` 零断言）** | 这俩是出事时的急救手段，而鉴权结果**带缓存**（`auth:key:<hash>`，60s TTL + `auth:ver` 版本号）。库里改了值不等于在跑的网关认——中间隔着 `auth_del`（按 hash 精确失效）与 `auth_flush`（版本号跳变）两套机制，一处漏调就是"页面显示已停用，key 还能再打满一分钟" | **已补** `gateway_key_admission.rs`（2 例）。每段都**先打一发成功请求把缓存焐热再改状态**，否则缓存 miss 回源 PG，测了个寂寞。覆盖：手动停用 → 401 `key_disabled` 且改回 1 立刻复活；`expires_at` 设到过去 → 401 且库里 `status` 仍是 1（3=expired 是派生态，不写回）；管理端封禁属主 → 名下两把 key 一起 401，解封**不连带复活令牌**（定案语义，防误解封放出一批本该单独确认的令牌，一并钉住）；白名单外的模型 403 `model_not_allowed` 而不存在的模型仍 404 `model_not_found`（两者混同的话用户会拿着能用的模型名去查为什么站点说没有）；空数组 `[]` 在入口归一成 null=不限（`docs/database.md` 的既定语义——前端清空勾选就发 `[]`，按字面存下去等于把 key 变砖）。摘掉 `patch_api_key` 里的 `auth_del` 复跑，两个用例同时变红 |
| **surge 加价：网关这半段一个集成用例都没有** | `settings.surge_inflight_threshold` 是唯一一个"读不到就静默不生效"的**计价**开关。`okapi-pricing` 有规则单测、`gateway_multipod` 有在途量单测，可中间那段——读设置、取**集群**在途量、把 `ctx.surge_active` 送进报价——没人接。三个环节任一断掉，账单只是"没加价"：无报错、无日志，月底对账才发现高峰期白跑 | **已补** `gateway_pricing_rules::surge_rule_reads_cluster_inflight_and_marks_up_the_bill`，从真实请求的**账单金额**倒着验：没配阈值 240、阈值 0（关闭，不是"零并发就算高峰"）240、阈值 5 且**另一个实例**报着 5 个在途 → 360 且规则进快照、邻居空下来立刻回 240。负载特意从别的 node 报进来而不是本进程并发造压：一来在途量本就是集群口径（§11.23 的定案就是"阈值语义与副本数无关"），二来本进程自报有 1 秒节流、请求一结束就归零，串行请求追不上自己 |
| **`site_url` 零覆盖，而它是反代场景下找回密码的唯一补救口** | `password_reset_flow` 只验了"缺省按请求 Host 推导"那一支。站点挂在反代后面时 Host 常是内网名字，重设链接照它拼出来用户根本打不开，`site_url` 就是那根救命稻草 | **已补**进同一个用例：配了 `site_url` 就压过 Host，且尾斜杠要吃掉（否则 `https://x//reset-password`）；配成空白串不算配置（管理员清空输入框会发空串），回落 Host 推导 |
| **2FA 只喂过正确的码** | `register_login_key_totp_full_flow` 验了 enroll / confirm / 无码 401 `totp_required` / 带对码 200——**唯独没验过给错码会怎样**。"校验恒真"这种把 2FA 变成纯装饰的实现，能让这个用例全绿 | **已补**三种错法：纯错码、位数不对、**一小时前那个时间窗的码**（最要紧的一条——容忍窗开宽了等于把 30 秒有效期拉成几分钟），三种都得 401 `totp_invalid`；外加"密码错 + 码对"仍报 `invalid_credentials`，不因为码对了就多漏一层信息 |
| 收紧三处"只看状态码"的断言 | `console_teams` 的月度限额只断言 429——而 429 还有 `rate_limited`（分组窗、无效 key 反扫）好几个来源；`gateway_retry_policy` 的首字超时只断言"不是 200"——候选耗尽、余额不足、渠道被摘都不是 200；`console_subscriptions` 的套餐拒删只断言 409——而 409 还有 `group_in_use` / `role_in_use` | 三处都改成断到 `error_code`（`member_limit_exceeded` / `upstream_timeout` / `plan_in_use`）。套餐那处再补一段反面：没人引用的套餐删得掉、再删 404——否则"拒删"可能只是这个端点根本删不动 |

剩下 14 个零字面断言的 error_code 复核后判定不补，理由分三类：`internal_error` / `not_found` /
`record_not_found` 是通用兜底（用例断的是状态码，断字面反而把"哪条路径 404"钉死成实现细节）；
`chsink` / `okapi_session` 是抽取正则的噪声（一个是组件名一个是 cookie 名，压根不是 error_code）；
其余九个（`email_taken` / `redemption_invalid` / `refund_not_committed` / `payment_*` / `oauth_*` /
`smtp_send_failed` / `model_in_fallback_chain`）所在的**行为分支都已有用例覆盖**，只是断在状态码上，
且都不像上面那三处那样存在"被别的同码错误顶替"的混淆风险。

两条**结构上就做不了 e2e** 的，把 §2 里含糊的备注改写成确切原因：

- `channel_balance.rs` 的 DeepSeek / SiliconFlow / OpenRouter / Moonshot 四个官方探针按 **`api_base` 的主机名**选（`api.deepseek.com` 等硬匹配），mock 只能起在 `127.0.0.1` 上，永远落进 `OpenAiDashboard` 分支。除非给探针选择开一个测试后门，否则这四家只能停在响应形状单测——不是疏忽。
- `bedrock.rs` 的 `list_foundation_models` 把控制面主机固定成 `https://bedrock.{region}.amazonaws.com`（有意为之，VPC 端点用户也走公网控制面），mock 接不上，所以 SigV4 形态的"验凭证"与"拉模型"两条分支只有 `aws_sigv4` 单测和真实凭证能验；本轮补的 bedrock 用例覆盖的是 Bearer 形态与 InvokeModel 那条。

另：开始前发现 8080 / 8081 上又挂着一个跑了 **23 小时**的 `okapi all`（前几轮验证留下的）。这轮 L1–L3 走隔离环境不受影响，L4 / L5 要用这两个端口，停掉后跑的。第 1 节那条注意事项到此已是第三次被同一个东西验证，跑全量前请务必先看一眼。

### 2026-09-08 第二十轮：按前端路由表把 e2e 补全并实测

对照 `frontend/src/routeTree.gen.ts` 的全部路由与第 2.5 节写操作面，此前 L3 的 `playwright.interactions.config.ts` 漏了四个有独立页面 / 抽屉的功能面（邀请返利、令牌管理、导入定价 / 在线同步、订阅 OAuth 登录卡）——它们只有截图套件或后端集成，没有"前端真的把约定形状送出去"的断言。

| 层 | 结果 | 说明 |
| --- | --- | --- |
| L3 既有 spec（补测前） | **84 / 84**，56.7s 量级（本轮复跑 57.3s） | `interactions` / `charts` / `catalog` / `request-examples` / `redemptions` / `subscriptions` / `guide` / `write-forms` / `playground` |
| L3 新增 `missing-surfaces.spec` | **4 / 4** | 见下；并入 `playwright.interactions.config.ts` 后再跑全量 **88 / 88**（56.7s） |
| L4 `smoke.spec.ts` | **10 / 10**，25.4s | 打真实 console（`target/debug/okapi` + `frontend/dist`）；演示超管在位，管理端大例未跳过 |

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| `/portal/aff` | 此前只有 `screenshots.spec` 走过页面 | 钉住 `?aff=` 链接、邀请码、人数 3、累计返利 micro→USD、`/api/me/aff` 500 出错误态 + 重试、不伪装零 |
| `/admin/keys` | smoke 只对普通用户走 403 | 检索 `q` / `user_id` 进 URL；停用 `PATCH {status:2}`；删除经 `alertdialog` 打 `DELETE` |
| `/admin/pricing` 导入抽屉 | 粘贴 JSON 与在线同步（`RatioSyncPanel`）零 e2e | 非法 JSON → `alert`「JSON 格式错误」且零请求；合法 JSON 整段 POST `import-newapi`；空源禁用拉取；fetch 体是去空白的源列表；默认不选、点 `1.5` 才 apply `{model,axis,value:"1.500000"}` |
| 渠道抽屉订阅 OAuth 卡 | 后端 `gateway_oauth_channels` 有流程，前端卡没有 | `start` 只带 `provider`；缺名 / 模型禁用换码并提示；新建发 `name`+`models`，编辑追加发 `channel_id` |
| 其余路由 | 登录 / 门户 / 管理端写表单 / 套餐 / 团队 / 安全 / 广场 均已有 L3 或 L4 | 不重复造 |

未纳入本轮默认路径的：`screenshots.spec.ts`（视觉回归，不在 interactions 配置里）；Playground 助手正文仍是纯文本（第 2.5 节既有备注）；第 3 节第 11 条仍是产品决策；第 3 节第 13 条的列表级写操作与少量 HTTP 直打（第二十一轮已把前端写操作补上）。结构上做不了前端 e2e 的两处（四家官方余额探针按主机名选、Bedrock 控制面主机写死）见第十九轮。

另：隔离环境 `/tmp/okapi-r19` 上 `OKAPI_VERIFY_IMAGE=1 bash scripts/verify-deploy.sh` 再过一遍（exit 0）：embed-web 四断言 + 发布镜像五断言（229MB、65534、空库首启 + Setup）。同环境 `cargo test --workspace` 的唯一失败是 `okapi-store` doctest 缺 sqlx 离线缓存（`mutate.rs` `delete_role`），属隔离树快照过期，当前工作树已有对应 `.sqlx`，不按产品缺陷记。

### 2026-09-08 第二十一轮：列表级写操作 e2e（第 3 节第 13 条前端）

对照第 2.5 节与第 3 节第 13 条，补 `list-writes.spec.ts`（6 例）并入 `playwright.interactions.config.ts`。桩接口、断言请求体形状；不碰数据库。

| 层 | 结果 | 说明 |
| --- | --- | --- |
| L3 `list-writes.spec` | **7 / 7**（初记 6，续 + 通知多路） | 见下 |
| L3 全量 interactions | **94 / 94**，1.1 min（8 workers） | 第二十轮 88 + 本轮 6；并行下渠道批量勾选曾被列表 invalidate 冲掉，已改成等选择条消失再勾 |

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 渠道列表 | 批量 / 复制 / 测活 / 单删此前只有抽屉与余额 | 复制 `{name:-copy}`；行测活 `{model}`；测全部空体且只打启用；批量 `enable/disable/delete`；单删手输名称 `DELETE /admin/channels/{id}` |
| 运维 DLQ | 只截过图 | 全选只含待处理；重投 / 丢弃经 `alertdialog`，体 `{ids}` |
| 充值兑换卡 | 只有下单 e2e | trim 后 POST `/api/me/redeem`；成功态套餐/分组/有效期；`redemption_invalid` 文案 |
| 运维退款 / 对账 / 保留 | 熔断已有 | 先查后退 `reason` trim；单用户 `{user_id}` / 全部 `{all,limit}`；缩短保留期二次确认 |
| 设置 | SMTP / 高级搜索已有 | 公告 `site_notice` 带 `updated_at`；注册 `0.29`→290000 micro；隐私即时 POST；MCP 写入抽屉 bool |
| 门户密钥 | smoke 只覆盖删除确认 | 新建 `/auth/keys` 带 `group_code` + `ip_allowlist`；停用 PATCH `status`；删除手输名称 |

未纳入：`screenshots.spec.ts`；第 3 节第 11 条产品决策。通知渠道卡、后端 `GET /v1/models` 与 `GET /admin/pools/{code}` 见本轮续记。

#### 续：通知多路 + 两条 HTTP 直打

| 项 | 结果 |
| --- | --- |
| L3 `list-writes` 通知多路 +1 | **7 / 7**。Webhook URL / 间隔 / 取消勾选 `margin_breaker`；邮件 `to` + `lang: zh-CN`；POST `notify_channels` 两路分行 |
| `GET /admin/pools/{code}` | `console_manage::admin_list_surface`：default 池含本用例渠道与模型并集；不存在 404 `not_found`；普通用户 403 |
| `GET /v1/models` | `gateway_compat::list_models_is_openai_shaped_and_skips_disabled`：`object=list`、条目 `owned_by=okapi`、停用后消失；**无 Bearer 也 200**（探测用，不是疏忽漏鉴权——有 key 同样 200） |

#### 再续：L4 冒烟 + 单条吊销 HTTP

| 项 | 结果 |
| --- | --- |
| L4 `smoke.spec.ts` | **10 / 10**，25.7s。真实 console（`target/debug/okapi`）；演示超管在位，管理端大例未跳过 |
| `DELETE /admin/channels/{id}` | `console_manage::channel_batch_and_user_actions`：软删盖 `deleted_at`、再删 404。与 batch 不同：单条**不**把 `status` 改成 2、也**不**停 key（调度仍按 `deleted_at IS NULL` 过滤） |
| `DELETE /admin/keys/{id}` / `DELETE /api/me/keys/{id}` | 同用例：门户只吊销自己的、管理面按 id、重复 404、普通用户打管理面 403 |

#### 三续：最近登录 / 健康时间线 / 审计过滤

此前只有截图或 smoke 深链、没有「前端真把约定形状送出去」的断言。补进 `missing-surfaces.spec`（+3），L3 全量 **98 / 98**，58.7s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 最近登录卡 | 安全页只有会话吊销 e2e | 预览 8 行、失败原因、仅失败筛选、展开 / 收起；500 走空态文案，不画成零条成功 |
| 渠道健康时间线 | 只有 `screenshots.spec` | 点近 24h 开抽屉；空流量提示；切 6h / 7 天改 `hours`；日志链带 `errors_only`，分析链 `days` 随窗 |
| `/admin/audit` | smoke 只打开过带 query 的 URL | 动作 / 对象 / 操作者进 URL；行展开多出的 detail 键；空条件提示；加载更多带 `before` 游标 |

#### 四续：管理日志过滤 / 路由诊断 / 注册入口

| 项 | 结果 |
| --- | --- |
| L3 全量 interactions | **101 / 101**，59.0s（8 workers）。渠道列表标题改为钉 `#main-content`（顶栏与页头各一个「渠道」） |
| 管理端日志 | 模型 / 用户 / 渠道 / request_id trim、只看失败进 URL 且列表与 `/admin/logs/stat` 同条件；切 7 天写 `hours=168`；展开露出 request_id；空表禁用导出 |
| 路由诊断 | 缺模型禁用；`model` trim + `group` / `pool`；结论文案、渠道已停用、经降级池、key 冷却 |
| 注册 / OAuth | 关闭不摆表单；邀请制无码禁提交；`?aff=` 只提示不手填；验证码 `{email, lang: zh-CN}`；OAuth 按钮只跳 `/auth/oauth/{code}` |

#### 五续：门户日志 / 总览待办 / 消耗排行 / 行内退款 / 登录 TOTP

此前门户日志只有 smoke 空表 + 禁导出；总览待办、消耗排行、日志行内退款、邮箱登录二次验证都没有「前端把约定形状送出去」的断言。补进 `missing-surfaces.spec`（+5），L3 全量 **106 / 106**，58.7s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 门户用量日志 | smoke 只验空表 | `scope` / 模型 trim / `errors_only` 进 `/api/me/logs`；展开账单快照（原价 / 实扣 / 规则链）；加载更多带 `before`；CSV 文件名 `okapi-usage-`；空表禁用导出 |
| 总览「需要注意」 | smoke 打开过落地页 | 芯片 PG / Redis / CH / NATS；Redis 挂了、死信 / 未定价 / 空池 / 高错误率 / 对账漂移深链；切近 30 天重拉 `channels?days=`；全清文案 |
| 用户消耗排行 | 图表套件只验共享交互 | `$1.23` 来自 `1_230_000` micro；用户链 `/admin/logs?user_id=&hours=天数×24`；切窗改 `days` |
| 日志行内退款 | 运维页退款卡已覆盖，行内没有 | 失败行不出现退款；成功扣费行确认后 POST `{request_id, reason}`（reason trim） |
| 邮箱登录 TOTP | 绑定流程已有，登录重试没有 | 首次 `{email, password}`；`totp_required` 后露出 `#totp`；第二次才带 `totp_code` |

#### 六续：账户流水 / 首启向导 / 服务质量卡 / 站点公告 / API Key 登录

smoke 只验流水空签与登录能进门；质量页图表套件只验共享交互。补进 `missing-surfaces.spec`（+5），L3 全量 **111 / 111**，54.5s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 账户流水 | smoke 两签空态 | 进账 `+$1.23` 来自 micro；退款标签 + `/portal/logs` 深链；订阅池徽章；`limit=50` / `before` 翻页；订单原币 `88.00 CNY` 不经浮点；空表与 500 不伪装成零 |
| 首启向导 | 只有 `smoke-all.sh` 后端 | `needs_setup` 才出向导、无登录分段；空用户名禁提交；POST `{username}` trim；Key 展示一次后进 `/admin` |
| 服务质量卡 | `charts.spec` 只验趋势图交互 | 渠道 / 模型 / 错误 / 客户端 `days`+`limit`；高错误率渠道链带 `errors_only`；模型 / 错误码 `hours=天数×24`；空 `client_type` 显示未识别；切 30 天重拉 |
| 站点公告横幅 | 设置页发布已覆盖，落地横幅没有 | warning 按 `updated_at` 关掉、同版刷新不再出、换版再出；critical 无关闭钮 |
| API Key 登录 | smoke 用整串 key | 空白禁提交；`'  sk-…  '` 探活 `/api/me` 的 Bearer 已 trim |

#### 七续：门户总览查询 / 站点规模 / 用户用量 / 登出 / OAuth 着陆

图表套件已验门户切签零请求与日期范围，但没钉 `scope`/`days`、本分钟 RPM、订阅剩余。规模条、用户用量签、登出体、OAuth 兑 key 也缺形状断言。补进 `missing-surfaces.spec`（+5），L3 全量 **116 / 116**，58.7s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 门户总览查询 | `charts.spec` 不验 scope / 90 天 | 缺省 `scope=key&days=7`；全账户改 `scope=user`；近 90 天改 `days=90`；key 视角 `12 / 60` RPM，全账户改平均 TPM；订阅剩余链 `/portal/plans`；余额到期清零 |
| 站点规模条 | smoke 只看得到标题 | 自动停用 / 未定价文案；深链用户 / 密钥 / 渠道 / 定价 |
| 用户用量抽屉 | write-forms 把 usage 桩成空 | `usage?days=7`；近 7 天 `$1.23`；流水类型 / 补偿标签 / 操作者；日志链 `user_id` + `hours=168` |
| 登出 | smoke 点过按钮 | POST `/auth/logout` 体 `{}` 后回登录页 |
| OAuth 着陆 | 只覆盖登录页跳 `/auth/oauth/{code}` | `?oauth=done` 兑 key `{name:oauth}` 后进门户 |

#### 八续：行内用量 / 未定价筛选 / KPI 实时条 / 语言主题 / 排行空态 / 退款幂等

列表页用量格子、定价「仅看未定价」、总览 KPI / 实时查询串、顶栏语言与主题、排行失败态、行内退款幂等此前没有形状断言。补进 `missing-surfaces.spec`（+6），L3 全量 **122 / 122**，58.2s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用户 / 密钥行内用量 | keys 页把 entity-usage 桩成 `{}`；用户页只验抽屉 | `kind` + `ids` + `days=7`；今日 / 7 天 micro→USD；深链 `user_id` / `api_key_id`；501 显示 —，`$1.23` 消失 |
| 仅看未定价 | write-forms 只覆盖抽屉提交 | 按钮进 URL `unpriced=true` 且查询带上；与搜索 `q=gpt-5` 并存；再点一次从查询串拿掉 |
| 总览 KPI / 实时条 | smoke 只看到 QPS 标签 | `overview?days=7` 切 30 天重拉；`realtime?window=60`；QPS 由 `qps_milli` 换算 |
| 语言 / 主题菜单 | 深色只靠 initScript | English 后页头 Dashboard、写 `okapi.lang`；深色挂 `html.dark` 写 `okapi.theme`；跟随系统清除 |
| 消耗排行空态 / 500 | 只有 happy path | 空表「暂无数据」不见 `$0`；500 走错误码文案 |
| 行内退款幂等 | 只覆盖 `refunded` | `already_refunded` 显示幂等保护文案，不出现「已退款 $…」 |

#### 九续：用量分析过滤条 / 运维退款三结局 / 经营分组表 / 模型删除 / 注册赠送

`charts.spec` 覆盖高级筛选，但过滤条芯片与深链 `user_id` 没钉；运维退款只有成功路径；经营页分组表与资金流入四桶、模型删除手输名称、注册赠送金额都缺形状断言。补进 `missing-surfaces.spec`（+4，注册例加赠送金额），L3 全量 **126 / 126**，54.8s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用量分析过滤条 | 高级筛选已覆盖，过滤条没有 | 深链 `user_id=7` 进 `trend?user_id=7&days=7`；芯片回填 alice；加 `model=gpt-5`；点 × 拿掉 `user_id` |
| 运维退款三结局 | list-writes 只覆盖可退成功 | 404 `record_not_found`；未扣费禁退并给提示；`already_refunded` toast 且预览翻成已退款 |
| 经营资金流入 / 分组 | smoke 只看到「资金流入」标题 | 四桶 micro→USD（含扣减 / 过期，>0 才出）；分组 `groups?days=`；切 30 天两接口都重拉 |
| 模型删除 | write-forms 只覆盖抽屉与发布 | 确认框手输 `gpt-5` 后 `DELETE /admin/models/gpt-5`；`requires_publish` 提示需发布 epoch |
| 注册赠送 | 已覆盖邀请制 / 验证码，没钉金额 | 无邀请码 `$1.00`；填 aff / `?aff=` 后叠加成 `$1.50` |

#### 十续：用量拆分聚焦 / 渠道 key 状态 / 质量趋势 / KPI 毛利窗

`charts.spec` 覆盖趋势图交互，但拆分表的 `by`/`limit`、日志链与聚焦下钻没钉；渠道列表近 24h 只有点开抽屉；质量页默认趋势签的 `metric`/`stack` 没进查询串；KPI 切窗只验了 overview。补进 `missing-surfaces.spec`（+3，KPI 例加 `margin?days=`），L3 全量 **129 / 129**，1.0m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用量分析拆分 | 过滤条已覆盖，拆分表没有 | 缺省 `by=model&limit=50`；日志链 `hours=168`；聚焦把行变成 `model=` 且下一层 `by=channel` |
| 渠道 key 状态 / 近 24h | 时间线抽屉已覆盖，列表列没有 | `days=1&limit=100`；`1/2 可用` + 冷却 + 约 N 分钟后恢复；`pools: []` → 未入池；错误率 25.0% / 80 次 |
| 服务质量趋势 | 质量卡覆盖 channels/models/errors/clients，默认趋势签没有 | 缺省 `metric=error_rate&days=7`；切平均时延 + 对比维度 model；换 30 天因 `key={days}` 重挂，metric 回到 `error_rate` |
| 总览 KPI 毛利 | 已覆盖 overview / realtime | 切窗同步 `margin?days=` 7 → 30 |

#### 十一续：拆分名次环比 / 渠道空闲与 OAuth 过期 / 团队详情用量

十续钉了拆分下钻与渠道部分可用，但名次/环比、切「按」、无流量行、OAuth 过期灰字、团队钱包金额都还没形状断言。补进既有拆分 / 渠道例（+1 团队），L3 全量 **130 / 130**，1.0m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 拆分名次 / 环比 / 按 | 十续只钉 by/limit/聚焦 | `previous_rank` 升 1 显示 ▲1；`delta_bp` 2300 → +23%；切「按」用户仍带 `model=gpt-5` |
| 渠道空闲 / OAuth / 未测 | 十续只钉部分可用与错误率 | 过期 token 灰字；`last_test` 空 → 未测过；无 24h 流量行显示 — |
| 团队详情用量 | write-forms 只覆盖建团写路径 | `/api/teams/{id}/usage` 钱包 $12.50；alice 本月 $0.25 / 累计 $3.00；空上限「不限」 |

#### 十二续：日志 CSV 内容 / 测活徽章 / 用量 KPI 环比

文件名已覆盖，但 CSV 正文（六位 USD、公式注入、失败 status、key 列）没读；测活列只有「未测过」；用量页 KPI 环比没钉。补进 `missing-surfaces.spec`（+1 CSV，过滤条 / 管理日志 / 渠道例加断言），L3 全量 **131 / 131**，58.8s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 门户 / 管理日志 CSV | 只断言文件名前缀 | UTF-8 BOM；金额 `toFixed(6)`；模型 `=1+2` 前加 `'`；失败行 `upstream_error`；本密钥无 key 列，全账户才有 |
| 渠道测活 / 无 key | 十一续只钉未测过 | 成功 `120 ms`；失败 `HTTP 429`；`keys: []` → 没有 key |
| 用量 KPI 环比 | 过滤条只钉金额 | 请求 ▲ +25%、消费 ▲ +23%、tokens 持平、对比上一个 7 天、输入/输出 mix |

#### 十三续：拆分/经营毛利列 / KPI 万元紧凑 / 用户列表搜索 / 渠道停用

成本覆盖列和 KPI 紧凑记法此前没有形状断言；用户列表搜索只在文档里写了「搜索回车」。补进 `missing-surfaces.spec`（+2），L3 全量 **133 / 133**，1.0m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 拆分 / 经营毛利 | 有成本才出列 | 拆分 `cost_known_requests>0` 出「已采集部分毛利」与覆盖率 80%；经营徽章同口径 |
| 用量 KPI 紧凑 | 小额已覆盖 | ≥$10k 显示 `7.2万`；副行已采集毛利 |
| 用户列表搜索 | 行内用量已覆盖，搜索没有 | `q` trim 进 URL；空结果；停用态；倍率 `×1.250000` 原样 |
| 渠道停用 / 全可用 | 十二续只钉测活 | `status=2` → 停用；idle 行「1 把 key 全部可用」 |

#### 十四续：渠道列表搜索 / 令牌限制徽章 / 拆分名次跌落 / 质量 ttft 与吞吐 / 含让利

渠道页搜索与协议筛选、令牌限模型/IP 与日志链、拆分「新」与下跌、质量趋势另外两个 metric、KPI 含让利都还没钉。补进 `missing-surfaces.spec`（+1 渠道搜索，其余加在既有例上），L3 全量 **134 / 134**，54.4s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 渠道列表搜索 | list-writes 只覆盖写操作 | `q` trim、协议 `provider` 进 URL 与查询；交叉过滤空结果；清空筛选 |
| 令牌限制 / 日志链 | 只钉 URL 与停用删除 | 查询串带 `q`+`user_id`；限 1 个模型 / IP；累计 $0.50；日志链 `api_key_id=9` |
| 拆分名次跌落 | 只钉 ▲1 / +23% | 上期不在榜「新」；跌 2 位 ▼2；环比 -23% |
| 质量趋势其余 metric | 十续只钉 latency | `ttft`、`throughput` 进查询 |
| KPI 含让利 | 无成本时折扣没钉 | `discount_micro` → 「含让利 $0.20」 |

#### 十五续：兑换码停用整批 / 渠道池列表徽章 / 供应商控制台 / 负毛利与名次持平

兑换批次停用只有后端集成、前端按钮未走交互；池列表只验删除禁用；渠道搜索没钉控制台链与列表自带余额；拆分没钉名次不变与负毛利红字。补进 `missing-surfaces.spec`（+2，渠道搜索 / 拆分例加断言），L3 全量 **136 / 136**，54.3s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 兑换码停用整批 | 生成抽屉与分页已覆盖，停用没有 | 未使用可点；已核销禁点；`DELETE /admin/redemptions/{batch}`；toast「已停用 3 张未核销码」 |
| 渠道池列表徽章 | write-forms 只覆盖抽屉与删禁用 | 内置；空池；优先级 + 加权 / 最低时延；`2 个分组 / 1 个令牌 · 1 个池的降级目标` |
| 供应商控制台 / 列表余额 | 搜索只钉 q/provider | openai / anthropic 固定站；compat 取 origin；非法 api_base 不显示；列表 CNY `¥110.50` 与 USD `$0.00` |
| 拆分负毛利 / 名次持平 | 只钉 ▲/▼/新 | `previous_rank === rank` → —；`known_margin_micro < 0` 红字 `-$0.10` |

#### 十六续：套餐/角色/规则列表列 / 令牌空检索 / 门户密钥钉住 / 分组空池

抽屉写路径早已覆盖，但管理端套餐/角色/规则列表列、令牌空检索、门户列表档位钉住、分组「可自选」与空池都还没形状断言。补进 `missing-surfaces.spec`（+3）并加在既有令牌 / 门户密钥 / 分组例上，L3 全量 **139 / 139**，1.0m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 管理端套餐列表 | 抽屉已覆盖形态互斥 | 充值模板 `$10` / 90 天；订阅 `$5 / 每日`、不售卖、人数 12 |
| 角色列表截断 | 抽屉已覆盖权限勾选 | 前 4 个权限点；第 5 个收进 `+1 项` |
| 计费规则列表人话化 | 抽屉已覆盖字段回填 | 阶梯 `月用量 ≥ 100 tokens + 月消费 ≥ $1 时 ×0.9`；时段 `周一五 每天 00:00–05:59 ×0.5`；独占 / 最优；范围拼接；折扣 `×0.8` / 全部 |
| 令牌空检索 / 到期 | 有命中检索与徽章 | `q` 无命中 → 暂无数据；到期 `2099-06-15` |
| 门户密钥钉住 | 只覆盖新建写路径 | 档位：vip；限 1 个来源；用量 `$1.23`；RPM 60 |
| 分组可自选 / 空池 | 只钉限流列 | 列表「可自选」；`channel_count=0` → 空池 |

#### 十七续：门户密钥重命名 / 定价列表列 / 质量粒度 / 团队列表

新建/停用/删除已覆盖，但门户密钥改名 PATCH、定价列表倍率与渠道列、质量页高级筛选粒度、团队列表角色/余额都还没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，58.5s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 门户密钥重命名 | 只覆盖新建 / 停用 / 删除 | 空名禁保存；trim；跟随分组 `group_code: null`；清 IP 后新名单；toast 已保存 |
| 定价列表列 | 只钉未定价过滤 | `ratio` / `1.25` / `$2.50 / $20.00` / `2 ×1.5`；已定价无渠道；未定价 `1 条渠道` 深链 |
| 质量趋势粒度 | charts.spec 只在统计页 | 高级筛选 `granularity=hour` 进 `/admin/stats/trend` |
| 团队列表 | 只钉详情抽屉 | 所有者徽章；列表钱包 `$12.50` |

#### 十八续：令牌启用 / 用户动作 / 定价空检索 / 质量重置 / 总览趋势卡

令牌例标题写了启用却只 PATCH 停用；用户抽屉封禁后没点解封 / 升降级 / 软删；定价空检索、质量筛选重置、总览趋势切消费都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，58.5s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 令牌启用 | 标题写了启用，体只停用 | 停用后列表翻成启用；PATCH `{status:1}` |
| 用户动作 | 只钉封禁确认框 | 解封无确认；promote / demote 直接 POST；软删除经确认框 `{action:delete}` |
| 定价空检索 | 有命中 q | `q=nope` → 没有匹配的结果；清空筛选 |
| 质量筛选重置 | 十七续只钉 hour | 「重置高级条件」去掉 `granularity` |
| 总览趋势卡 | KPI 切窗已覆盖 | 切「实际消费」`aria-pressed` |

#### 十九续：门户密钥 401 / 启用、邀请链接复制、质量日期校验

管理端令牌启用已钉，门户密钥仍只停用不翻启用；新建 401 关抽屉的会话文案、邀请链接剪贴板、质量只填开始日期的区间拦截都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，50.9s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 门户密钥 401 | 只钉成功新建 | POST `/auth/keys` 401 → 抽屉关掉，提示需邮箱密码登录 |
| 门户密钥启用 | 只钉停用 PATCH | stub 翻 `status`；停用后点启用 PATCH `{status:1}` |
| 邀请链接复制 | 只验按钮可见 | `clipboard.writeText` 含 `/?aff=`；toast 已复制 |
| 质量日期校验 | 十八续只钉重置 | 只填开始日期 → `role=alert` 无效区间，不发查询 |

#### 二十续：趋势卡有数据、日志 CSV 公式前缀、明文/订单号复制

总览趋势卡此前只切 Segmented（空 `data` 不挂 TimeChart）；管理日志 CSV 没钉公式注入；流水订单号、门户明文 Key、首启 Root Key 的复制都没读剪贴板。加在既有例上（无新增用例），L3 全量 **139 / 139**，59.0s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 总览趋势卡 | 十八续空序列只切指标 | margin 有日点后数据表 `42`；切「实际消费」`US$1.23` |
| 管理日志 CSV | 只钉表头与六位 USD | `error_code` 为 `=1+1` → CSV `'=1+1`；展开行复制 `request_id` |
| 流水订单号 | 只验 `ord-paid` 可见 | 行内复制写入剪贴板 |
| 明文 Key | 门户/首启只验可见 | 复制 `sk-okapi-new-plain` / `sk-okapi-root-once` |

#### 二十一续：otpauth / 团 key / 门户日志复制，质量空态与超 31 天按小时

明文复制已覆盖门户/首启/流水，TOTP otpauth、团 key、门户日志 `request_id` 还没读剪贴板；质量页空窗、按小时跨过 31 天、trend 500 也没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.0m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| TOTP otpauth | 只验 URL 可见 | 「复制 otpauth 链接」写入剪贴板 |
| 团 key 明文 | 只验可见 | 抽屉内复制 `sk-okapi-team-plaintext-once` |
| 门户日志 request_id | 只验可见 | 展开行复制 `req-50` |
| 质量空态 / 超窗 / 500 | 十九续只钉缺结束日 | 空窗文案；hour + 39 天不发查询；500 → 重试 |

#### 二十二续：待办组件宕机 / 规模条分支、池孤儿、审计 500、角色空态

待办只钉了 Redis 挂与死信，PG/CH 宕、outbox 积压、冷却 key 都已在 stub 里却没断言；规模条 `auto_disabled` 盖住了无 key / 近 7 天 / 全定价分支；池成员从不走到 `orphan: true`；审计无 500；角色空态带新建入口没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.1m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 总览待办 | Redis 挂、死信已钉 | PG/Redis/CH 不可达；outbox 1000；冷却 2 把 key |
| 站点规模条 | 只钉自动停用 + 未定价 | 近 7 天 +5；启用但无可用 key；5 个有启用渠道 |
| 池成员孤儿 | 只钉非空保存 | 全不勾红字警告；POST `pools: []`；toast 对所有人不可达 |
| 审计 500 | 只钉空态与翻页 | 500 → ErrorState + 重试 |
| 角色空态 | 只钉权限截断 | 空表 hint；空态「新建角色」打开抽屉 |

#### 二十三续：测活失败 toast、错误占比、渠道/套餐空态

测全部此前只钉成功汇总；行测活只钉成功 ms；错误分布只钉码与 502；渠道/套餐空态带新建入口没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，58.4s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 渠道测全部失败 | 只钉空体 + 1 可达 | `测试完成：0 可达 / 1 失败（共 1）`（warning / `role=status`） |
| 行测活模型失败 | 只钉 `连通正常（12 ms）` | `scope: model` → `模型 gpt-5 调不通（model_not_found）：unknown model xyz` |
| 错误分布列 | 只钉码 / 502 / 深链 | 占比 `75.0%`；top 渠道 `openai-main`；top 模型 `gpt-5` |
| 渠道空表 | 搜索空结果可清空 | 无筛选空表 hint；空态「新建渠道」打开抽屉 |
| 套餐空表 | 只钉列形态 | 空表 hint；空态「新建套餐」打开抽屉 |

#### 二十四续：通用测活失败、空错误码、池/规则/分组/密钥/团队空态

测活只钉了 `scope=model`；错误码空串、upstream 0、无名渠道没钉；池/规则/分组/门户密钥/团队空态带新建入口没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.0m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 行测活通用失败 | 二十三续只钉 scope=model | 无 scope → `测活失败：upstream_timeout` |
| 空错误码 | 只钉有码行 | `(empty)` 深链 `errors_only`；status `—`；渠道 `#7` |
| 错误空表 | 只钉有数据 | 「窗口内没有失败请求」 |
| 渠道池空表 | 只钉徽章 | 空表 hint；空态「新建池」打开抽屉 |
| 计费规则空表 | 只钉人话化 | 空表 hint；空态「新建规则」打开抽屉 |
| 价格分组空表 | 只钉徽章 | 空表 hint；空态「新建分组」打开抽屉 |
| 门户密钥空表 | 只钉列表列 | 空表 hint；空态「新建密钥」打开抽屉 |
| 团队空表 | 只钉列表列 | 空表 hint |

#### 二十五续：sync 无差异、趋势卡 500、模型/兑换码/门户套餐/熔断/客户端空态

在线同步从不走到 `differences: {}`；总览趋势卡只钉有数据；模型/兑换码/门户套餐/熔断空表、客户端空表、门户总览空窗 hint 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，56.6s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 在线同步无差异 | 只钉有差异 apply | `differences: {}` → syncNoDiff |
| 总览趋势卡 500 | 二十续只钉有日点 | `internal_error` → ErrorState + 重试 |
| 模型空表 | 只钉检索空结果 | 无筛选 hint「还没有模型…」 |
| 兑换码空表 | 只钉停用整批 | 空表 hint；空态「生成」打开抽屉 |
| 门户套餐空表 | 只钉在售/续期 | 「暂无在售套餐」 |
| 毛利熔断空表 | 只钉暂停行 | 启用且无对 → 空表 hint |
| 客户端空表 | 只钉 sdk / 未识别 | `trendEmptyHint` |
| 门户总览空窗 | 只钉 KPI / 切窗 | `emptyUsageHint` |

#### 二十六续：死信/熔断关闭、用户/KPI/总览/日志/时间线/团队 500、目录未知模型

死信从不走到空表；熔断只钉启用态；用户无筛选空表与列表 500、KPI overview 500、门户 breakdown 500、管理日志 500、时间线 500、团 usage 500、流水空表、目录未知 `?model=` 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，53.0s（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 死信空表 | 只钉重投 / 丢弃 | 「没有死信。统计与账本一致。」 |
| 熔断未启用 | 二十五续只钉启用空表 | `enabled: false` → disabled hint |
| 用户空表 / 500 | 只钉检索空结果 | 无筛选「暂无数据」；500 → ErrorState + 重试 |
| KPI overview 500 | 二十五续只钉趋势卡 500 | overview 500 → ErrorState + 重试 |
| 门户总览 500 | 二十五续只钉空窗 | breakdown 500 → ErrorState + 重试 |
| 管理日志 500 | 只钉空表禁导出 | 换过滤后 500 → ErrorState + 重试 |
| 渠道时间线 500 | 只钉空流量 / 深链 | 切 hours 后 ErrorState |
| 团 usage 500 | 只钉成功用量 | 成员表 ErrorState；钱包仍 `$0` |
| 流水空表 | 只钉订单空 + 流水 500 | 余额变动空 hint |
| 目录未知模型 | 只钉已发布深链 | `?model=does-not-exist` → 未发布并可关掉 |

#### 二十七续：管理端/门户列表页 500

二十六续只钉了用户列表与看板 500，渠道 / 池 / 套餐 / 角色 / 规则 / 分组 / 模型 / 兑换码 / 令牌 / 门户密钥 / 门户日志 / 门户套餐列表失败都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.0m（8 workers）。令牌页 URL 仍带 `q=` 时不能 `reload`（`**/admin/keys?*` 会把文档写成 JSON），改成换检索词打缓存。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 渠道 / 池 / 套餐 / 角色 / 规则 / 分组 / 模型 / 兑换码 | 只钉空表 + 新建 | 列表 500 → ErrorState + 重试 |
| 管理端令牌 | 只钉检索 / 启停删 | 换 `q` 后 500 → ErrorState + 重试 |
| 门户密钥 / 日志 / 套餐 | 只钉空表或检索 | 500 → ErrorState + 重试 |

#### 二十八续：质量卡 / 经营 / 死信 / 熔断 / 高级设置 500

质量四卡只钉有数据与空表；经营只钉四桶与切窗；死信 / 熔断 / 高级设置只钉写路径与空态。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.1m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 错误分布 / 客户端 | 只钉空表 | 500 → ErrorState，无重试按钮 |
| 渠道健康 / 模型时延 | 只钉深链 | 500 → destructive 文案（不是 ErrorState） |
| 经营 cashflow | 只钉四桶 | 500 → 资金流入行收起 |
| 经营 margin | 总览趋势卡已钉 500 | 经营页切窗后 ErrorState + 重试 |
| 死信 / 熔断 | 只钉空表 | 500 → ErrorState，无重试 |
| 高级设置列表 | 只钉 MCP 写入 | GET `/admin/settings` 500 → ErrorState + 重试 |

#### 二十九续：抽屉 500、对账空表、实时/规模/分组收起、流向 500

列表页 500 已钉完，抽屉内 overview / usage / 订阅 / 权限清单 / ModelPicker、对账空表、实时条与规模条失败收起、经营分组表收起、流水订单 500、门户「我的订阅」500、分析流向 500 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.2m（8 workers）。并行下 `interactions` 易支付 `pid` 填入偶发未进 POST（第十一轮已知焦点时序），单跑与复跑全量均过。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用户抽屉 overview / usage | 只钉成功用量 | 两个 ErrorState |
| 用户抽屉订阅 | 只钉发放 / 结束 | GET 500 → ErrorState |
| 角色权限清单 | 只钉勾选提交 | `/admin/permissions` 500 |
| 新建渠道 ModelPicker | 只钉手动补模型 | `GET /admin/models` 500 |
| 三方对账 | 只钉有漂移校准 | 零差异文案；500 无重试 |
| 实时条 / 规模条 | 只钉有数据 | 500 收起，不占版面 |
| 经营分组表 | 二十八续只钉 cashflow 收起 | groups 500 收起 |
| 充值订单 500 | 只钉流水 500 | 切签后 ErrorState |
| 门户我的订阅 | 只钉套餐列表 500 | `/api/me/subscription` 500 |
| 分析流向 | 只钉阶段 / 下钻 | 切窗后 ErrorState |

#### 三十续：拆分/趋势空窗、经营空表、抽屉用量空表、团队列表 500

二十九续钉了抽屉与看板 500，拆分 500、趋势空窗、经营空 `data`、质量空表、用量空 daily/ledger、`stats_disabled`、团队列表 500、高级设置无匹配 hint 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.2m（8 workers）。无易支付 flake。L4 `smoke.spec` **10 / 10**，26.3s（:8080 / :8081 空闲）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用量分析趋势 | 拆分例只走 `view=breakdown` | `/admin/stats` 空 `data` → `trendEmptyHint` |
| 用量分析拆分 | 只钉成功下钻 | 切 `by=group` 后 500 → ErrorState，无重试 |
| 经营报表 | 只钉四桶 / 500 | 重试后空 `data` → `trendEmptyHint` |
| 渠道健康 / 模型时延 | 二十八续只钉 500 文案 | 切 30 天后空表无渠道/模型链 |
| 用户用量抽屉 | 二十九续只钉 500 | 空 daily/ledger → 「暂无数据」 |
| 用户抽屉用量签 | 只钉角色/订阅写路径 | `stats_available: false` → `stats_disabled` |
| 团队列表 | 只钉 401 降级 | 500 → ErrorState，无重试 |
| 高级设置筛选 | 只钉 article 计数归零 | 无匹配 hint `settingEmptyHint` |

#### 三十一续：拆分/流向空表、PoolReach 500、诊断/拉模型/导入/OAuth toast

三十续钉了拆分 500 与经营空表，拆分空 `data`、流向空桑基、分组抽屉池详情失败收起、路由诊断 / 拉模型 / 粘贴导入 / OAuth start 的 500 toast、待办 diagnose 失败收起芯片、ModelPicker 空表都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.3m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用量分析拆分 | 三十续只钉 500 | 切 `by=provider` 后空 `data` → `trendEmptyHint` |
| 用量分析流向 | 二十九续只钉 500 | 切 30 天后空 `nodes`/`links` → `trendEmptyHint` |
| 价格分组 PoolReach | 只钉成功摘要 | 池详情 500 收起，不占版面 |
| 路由诊断 | 只钉成功结论 | 再点诊断 500 → toast |
| 拉上游模型 | 只钉成功覆盖 | 再点拉取 500 → toast |
| 导入定价 | 只钉成功 / 无差异 | 粘贴再导入 500 → toast |
| 订阅 OAuth | 只钉 start / exchange | start 500 → toast |
| 总览待办 | 只钉全清 | diagnose 500 收起 PG 芯片 |
| 新建渠道 ModelPicker | 二十九续只钉 500 | 空清单「尚无模型」 |

#### 三十二续：写路径 500 toast、Playground 发送失败、设置空表

三十一续钉了读路径空表 / 收起与诊断、拉模型、粘贴导入、OAuth start 的 toast。写路径再点一次失败（拉取同步、OAuth 换码、凭证轮换、池成员、发布、吊销会话、SMTP 测试、通知保存）以及 Playground 发送 500、高级设置无筛选空表都没钉。加在既有例上（无新增用例）。OAuth 成功换码后 `onSuccess` 会卸掉粘贴区，须再点「打开登录页」才能测 exchange 500；失败后按钮是「重新打开登录页」。L3 全量 **139 / 139**，1.3m（8 workers）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 在线同步拉取 | 三十一续只钉粘贴导入 500 | 无差异后再点拉取 500 → toast |
| 订阅 OAuth 换码 | 三十一续只钉 start 500 | 再打开登录页后 exchange 500；失败后再点「重新打开登录页」500 |
| 凭证轮换 / 池成员 | 只钉成功清空 / 孤儿 toast | 再点轮换 / 保存池成员 500 → toast |
| 发布定价 | 只钉 epoch toast | 再点发布 500 → toast |
| 安全页会话 | 只钉吊销端点 | 吊销 500 → toast |
| SMTP 测试 | 只钉成功发送 | 再点测试 500 → toast |
| 通知多路 | 只钉保存体形状 | 再点保存 500 → toast |
| Playground 发送 | 只钉流式成功 | 发送 500 → 助手气泡内英文 alert |
| 高级设置空表 | 三十续只钉筛选无匹配 | 无筛选 `data: []` → 「暂无数据」+ hint |

#### 三十三续：其余写路径 500

三十二续钉了同步拉取、OAuth 换码、轮换/池/发布/会话/SMTP 测试/通知保存。入账、封禁、渠道 PATCH/新建、同步 apply、TOTP enroll、key PATCH、兑换码生成、SMTP 保存、找回密码的 500 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.3m（8 workers）。隐私 / 公告 / SMTP 单键 GET 失败仍按缺省空表单渲染，不走 ErrorState（产品行为，不改）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用户入账 / 封禁 | 只钉成功 micro 与动作序列 | 入账 500 / 封禁 500 → toast |
| 渠道 PATCH | 只钉 400 受保护键 | 400 后再保存 500 → toast |
| 新建渠道 | 只钉成功 POST 与 ModelPicker 500 | 新建 500 → toast |
| 在线同步 apply | 三十二续只钉拉取 500 | 应用前 500 → toast，再应用成功 |
| TOTP enroll | 只钉 401 降级 | enroll 500 → destructive Alert，再点走 401 |
| 渠道 key PATCH | 只钉权重 / 并发 / 启用 | 再点保存 500 → toast |
| 兑换码生成 | 只钉 400 后成功 | 400 与成功之间 500 → toast |
| SMTP 保存 | 三十二续只钉测试 500 | 有草稿后再保存 500 → toast |
| 找回密码 | 只钉 501 SMTP | 再点发送 500 → `internal_error` |

#### 三十四续：抽屉创建与运维写路径 500

三十三续钉了入账 / 封禁 / 渠道 PATCH / apply / TOTP enroll。系数、分组、发放订阅、套餐 / 角色 / 池 / 建团保存、TOTP confirm、公告再发、死信重投、退款、清缓存、保留、毛利保存、核销、充值下单、重置密码的 500 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.3m（8 workers）。公告再发须等首次发布把「保存并发布」置禁用后再改标题，否则并行下 `onSuccess` 会把草稿清掉。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 用户系数 / 分组 | 三十三续只钉入账 / 封禁 | 改系数 / 再保存分组 500 → toast |
| 发放订阅 | 只钉成功发放 | 发放前 500 → toast，再发放成功 |
| 套餐 / 角色 / 池 / 建团 | 只钉成功保存 | 保存前 500 → toast，再提交成功 |
| TOTP confirm | 三十三续只钉 enroll | 错码后再确认 500 → 字段文案 |
| 站点公告 | 只钉发布体形状 | 发布成功后再改标题 500 → toast |
| 死信重投 | 只钉成功重投 | 再重投 500 → toast |
| 运维退款 / 清缓存 / 保留 | 只钉成功体形状 | 退款前 / 再清缓存 / 再保存保留 500 |
| 毛利熔断保存 | 只钉成功保存 | 改窗后再保存 500 → toast |
| 核销 / 充值下单 / 重置密码 | 只钉 400 / 无地址 / 失效 token | 再提交 500 → `internal_error` |

#### 三十五续：其余写路径 500（保存 / 删除 / 校准 / 解除）

三十四续钉了系数、分组、发放订阅、套餐 / 角色 / 池 / 建团保存、TOTP confirm、公告再发、死信重投、退款、清缓存、保留、毛利保存、核销、充值下单、重置密码。模型 / 分组 / 规则保存、规则再停用、池 / 套餐 / 模型删除、兑换码停用整批、团队加成员、门户新建密钥、注册保存、隐私再切、渠道复制、毛利解除、对账校准、死信丢弃、角色应用、结束订阅、余额有效期、令牌再停用的 500 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.4m（8 workers）。抽屉脚部被上次成功 toast（`role=status`）挡住时须先关掉再点保存。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 模型 / 分组 / 规则保存 | 只钉成功体形状 | 保存前 500 → toast，再提交成功 |
| 规则再停用 | 只钉一次 toggle | 再点停用 500 → toast |
| 池 / 套餐 / 模型删除 | 只钉确认后成功 | 确认后 500 → toast，再删成功 |
| 兑换码停用整批 | 只钉成功张数 | 确认后 500 → toast，再停用成功 |
| 团队加成员 | 只钉成功 upsert | 加入前 500 → toast，再加入成功 |
| 门户新建密钥 | 只钉 401 后成功 | 新建前 500 → toast，再新建成功 |
| 注册保存 / 隐私再切 | 只钉成功体形状 | 保存 / 再切 500 → toast |
| 渠道复制 | 只钉 `-copy` 体 | 再复制 500 → toast |
| 毛利解除 / 对账校准 / 死信丢弃 | 只钉成功体形状 | 解除 / 再校准 / 丢弃前 500 → toast |
| 角色应用 / 结束订阅 / 余额有效期 | 三十四续只钉发放 500 | 应用 / 结束 / 保存前 500 → toast |
| 令牌再停用 | 只钉停用→启用 | 再点停用 500 → toast |

#### 三十六续：列表删除 / 批量 / 测活 / 密钥 500

三十五续钉了保存与大部分删除。渠道行测活 HTTP 500、行停用、批量启用、单删、分组 / 规则 / 角色删除（角色仍落到 409）、团 key、门户停用与删除、管理令牌删除、失效 key 再启用、查上游余额的 500 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.4m（8 workers）。渠道列表会叠多条 `role=alert` toast，关关闭钮须按文案过滤或先清掉全部 toast，不能 `getByRole('alert')` 一把关。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 渠道行测活 | 只钉 ok:false 文案 | HTTP 500 → toast |
| 渠道行停用 / 批量启用 / 单删 | 只钉成功体形状 | 再停用 / 启用前 / 确认后 500 → toast |
| 分组 / 规则删除 | 只钉成功需发布 | 确认后 500 → toast，再删成功 |
| 角色删除 | 只钉 409 `role_in_use` | 500 → toast，再删仍 409 |
| 团 key / 门户停用与删除 / 令牌删除 | 只钉成功体形状 | 发 key / 停用 / 确认后 500 → toast |
| 失效 key 再启用 | 只钉 `status: 1` | 再启用前 500 → toast |
| 查上游余额 | 只钉 400 形状 + 成功 CNY | 400 后再点 500 → toast，再查成功 |

#### 三十七续：门户改名 / 批量删除 / 返利保存 500；复跑 L4

三十六续钉了行测活、行停用、批量启用、单删。门户密钥改名 PATCH、渠道批量删除、高级设置返利比例保存的 500 都没钉。加在既有例上（无新增用例），L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，25.3s。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 门户密钥改名 | 三十六续只钉停用 / 删除 500 | 保存前 500 → toast，再保存成功 |
| 渠道批量删除 | 三十六续只钉批量启用 500 | 确认后 500 → toast，再删成功 |
| 充值返利保存 | 只钉 12.35% → 1235 bp | 保存前 500 → toast，再保存成功 |

#### 三十八续：模型限流 / SSRF / OAuth 保存 500；复跑 L5

三十七续钉了返利比例保存 500。模型 RPM、上游访问策略、第三方登录这三处 `POST /admin/settings` 仍只钉成功体形状。加在既有例上（无新增用例），500 后必须把 POST+GET 路由还原，否则同例后续保存会一直吃 500。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）；脚本 trap 已清监听。不额外点易支付保存（第十一轮焦点时序 flake）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 模型请求限流 | 只钉 `model-a: 2` / `model-b: 0` | 首次保存前 500 → toast，还原路由后再保存成功 |
| 上游访问策略 | 只钉 `allow_http: false` + 扩展字段 | 切「允许 HTTP 上游」后保存前 500 → toast，再保存成功 |
| 第三方登录 | 只钉 github + `client-new` 且保留密钥 | 改客户端 ID 后保存前 500 → toast，再保存成功 |

#### 三十九续：Stripe / 扩展 JSON / MCP 保存 500；复跑 L4

三十八续钉了模型限流、SSRF、OAuth 保存 500。Stripe 抽屉此前没有写路径（列表只验密钥不进 HTML），扩展 JSON 只钉非法 JSON 禁保存后取消，MCP 只钉成功体再验列表 GET 500。加在既有例上（无新增用例）。非法 API 地址禁保存；合法 JSON 保存前 500 后须还原 POST+GET。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，25.2s。不额外点易支付保存。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| Stripe 支付 | 无写路径 e2e | 非法 `api_base` 禁保存；保存前 500 → toast，再保存成功且密钥保留 |
| 扩展 JSON | 只钉非法 JSON 禁保存后取消 | 合法 JSON 保存前 500 → toast，再保存 `retries: 5` |
| MCP 写入 | 只钉 `value: true` + 列表 GET 500 | 保存前 500 → toast，再保存成功，列表 GET 500 仍走 ErrorState |

#### 四十续：密钥 / 数字 / 字符串设置编辑器 500；复跑 L5

三十九续钉了 Stripe / 扩展 JSON / MCP。支付凭证（secret）、Web 会话数上限（number）、站点地址（string）三种通用编辑器此前没有写路径。加在既有「高级配置按用途分组」例上（无新增用例）；保存成功 toast 会挡住下一张抽屉的保存按钮，须先关掉 `role=status`。桩里补上 `web_session_limit` / `site_url` 后列表 12 条、访问与安全 3 条。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）；脚本 trap 已清监听。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 支付凭证 | 只钉 `is_secret` 不进 HTML | 空值禁保存；保存前 500 → toast，再保存成功且新密钥不进列表 |
| Web 会话数上限 | 无写路径 e2e | 非数字禁保存；保存前 500 → toast，再保存 `0` |
| 站点地址 | 无写路径 e2e | 保存前 500 → toast，再保存新 URL |

#### 四十一续：试用台预设 JSON / 自定义 OAuth 服务商；复跑 L4

四十续钉了 secret / number / string 编辑器。试用台预设（catalog JSON 数组）和第三方登录「添加服务商」此前没有写路径。加在既有例上（无新增用例）。自定义标识缺三地址禁保存；与 github 重复禁保存；github 的 `custom` 扩展字段仍保留。桩里补上 `playground_presets` 后列表 13 条。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，25.3s。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 试用台预设 | 试用台只钉公开 GET 导入 | 非法 JSON 禁保存；保存前 500 → toast，再保存两项数组 |
| 添加登录服务商 | 只钉改已有 github 的 client_id | 标识重复 / 自定义缺 URL 禁保存；保存前 500 → toast，再保存 github+custom-idp |

#### 四十二续：移除限流规则 / 移除登录服务商；复跑 L5

四十一续钉了添加自定义服务商和试用台预设 JSON。限流规则与登录服务商的移除按钮此前没有写路径。加在既有例上（无新增用例）：去掉第 2 条 RPM 后只留 `model-a`；去掉 custom-idp 后 github 的 `custom` 仍保留。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 移除限流规则 | 只钉添加 + 重复标识禁保存 | 移除第 2 条后保存前 500 → toast，再保存只剩 `model-a: 2` |
| 移除登录服务商 | 四十一续只钉添加 custom-idp | 移除第 2 家后保存前 500 → toast，再保存只剩 github |

#### 四十三续：清空限流规则 / Discord 预设服务商；复跑 L4

四十二续钉了移除第二条 RPM / 第二家 OAuth。清空全部限流规则、以及 github/discord/linuxdo 预设（不填三地址）此前没有写路径。加在既有例上（无新增用例）。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，26.3s。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 清空限流规则 | 四十二续只钉剩 `model-a` | 再移除第 1 条，保存前 500 → toast，再保存 `{}` |
| Discord 预设 | 四十一续只钉自定义三地址 | 添加 discord 不填 URL；保存前 500 → toast，再保存 github+discord |

#### 四十四续：OAuth 可选字段 / 清空服务商 / 删除通知渠道；复跑 L5

四十三续钉了 Discord 预设。授权范围、清空全部登录服务商、通知多路删掉邮件渠道此前没有写路径。加在既有例上（无新增用例）。通知卡保存成功后 `setRows(null)` 并重拉单键 GET，须把 `/admin/settings/notify_channels` 桩成已保存值，否则删除按钮会随表单卸掉。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| Discord 授权范围 | 四十三续只钉 code/id/secret | 展开可选字段填 `identify`；保存前 500 → toast，再保存带 `scopes` |
| 清空登录服务商 | 四十二续只钉移除第 2 家 | 两家都移除后保存前 500 → toast，再保存 `[]` |
| 通知多路删除 | 只钉两路一起保存 + 再保存 500 | 删邮件后 500 → toast，再保存只剩 webhook |

#### 四十五续：清空通知渠道；复跑 L4

四十四续钉了删掉邮件只留 webhook。最后一路 webhook 删掉后保存空数组此前没有写路径。加在既有例上（无新增用例）。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，27.5s。

| 功能面 | 结论 | 处置 |
| --- | --- | --- |
| 清空通知渠道 | 四十四续只钉剩 webhook | 再删最后一路，空态文案出现；保存前 500 → toast，再保存 `[]` |

#### 四十六续：复跑 L3 / L5，无新缺口

四十五续之后，现有功能面的前端写路径与失败 toast 已钉完；本轮不再加用例。未纳入：易支付额外保存 500（第十一轮焦点时序 flake）；`CopyButton` 剪贴板失败；Playground 助手正文仍是纯文本；`screenshots.spec.ts` 视觉套件不在 interactions 配置里；第 3 节第 11 条订阅凭证合规边界（产品决策）；隐私 / 公告 / SMTP 单键 GET 失败仍按缺省空表单渲染。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）。

#### 四十七续：复跑 L3 / L4，无新缺口

与四十六续相同口径，不加用例。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，26.3s。

#### 四十八续：复跑 L3 / L5，无新缺口

与四十六续相同口径，不加用例。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）。

#### 四十九续：复跑 L3 / L4，无新缺口

与四十六续相同口径，不加用例。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，25.7s。

#### 五十续：复跑 L3 / L5，无新缺口

与四十六续相同口径，不加用例。L3 全量 **139 / 139**，1.3m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）。

#### 五十一续：复跑 L3 / L4，无新缺口

与四十六续相同口径，不加用例。L3 全量 **139 / 139**，1.4m（8 workers）。:8080 / :8081 空闲，L4 `smoke.spec` **10 / 10**，25.0s。

#### 五十二续：复跑 L3 / L5，无新缺口

与四十六续相同口径，不加用例。L3 全量 **139 / 139**，1.3m（8 workers）。:8080 / :8081 空闲，L5 `smoke-all.sh` 前端可达 + 数据面 fail-closed + 单机形态通过（root 已存在跳过 key 断言）。

### 2026-09-16 第二十二轮：换"每个接口的准确性"这条轴——跨出口对账 + 全路由错误壳

前两轮把**可达性**收口了（路由×方法机械对表，零覆盖归零）。这轮换一个判定口径重跑：
不问"有没有用例打到"，问**"打到之后断言了什么"**。两条轴各自抓到东西。

#### 轴一：同一笔账在多个出口是否报同一个数（`billing_surface_parity`，2 例）

既有套件是按出口切的——`console_logs` 验日志、`console_stats` 验统计、`console_portal` 验门户、
`pg_settlement` 验落库，每个都**自己造数据自己断言**。于是"同一笔账在两个出口对不上"这类缺陷
谁都看不见：各自的用例都是绿的。

新用例反过来走：打一笔真请求，把 `billing_records.amount_micro` 当唯一权威，要求其余出口逐个等于它。
钉住五个**互相独立的写侧累加器**（不是同一份数据的不同视图，是五处各写各的）——
`billing_records` / `billing_events` / `users.balance_micro` / Redis 热余额 / `api_keys.used_micro`——
外加四个读出口（`/api/me/logs`、`/api/me/usage`、`/v1/dashboard/billing/usage`、`/admin/logs`）。
第二例验退款后九处同步回冲。

**变异验证**：往 `dashboard.rs` 注入 1 分偏差，两例精确报红（46 vs 45、0.01 vs 0.0），已还原。
新增对账类用例都该这么验一次——用例自己绿不等于它抓得住漂移。

口径注记：`ledger.credit` 只动 Redis（"PG 事件由调用方另记"），种子充值不进 `users.balance_micro`，
故该列断言走**增量**而非绝对值；`/api/me/usage` 与 `/admin/logs` 由 CH 支撑，必须 drain + 轮询，
且 `build_state` 要传 CH URL（传 None 会静默退化成 501 `stats_disabled`，对账就只剩 PG 半边）。

#### 轴二：全路由错误壳（`route_error_envelope`，1 例覆盖 163 条路由×方法）

用例**从源码现抽路由表**（解析 `.route("…", get(…))`），新增端点自动进覆盖，不会随迭代腐化——
这是它存在的理由，不要改成硬编码清单。判定只钉不该退让的那条线：2xx/3xx 放行（按设计公开），
4xx/5xx 必须有机器可读的分类标识，且匿名探测不得打到 5xx。三种方言壳都认
（Okapi `error.code` 字符串 / Anthropic `error.type` / Gemini `error.code` 数字 + `status`）。

**首跑 46 条不合规，根因只有一个**：axum 内置提取器的拒绝绕过 `AppError`，回 `text/plain` 英文句子。
`extract.rs` 早就为 `Query<T>` 写过替身、理由白纸黑字（违反"后端错误只回 error_code"），
但**另外两半从没做**：`Json<T>` 42 条、`Multipart` 3 条。

比 i18n 更要紧的是顺序：**提取器跑在 handler 体之前，也就跑在 `guard()` 鉴权之前**。
于是匿名调用方 POST 一个 `{}` 到任意管理面端点，就能把内部请求结构体的字段名逐个问出来
（实测 `POST /admin/billing/refund` → `missing field \`request_id\``）。

修法照 `Query` 的既有设计：`extract::Json` / `extract::Multipart` 两个替身，拒绝时回
`{"error":{"code":"bad_request","param":"body"|"multipart"}}`，具体字段只进 debug 日志。
54 个提取点机械换过去（返回位的 `axum::Json` 不动）。修完 163 / 163 全绿。

通则：**新加 axum 内置提取器（`Form`、`TypedHeader` 等）前先确认它的 Rejection 走不走 `AppError`**；
不走就照 `extract.rs` 补替身，否则等于在鉴权之前开了一个回英文的洞。

### 2026-09-19 第二十三轮：把"越权"这条轴补完——两类探针都是机械扫全量

第二十二轮的错误壳探针只探**匿名**，它有个盲区：没带 key 一律止步于 `authenticate`，
所以**区分不出 handler 里到底有没有 `guard()`**——漏挂权限闸的端点在匿名探测下同样是 401，
看着很安全。这轮补两类"带身份"的探针。

#### 一：管理面权限闸（`route_error_envelope::every_admin_route_rejects_an_authenticated_but_unprivileged_user`）

用**已登录但无权限**的普通用户（role=1、无 admin 角色）扫全部 `/admin/*`：能过 `authenticate`、
必须倒在 `guard()`。判据是不得 2xx（越权）也不得 5xx（闸没拦住、打进业务逻辑才炸）。

**95 条管理面路由全绿**——这是个阴性结论，但现在被钉住了：既有的
`console_m2::permission_point_matrix` 验的是机制本身（角色→权限点→放行/拒绝），
只在 `/admin/channels` 两个端点上验，哪条新路由忘了挂闸它照样绿。

变异验证：摘掉 `margin.rs` 的一个 `guard`，探针精确点名 `GET /admin/margin-breaker → 200`。

#### 二：门户归属校验（`portal_ownership`，IDOR 面）

权限闸管的是"有没有权限进这个面"；门户是另一回事——**A 和 B 都有权用 `/api/me/keys/{id}`，
问题是 A 能不能拿 B 的 id 去用**。这层权限闸无感，得逐个端点在 SQL 的 `WHERE user_id = $me` 上兜。

既有套件里没有"拿别人的 id"这类用例：`console_portal` / `console_teams` 各自用自己的资源
跑通正向流程，反向没人打。新用例覆盖 `console/mod.rs` 全部五条带 id 的门户路由
（keys 的 PATCH/DELETE、sessions 的 DELETE、teams 的 members/keys/usage），全绿。

用例内建**反向对照**（受害者删自己的 key 必须成功），防"端点整个坏了所以全拒"的假阳；
变异验证：把 `delete_key` 的 `Some(key.user_id)` 改成 `None` 即红。

#### 这轮顺带确认的阴性结论（不要重做）

- 52 个 `*Query*` 结构体字段在用例语料里全部出现过，无"解析了但从没被测过"的参数。
- `/admin/*` 静态扫 `guard` 调用不可靠（正则抽 handler 名会错配），**以运行时探针为准**。

#### 通则

新增"某类端点必须满足某性质"的机械探针时，配一次**变异验证**再合入：
把被测性质在实现侧故意破坏一处，确认探针报红且点名准确。探针自己绿不等于它在探——
本轮三个探针都按此验过（第二十二轮的跨出口对账同）。

### 2026-09-19 第二十四轮：把"每个接口"的准确性做实——计费端点全覆盖 + 翻页恒等式

第二十二/二十三轮的探针是**机械扫全量路由**，但扫的是错误壳与权限闸这两个横切面。
复盘时点破一件事：**机械路由扫 ≠ 端到端准确性**；`billing_surface_parity` 当时只驱动了
`/v1/chat/completions` 一条，而落结算的是七个模块，单笔 chat 对上账不代表另外六个也对得上
（它们各自拼 `SettlementInput`，字段漏填或填错只有自己那条路径看得见）。这轮补两块。

#### 一：每个计费端点都要在全出口对上账（`every_billing_endpoint_agrees_across_surfaces`）

表驱动扫 chat / embeddings / rerank / images / audio.speech 五条，含两种定价形态
（ratio 与 per_call），每条走同一套断言：权威结算行 → 门户日志 → 生态口径累计。
videos（异步任务）与 realtime（WebSocket）形态不同，各自套件已有专项覆盖，不进此表。

**抓到一个真缺陷**：`/v1/images/generations` 与 `/v1/videos` **不回 `x-okapi-request-id`**。
chat / embeddings / audio / custom_pass 都回，errors 路径也回——只有这两个漏了，
而它们都是**计费**端点：用户看到扣款却拿不到 request_id，对不回是哪次调用。
顺带发现 `with_request_id` 被复制了两份（chat 与 embeddings 各一），已收成 `error.rs`
里一份共用的，四处统一引用。

#### 二：列表端点翻页不重不漏（`list_pagination`）

分页机制本身有单测（`page_params_are_clamped`），逐端点集成此前只有
`price_group_pagination_matches_database_pages` 一条；其余只验过"能调用、回了 200"。
而这一类最典型的缺陷恰恰是 200 下的错：`limit` 接了但 `offset` 没进 SQL、
或排序键不唯一导致两页在边界重叠/漏行。

判据用不依赖库内容的恒等式，对并发写入不敏感，也不必把全表拉下来
（`/admin/users` 开发库里上千行）：

```text
A = ?limit=2N&offset=0 ； B = ?limit=N&offset=0 ； C = ?limit=N&offset=N
要求  B ++ C == A（逐 id 等且同序）、B ∩ C == ∅、三次 total 相同
```

覆盖 `/admin/keys`、`/admin/pools`、`/admin/users`、`/api/me/keys` 四条真列表端点；
`/admin/stats/*` 那几个回 `{data,total}` 的是 CH 聚合，分页语义不同，不进此表。

变异验证：把 `PageParams` 的 `offset` 钉死成 0，四条全部报红并列出重叠的行 id。

#### 仍未覆盖的两类（判断：机械做不动，留给按端点的业务用例）

- **参数组合约束**（"A=x 时 B 必须为 y"）：没有统一的声明式来源，机械探针无从知道
  哪些组合非法；逐端点手写等于把业务语义抄第二遍。现状是各套件按自己的语义验。
- **特定输入 → 特定 error_code**（超限回 429 还是 403 还是 400）：判定条件散在各 handler，
  同样没有可机械对照的真值表。第二十三轮的 65 个错误码覆盖验的是"码都能被触发到"，
  不是"该触发哪个码"。两者都建议随新端点在其自己的套件里写，不追求机械全覆盖。

### 2026-09-19 第二十五轮：上一轮说"机械做不动"的两类，其实是框架找错了

第二十四轮末尾把两类缺口判成"机械做不动、留给按端点的业务用例"：
参数组合约束、以及"特定输入该回哪个 error_code"。复盘后推翻——**做不动的是我当时
设想的做法（手写真值表），不是这两条轴本身**。换个判据就都能机械化，而且各抓到一个真缺陷。

#### 一：错误码不写真值表，写**跨端点一致性**（`error_taxonomy_parity`）

硬编码"输入 X 应回码 Y"等于把业务语义在用例里抄第二遍，抄错了还会变成"实现和用例
都错但互相印证"。改成：**拿覆盖最全的 `/v1/chat/completions` 当参照系，运行时把真值表
导出来**，要求其余计费端点逐条对齐。用例不声称哪个码"对"，只声称它们必须**一致**。

缺口是实的：`gateway_key_admission` 把停用 / 过期 / 封禁 / 白名单 / 未知模型五种条件
验得很细，但**只在 chat 上验**；其余端点各自跑一遍鉴权与模型解析，任一分支回了别的码，
用户侧就是"同一个错在不同接口报不同话"，而每个端点自己的套件都是绿的。

**抓到真缺陷**：余额不足时 `/v1/images/generations` 回 `502 upstream_error` 而非
`429 insufficient_quota`——因为图片请求没有 token，**ratio 定价算出来恒为 0**，
预扣 0 必过，余额为空的调用方一路打到上游（白嫖运营方的上游额度）。
`audio` 的 transcriptions 早有同款闸（`quote.snapshot.mode != "per_call"` 即 400），
images 两条路径都漏了，已补齐。

构造上的一个坑记下来：这类"跨端点共有条件"必须**各自喂它收得下的模型**
（images 只收 per_call，chat 收 ratio），否则撞的是配置错而不是要验的那个条件。

#### 二：参数组合不枚举，验**越界值必须被夹取或拒绝**（`list_pagination` 第二例）

"A=x 时 B 必须为 y"没有声明式来源，确实枚举不动；但其中**机械可判的那部分**是：
喂越界值不得 5xx、显式传的 `limit` 不得被静默当真（未夹的 limit 就是个 DoS 面）。
此前只有 `listing::page_params_are_clamped` 这一个**单元**测试，它验的是
`Slice::new` 本身，**不保证每个端点真的经由它取参**——手写 limit 解析绕开夹取，单测照样绿。

变异验证：移掉 `Slice::new` 的 clamp，`?limit=999999` 在 `/admin/users` 上真回 15994 行。

**一处差点误判**：`/admin/pools?offset=-5` 回 810 行，初看像"limit 没夹住"。
查 `listing.rs` 注释确认是**刻意设计**——`limit = None` 对配置类列表（下拉选项）就是回全量，
只有大表走 `capped_limit`。断言已收窄到"显式传了 limit 才断行数上限"。
**新写这类普适性探针时，先去实现侧确认该性质是否真的普适，别把设计当缺陷。**

#### 通则（补第二十三轮那条）

判一条轴"机械做不动"之前，先问：**能不能换成"跨实例一致性"或"普适不变量"**？
前者拿覆盖最好的那个实例当运行时参照系（本轮的错误码、第二十四轮的跨出口对账都是这个手法），
后者只断言与业务语义无关的边界（夹取、不 5xx、不重不漏）。两者都不需要把业务语义抄第二遍。

### 2026-09-19 第二十六轮：把上一轮明写的排除项收掉，并修补变异验证流程本身的漏洞

第二十四/二十五轮在计费端点对账表上写了句"videos（异步任务）与 realtime（WebSocket）
形态不同，各自套件已有专项覆盖，不在此表"——**这正是跨出口对账当初要推翻的那个理由**
（"各自套件已覆盖"恰恰是它看不见漂移的原因）。而且还漏提了第七个计费模块 `custom_pass`。

现在表里是七条：chat / embeddings / rerank / images / audio.speech / **videos** / **custom_pass**，
覆盖 ratio 与 per_call 两种定价、POST 与 GET 两种打法、同步与异步任务两种形态。
唯一仍在表外的是 **realtime**：它是 WebSocket 升级，HTTP 表驱动装不下——这是协议形态的
硬限制，不是"另有覆盖"这种托词；其结算同样经 `settle_write`，而那条路径被表里七条钉住。

#### 本轮更重要的产出：变异验证流程自己有个洞

给 videos 做变异时，连续两次注入"上游回 500"，用例都绿——差点写成"videos 上游失败
仍回 200 客户端"这个**并不存在的缺陷**。加了 `assert old in s` 才发现：
**`cargo fmt` 在我插入 mock 之后重排过那段代码，字符串替换早就匹配不上，两次变异都是空操作**。
修正匹配串后重跑：上游 418 → 客户端 502，用例正确报红——videos 的行为一直是对的。

所以第二十三轮那条通则要补一句：

> 变异验证必须**先断言变异确实注入**（替换前 `assert` 目标文本存在），再看用例是否报红。
> 否则"注入了但用例没红"与"压根没注入"在输出上无法区分，而后者会把正确实现误报成缺陷——
> 比漏测更坏。本轮实测踩到，且 `cargo fmt` 让它变得很容易发生。

配套：本轮给表驱动用例加了逐条状态输出的调试手法（临时 `eprintln`，验证完即删），
用来确认"每个 case 真的被打到"，而不是静默跳过——这比只看总数可靠。

### 2026-09-19 第二十七轮：realtime 也进对账（八个计费端点齐了），顺带发现新用例比老用例弱

上一轮把 realtime 留在表外，理由写的是"WebSocket，HTTP 表驱动装不下"。前半句成立、
后半句不成立——**装不进那张表 ≠ 不能验**。仓库里 `gateway_realtime` 早有 WS 客户端 harness
（`tokio_tungstenite` + `IntoClientRequest`），照搬过来单写一例即可，走与表里七条**完全相同**
的断言。至此八个计费模块（chat / embeddings / rerank / images / audio.speech / videos /
custom_pass / realtime）在跨出口对账上全覆盖。

#### 变异验证又抓到一次——这次抓的是新用例自己

realtime 用例初版写完即绿，两次变异都没咬住：

1. 把结算金额整体 ×2 —— 不红。**这是设计边界不是缺陷**：各出口仍然互相一致，
   而本用例验的是"出口之间是否一致"，不是"金额算得对不对"（后者归 `parity.rs` / `prop.rs`）。
   这条边界值得写下来，免得下次有人指望对账用例兜住算价错误。
2. 把落进 `billing_records` 的金额减半、ledger 扣款不变 —— **也不红，这就是缺陷了**。
   查下来：初版只断言了三个**从 `billing_records` 派生**的出口（门户日志、门户用量、生态口径），
   记账行一改它们跟着一起改，自然永远一致。而 chat 那条老用例还压了
   `billing_events` / Redis 热余额 / `users` 快照这些**独立累加器**——新用例漏了这一层，
   比老用例弱。补上后同一变异立刻红在"事件流的扣减与结算行金额对不上"。

通则再补一条：

> **照着已有用例新增同族用例时，先把老用例的断言集逐条过一遍**，别只搬骨架。
> 少搬的那几条往往正是它唯一能发现问题的地方——本轮的新用例看着结构一样，
> 实际只覆盖了派生视图，独立累加器一个没压。判断"新用例是否真的等价"，靠变异验证，
> 不靠肉眼比对结构。

### 2026-09-19 第二十八轮：把"业务语义交给各自套件"从断言变成实测

前几轮我反复说"各端点自己的业务语义正确性由各自套件覆盖，机械探针替代不了"。
**这句话本身从没被验证过**——正是这一整轮在抓的那类未经核实的假设。
这轮不写新探针，改为对**既有套件**做变异测试：把业务规则改坏，看它们抓不抓得住。
活下来的变异就是真缺口。

harness 在 scratchpad（`mut.sh`：注入 → 跑指定套件 → 还原 → 判 CAUGHT/SURVIVED），
关键是**先比对替换前后的行内容确认变异真的注入**（第二十六轮的教训）。

| 业务规则 | 变异 | 结果 |
| --- | --- | --- |
| service_tier 结算只降不升 | 比较反向（`<=` → `>=`） | CAUGHT，`gateway_tier` 两例报红 |
| 缓存倍率（ratio 模式） | `cache_ratio` → 恒 1.0 | CAUGHT |
| 分组倍率参与算价 | `group_ratio` → 恒 1.0 | CAUGHT |
| reasoning 后缀注入 effort | 删掉 `reasoning_effort` 注入 | CAUGHT |
| 音频输出倍率叠乘 | `audio_completion_ratio` → 恒 1.0 | CAUGHT（在 `parity.rs`，不在 gateway 层） |
| **图片输入倍率** | `image_ratio` → 恒 1.0 | **只被音频那条用例顺带抓住** |

#### 唯一的实质发现：`image_ratio` 靠邻居的 fixture 兜着

把图片轴改成恒 1.0，唯一报红的是 `openai_audio_official_pricing_parity`——
因为**它的 fixture 恰好也带了 2000 图片 token**。也就是说图片轴此前没有自己的用例，
谁把那条音频 fixture 简化掉（比如只留音频两轴），图片轴就会**悄无声息地失去覆盖**，
而全量仍然全绿。已补 `openai_image_input_ratio_parity`：只动图片轴、不依赖邻居，
另钉住"轴为 1.0 时必须与按文本计完全等价"。补后同一变异下它独立报红。

#### 两个方法论记号

1. **变异测试要选对层**。头两次把音频/图片判成 SURVIVED，是因为我只跑了
   `gateway_audio` / `gateway_images`，而这两条轴的对拍在 `okapi-pricing` 的 `parity.rs`。
   跨层的规则，变异时要把实现所在 crate 的测试一起跑，否则"存活"是假阳。
2. **"被抓住"还要看是被谁抓住**。同样是 CAUGHT，"有专属用例钉住"与"被别人的 fixture
   顺带覆盖"的健壮性差一个量级——后者会随邻居的改动无声失效。判断覆盖质量时要看
   报红的是哪一条用例，不能只看红没红。

### 2026-09-19 第二十九轮：把计费规则的分母列出来，17 条穷举变异

第二十八轮只变异了 6 条规则，**分母没交代**——"抽样 6 条全中"和"总共就 17 条、条条都中"
是两个强度完全不同的结论。这轮先把总体机械枚举出来，再逐条打。

总体取**计费引擎的全部规则点**（从源码枚举，不是拍脑袋挑的）：
`RatioSet` 的 7 条倍率轴 + 2 个全局乘子（分组 / 用户）+ 1 个档位修饰
+ `RuleKind` 的 4 个变体 + `PricingMode` 的 3 种模式 = **17 条**。

| 类别 | 条目 | 结果 |
| --- | --- | --- |
| 倍率轴（7） | model / completion / cache / cache_write / audio / image / audio_completion | 全 CAUGHT |
| 全局乘子（2） | group_ratio / user_multiplier | 全 CAUGHT |
| 档位修饰（1） | tier_ratio | CAUGHT（`gateway_tier`，不在定价 crate） |
| 规则类型（4） | Volume / TimeBased / Discount / Surge | 全 CAUGHT |
| 定价模式（3） | Ratio / PerCall / Tiered | 全 CAUGHT |

**17 / 17 CAUGHT。** 过程中发现并修掉的唯一实质缺口是上一轮那条（`image_ratio`
只被音频 fixture 顺带覆盖，已补专属对拍）。

#### 这轮最该记住的：变异测试的主要失败模式是 harness 自己搞错范围

本轮三次判出 SURVIVED，**三次都是假阳**：

1. `audio_completion_ratio` / `image_ratio`：只跑了 `gateway_audio` / `gateway_images`，
   而这两条轴的对拍在 `okapi-pricing` 的 `parity.rs`。
2. `tier_ratio`：只跑了 `-p okapi-pricing`，而档位的集成用例在 `gateway_tier`。
3. `TimeBased`：harness 写成 `cargo test -p okapi-pricing -p okapi --test gateway_pricing_rules`
   —— **`--test` 会把范围过滤到那一个 target，定价 crate 的测试压根没跑**。

换层重跑后三条全部 CAUGHT。所以变异测试的通则要写死：

> **SURVIVED 不能直接当结论**，必须先确认"该规则的测试到底在哪一层"并重跑。
> 跨 crate 的规则尤其容易踩：`--test <name>` 与多个 `-p` 同时用会静默缩小范围。
> 宁可把整个 workspace 跑一遍（慢但不会骗人），也别信一次范围可疑的 SURVIVED——
> 假阳会让人去"修"一个本来就正确的实现，比漏测更贵。

### 2026-09-19 第三十轮：把穷举变异推到第二个域（鉴权/准入），找到一处零覆盖的纵深防御

第二十九轮只在**定价**这一个域做了穷举（17/17）。这轮把同一口径推到**鉴权/准入**——
选它是因为它是全部 142 条路由的共同闸门，比任何单个端点的语义覆盖面都大。

规则点从 `AuthedKey` 与 `authenticate` 机械枚举。先打三条准入谓词：

| 规则 | 变异 | 结果 |
| --- | --- | --- |
| `key_status == 1` | 删掉该项 | CAUGHT |
| `expires_at > now` | 恒 true | CAUGHT |
| **`user_status == 1`** | 删掉该项 | **SURVIVED（全量 557 例确认）** |

#### 发现：`user_status` 这道闸一个用例都没压住

去掉"属主必须启用"，**全量一个都没红**。原因在既有那条封禁用例的语义——
管理端 ban 是"一刀切"，同时把名下 `api_keys.status` 也置 2，于是 `key_status`
那道闸先拦住了，`user_status` 在任何用例里**都从不是决定性的那一道**。

补用例时第一版写错了方向：直接改库把 `users.status` 置 2，结果在**未变异的代码上就红**——
那条路绕开管理端的缓存失效，本就有 TTL 滞后，是设计而非缺陷（既有用例名里的
`without_ttl_lag` 说的正是这件事）。改用 API 可达的路径才对：
**ban 是一次性 UPDATE，封禁之后再建的令牌仍是 `status = 1`**，其哈希不在鉴权缓存里、
必走一次库，于是 `user_status` 成为唯一能拦住它的闸。用例
`banned_owner_blocks_keys_minted_after_the_ban` 补上后，同一变异精确报红。

#### 这轮的方法学增量

> **"补用例时它在未变异代码上就红"是个强信号——先怀疑自己的前提，别急着报缺陷。**
> 本轮那一版红，红的不是实现，是我把"直接改库"当成了和"走管理端"等价的路径。
> 同一个不变量，**经由哪条路径触发**决定了它该不该立刻生效；写用例前要先确认契约说的是哪条。

域的推进口径也记一下：先挑**覆盖面最大的横切域**（鉴权 > 调度 > 单端点语义），
枚举其规则点，穷举变异，SURVIVED 一律升级全量复核。鉴权域剩余规则点
（IP 白名单、模型白名单、四个 `LimitCaps`、分组限流、团队月度限额、无效 key 限流、
积压熔断）按同法逐条推进，下一轮继续。


### 2026-09-22 第三十一轮：把穷举变异推完剩下的域；三处真缺口、一个测试卫生缺陷，以及一次对自己结论的纠正

第二十九、三十轮只做完了定价、鉴权两个域。这轮把同一口径推到**调度、额度/限流（ledger）、
协议转换、console 读端点**，并把鉴权域剩下的 6 条跑完。

#### 结果

| 域 | 规则点 | 结果 |
| --- | --- | --- |
| 鉴权准入（剩余） | 6 | 6/6 CAUGHT |
| 调度/渠道选择 | 10 | 9/10 → **1 条真缺口**，已补 |
| 额度/限流（ledger Lua） | 10 | 9/10 → **1 条真缺口**，已补 |
| 协议转换（文件级探针） | 6 个转换器 | 6/6 CAUGHT，但见下文"探针太粗" |
| 协议转换（`gemini_to_openai` 规则级） | 5 | 网关层只钉住 1/5 → **4 条真缺口**，已补 |
| console 读端点（`"data"→[]` 探针） | 16 个文件 | 15/16 CAUGHT → **`/v1/models` 零覆盖**（见下） |

#### 缺口一：无样本渠道"按中位数插队"，用例分不清中位数和最小值

`order_candidates_by_latency` 里无时延样本的 key 按本层**中位数**参与排序。把
`samples[len/2]` 改成 `samples[0]`（取最小），全量无一变红。原用例只喂了两个样本
`{50,500}`，并列时输入序决定胜负，恰好还是原来的相对位置。取最小和注释里警告的"给 0"
一样坏：新渠道排到本层最快渠道的位置，抢在已验证的好渠道前面拿流量。

改为样本 `{10,100,1000}`，分两个子 case（无样本者分别在输入最前、最后）——
"杀掉取最小"与"杀掉取最大"需要相反的输入序，一次排序做不到。
实测对取最小 / 取最大 / 取零三种变异全部变红。

#### 缺口二：并发槽键过期后结算，计数会被减成负数——这是个限额绕过

`commit.lua` / `refund.lua` 释放槽位前有 `GET > 0 才 DECR` 的守卫。改成无条件 DECR，
全量无一变红。既有用例里明明有一句"并发槽不会被重复释放成负数"，但它**走不到这道守卫**：
第二次 commit 在 `NO_RESERVATION` 处就提前返回了。

守卫真正起作用的场景是"预扣还在、并发键已经没了"——`conc:` 的 TTL 是 3600s，
长流式、视频任务、realtime 会话都会跨过它。此时无条件 DECR 在不存在的键上建出 `-1`，
下一次 reserve 判 `conc + 1 > conc_cap` 看到的是 0，这把 key 平白多一个槽；反复几次，
并发上限就被悄悄抬高了。补 `settling_after_conc_key_expired_does_not_go_negative`，
commit 与 refund 两条路径都覆盖，两处守卫的变异都精确变红。
（`refund.lua` 那处原清单没列，未单独跑全量证明其此前无人覆盖。）

#### 缺口三：Gemini 入口转换的映射规则，网关层用例只钉住 1/5

`gateway_gemini_ingress` 端到端覆盖了 Gemini 入口，文件级探针（输出键全改名）也被它抓住。
但逐条规则变异后，它只抓住"流式注入 include_usage"一条，另外四条改坏了全绿：
`candidatesTokenCount` 把 thoughts 算进去（SDK 用户看到的 token 数不对）、
functionResponse 配到最晚而非最早的同名调用（多次调用同名工具时结果串位）、
`length` 不映射 `MAX_TOKENS`（客户端检测不到截断）、thought 部件回灌进历史
（推理被当正文发给上游）。新增纯函数单测 `convert_gemini_ingress.rs`（13 例），
五条语义变异与文件级变异全部当场变红，耗时 0.00s。

#### 测试卫生缺陷：`gateway_shutdown` 会漏掉子进程，并让管道捕获的调用方永久挂死

console 读端点 sweep 卡了 20 小时。查下来是一个跑了 20 小时的孤儿 `okapi gateway`
攥着 sweep 的管道写端。来源是 `gateway_shutdown`：它用 `std::process::Command` 起真实
二进制、stderr 继承，而 `std::process::Child` 在 drop 时**不杀子进程**——SIGTERM 之前
任一断言失败（负载高时 `wait_healthy` 30s 超时最常见），服务进程就成了孤儿。
任何通过管道收集输出的调用方（CI 就是这样收的）都会因等不到 EOF 而挂死，而不是报失败。

加 `KillOnDrop` 守卫。验证方式是注入"SIGTERM 之前 panic"，用当初挂死的同一个模式
（`$(cargo test …)`）跑：50 秒返回、用例报 FAILED、零孤儿。
残余风险：测试进程本身被 SIGKILL 时 Drop 不执行（macOS 无 `PR_SET_PDEATHSIG`），不在用例层兜。

#### 我自己那条用例的缺陷：`route_error_envelope` 的 5xx 判据依赖环境

鉴权 sweep 的基线在全新库上是红的：`/pay/callback/{epay,stripe}` 在 `settings` 无配置时回
501 `payment_not_configured`，而判据是"匿名探测一律不得 5xx"。501 配具体 i18n 码、壳完好，
是刻意答复；支付回调是 webhook，设计上必须匿名可达。这条用例此前长期绿，只因旧开发库里
恰好有支付配置——**它依赖的是环境，不是语义**。判据收紧为"未处理的内部错误"
（500，或任何带 `internal` 码的 5xx）。

#### 对自己结论的纠正：两个"只被计费用例顺带覆盖"的转换器，其实都有网关层专属用例

sweep 记录显示 `gemini_to_openai`、`responses_to_chat` 只在全量里被计费一致性用例抓到，
我据此写了"无专属覆盖"。**这是错的**。sweep 用 `grep … | head -2` 记失败名，而第二条常是
`test result:` 汇总行——每次最多只看到一个失败用例名，排在前面的恰好是计费用例。
直接对专属套件复跑：`gateway_responses` 5 条失败恰好是全部降级路径用例（原生直转 3 条照常通过），
`gateway_gemini_ingress` 3 条失败恰好是全部走转换的用例。

同一个截断还制造了一条错误归因：`margin` 被记成被 `gateway_shutdown` 抓住（那是上面那个
孤儿问题的超时失败，与 margin 毫无关系），实际抓住它的是 `worker_margin_breaker` 里的专属用例。

#### 这轮的方法学增量

1. **基线绿检查。** 变异前先跑一遍未变异的套件，编译不过或本身就红就中止并写明"不是覆盖问题"。
   这轮它挡了三次：容器没起（编译不过）、迁移没跑（编译过但测试红）、用例自身依赖环境（红）。
   没有它，第二次那 6 条会**全部记成 CAUGHT**——测试因表不存在而失败，和"变异被抓住"看起来一模一样。
2. **失败名要记全。** 去掉 `head -2`，只匹配单条用例行、`sort -u` 全量记录。
   **CAUGHT(全量) 若记录的抓手与被变异代码无关，必须对专属套件复跑**——它可能是抖动掩盖了
   SURVIVED，也可能是截断藏起了真正的抓手；无论哪种，记录的证据都是错的。
3. **文件级探针太粗。** "输出键全改名""`data` 换空数组"只能测出"输出有没有人看"，
   测不出"映射规则有没有被钉住"。`gemini_to_openai` 在文件级探针下是 CAUGHT，
   逐条规则下是 1/5。**文件级 CAUGHT 不能当成该域覆盖充分的结论。**
4. **结果必须持久化。** 上一次鉴权 sweep 跑了 2 小时，结果全丢：写在 stdout、经 `column -t`
   缓冲、落在 `/tmp` 的任务输出里，跨天 `/tmp` 被清。`column -t` 还让它运行中全程零输出，
   一度被误判为卡死。脚本与结果改放仓库内 `.verify/`（gitignore），每条立刻落盘。
5. **`--test` 收窄范围的坑又踩了一次。** 鉴权清单写成 `-p okapi -p okapi-ledger --test …`，
   ledger 包自己的 `lua_contract` 没跑，四条限速 cap 只在全量才被抓到。第二十九轮已记过这条，
   这次是在新清单里重犯——清单模板应当默认不混用 `-p` 与 `--test`。
6. **并行 sweep 的边界。** 纯函数域（协议转换）可放独立 worktree + 独立 `CARGO_TARGET_DIR` 并行；
   碰库的域并行会互相制造抖动，抖动在全量复核里会变成假 CAUGHT。
7. **提交闸要跑在"HEAD + 本次改动"的干净树上。** 第一次在主仓库工作区里跑，clippy 报的
   `console_manage.rs` 绑定名过近其实是并行会话**未提交**的新增（HEAD 里没有），而 clippy
   在一个目标失败后不保证检查完其余目标——那份输出既不能证明我的改动干净，也不能说明
   main 是红的。改在独立 worktree 里只放本次要提交的文件重跑。
8. **截断又犯了一次。** 同一轮刚把 `head -2` 写成教训，提交闸的测试汇总又用了
   `sort | uniq -c | sort -rn | head -40`：不同汇总行超过 40 条，FAILED 行恰可能被截掉。
   规则收紧为：**闸的原始输出完整落盘，判定只对完整文件做 `grep FAILED`，任何展示用的截断
   都不能出现在判定路径上。**

#### console 读端点：`GET /v1/models` 在 HEAD 上零覆盖

把 `list_models` 的 `data` 换成空数组，全量无一变红。这是所有客户端（Claude Code、Codex、
Cursor、各家 SDK）发现可用模型时第一个调用的接口。归属要说清：**并行会话已在
`gateway_compat.rs` 里写了 `list_models_is_openai_shaped_and_skips_disabled`（未提交）**，
带正向断言"启用模型必须出现"，实测能抓住同一变异——本轮不提交该文件，缺口随那边的提交关闭。

顺带一个产品问题（不是缺陷）：该用例写明 `/v1/models` 刻意不做数据面鉴权、不按 key 过滤。
于是被 `model_allowlist` 限到两个模型的 key 也会拿到全量列表；客户端用它填模型选择器，
用户选中列表里的模型，打过去会被白名单拒掉。带 Bearer 时是否按 key 过滤，值得单独决定。

其余 15 个文件均 CAUGHT。按方法学第 3 条，这只说明"`data` 有人看"，不说明内容对。

另记一个等待脚本的坑：`until ! pgrep -f 'hollow.manifest'` 会**匹配到等待器自己的命令行**
（它本身含这个字符串），永远等不到退出——hollow 早在 09-22 11:26 跑完，三个等待器空转了
18 小时。改用 `pgrep -f '[h]ollow.manifest'` 这类不会自匹配的写法，或直接等结果文件出现"完"行。

#### 下一步（已暂存靶串，均对 HEAD 核验唯一命中）

- `openai_to_anthropic` 规则级 17 条（其中 usage 映射 4 条直接对着计费：prompt 须含
  cache_read 与 cache_creation）。
- console 写入面 8 条：超管守卫、角色取值、三处改完必刷的鉴权缓存、倍率范围，以及两条
  **权限错配**——RBAC 横切 sweep 只证明"完全没权限的用户会被拒"，`PRICING_WRITE` 错写成
  `USER_MANAGE` 它发现不了。

### 2026-09-23 第三十二轮：console 写入面与 Anthropic 转换逐条变异；三个用户侧 500，以及守护缓存刷新的用例在并行下失灵

两批规则级变异，均在独立 worktree 上、对已提交的树跑（第三十一轮第 7 条）。

#### 结果

| 批次 | 规则点 | 初判 | 复核后 |
| --- | --- | --- | --- |
| `openai_to_anthropic` | 17 | 13 CAUGHT（专属用例当场），2 SURVIVED，2 CAUGHT(全量) | **4 条真缺口**，已补 |
| console 写入面 | 8 | 3 CAUGHT，5 SURVIVED | **5 条真缺口**，已补 |

#### 两个 CAUGHT(全量) 都是假的

`max_tokens → length` 与 `cache_write = cache_creation` 被记成全量里抓住，抓手却是
`channel_batch_and_user_actions`、`invalid_key_*`、`videos_create_poll_download_bills_per_seconds`——
与停止原因、缓存计量毫不相干。按第三十一轮第 2 条复跑：变异后跑全量、跳过这三条，**零失败**。
所以两条在本轮之前都是真缺口，其中 `cache_write_tokens` 直接对着钱：它为 0 时 Claude 的缓存写入
按普通 prompt 计价（计价引擎有单独的 cache_write 轴）。既有用例断言了 `cached_tokens`，偏偏漏了它。

这批里按规矩复核了两条，**两条都是假 CAUGHT、底下藏着真缺口**。

顺带记下三条随执行顺序抖动的用例（单独跑全绿，全量里偶发红，本轮未修）：
`console_manage::channel_batch_and_user_actions`、
`gateway_invalid_key_rate::{invalid_key_trips_per_ip_limit, invalid_key_limit_is_per_ip}`
（限流按 IP 计数，全体用例都来自 127.0.0.1，前序用例留在窗口里的计数会让它提前跳闸）、
`gateway_videos::videos_create_poll_download_bills_per_seconds`。

#### `openai_to_anthropic` 补上的四条

`max_completion_tokens` 优先于 `max_tokens`（新版 SDK 两个都带、取值不同时取错）；
`refusal → content_filter`（映射成 `stop` 等于把拒答伪装成正常结束）；`max_tokens → length`；
`cache_write_tokens = cache_creation`。停止原因改成全表断言，四条变异均精确报红。

#### console 写入面：权限错配是 RBAC 横切扫描的盲区

5 条 SURVIVED：改角色后刷缓存、改分组后刷缓存、角色取值白名单，以及两条**权限错配**——
`set_user_multiplier` 的 `pricing.write` 换成 `user.manage`、`set_user_groups` 的 `user.manage`
换成 `user.read`（后者等于只读权限能执行写操作）。`route_error_envelope` 的 RBAC 扫描只证明
"完全没有管理权限的人会被拒"，**用错了权限点它看不见**。补 `scoped_admins_cannot_write_beyond_their_permissions`：
造只带单个权限点的管理员，每条都带正向对照，确认 403 来自权限而不是别的。

写权限用例时有个耦合要避开：若"先绑 A、测、再改绑 B、测"，改绑那一步本身就依赖刚才 SURVIVED 的
缓存刷新，两条规则会缠在一起。改为每个场景一个新用户，绑定发生在它的 token 第一次被使用之前。

角色白名单那条，注释里我起初写了"其它按 `== 10` 分支的地方不认越界值"，核实后是错的：
后端的角色判断全是阈值式（`>= 100` / `>= 10`），越界值会落进最近的档位，且只有超管能改角色，
**不构成提权**。白名单守的是库里的值始终是文档约定的三个之一。已改正注释。

#### 写用例时撞出三个用户侧 500（均已修）

分组那条用例第一版在**未变异代码上就红**，第二笔请求回 500。又是第三十轮那个坑——我直接往库里
插了分组，绕开了"发布"，网关价簿不认识它。但这次追下去，同一个坏状态**纯走 API 也到得了**：

1. **把用户放进未发布的分组**：建分组 200、分配 200，之后该用户**每笔请求都回 500**
   （`UnknownGroup` 按设计 fail-closed），直到有人发布定价。价簿从实时表加载，epoch 只是
   "该重载了"的信号，所以分组写进表不等于进了价簿。现在写入时回 400 `group_not_published`。
2. **放进不存在的分组**：撞 `user_groups` 外键回 500。现在回 400 `group_code`。
3. **给不存在的用户设分组**：撞 `user_id` 外键回 500。现在回 404，与改角色 / 改倍率一致
   （查询与 `manage.rs` 那句逐字相同，复用已有 `.sqlx` 缓存条目，不必重新生成）。

第 1 条的校验要查"本进程当前价簿"，这又牵出一个缺陷：**console 角色只订阅 NATS 广播，没有
gateway 那个 30s 轮询兜底**，未配 NATS 时 console 的价簿停在启动那一刻（MCP 健康工具报的
`pricebook_epoch` 也是旧的）。若直接拿它校验，刚发布的分组会被误拒。两处处理：写入前先按需
`refresh_pricebook_if_newer`（一条 `SELECT MAX(epoch)`），落后只会误拒、不会放进坏状态；
console 角色补上轮询，与 DESIGN §3.3 和 console 注释里"多副本答案要一致"的意图对齐。

#### 守护缓存刷新的用例，在默认并行下时灵时不灵

降权用例补上后，变异 `cw_role_flush` 仍 SURVIVED。单独跑、`--test-threads=1` 跑都 CAUGHT。
原因：`auth_flush()` 是**全局**清空，同一二进制里并行的其他用例每次改角色 / 分组 / 倍率都会触发它，
恰好把"降权后没刷缓存"冲掉。CI 用的正是默认并行——删掉刷新的回归可能照样过 CI。
倍率、分组两条同样受影响，只是验证那一轮时序凑巧。`console_users`、`console_pricing_write`
两个文件的用例改为拿同一把静态 `tokio::sync::Mutex` 串行执行（`std` 的锁跨 await 持有会被
clippy `await_holding_lock` 以 `-D warnings` 拦下）。

#### 加锁后的验证（默认并行模式，对最终代码）

| 变异 / 撤回 | 结果 | 抓手 |
| --- | --- | --- |
| 角色取值白名单 | CAUGHT | `role_outside_the_whitelist_is_rejected` |
| 改角色后刷缓存 | **3/3 CAUGHT**（加锁前 SURVIVED） | `demotion_takes_effect_without_waiting_for_the_auth_cache` |
| 改分组后刷缓存 | CAUGHT | `group_assignment_is_validated_and_billed_immediately` |
| 改倍率后刷缓存 | CAUGHT | `user_multiplier_is_writable_and_billed` |
| 倍率需 pricing.write / 分组需 user.manage | CAUGHT | `scoped_admins_cannot_write_beyond_their_permissions` |
| 撤回"未发布分组拒绝" / "不存在分组 400" / "按需追 epoch" / "不存在用户 404" | 均 CAUGHT | `group_assignment_is_validated_and_billed_immediately` |

#### 方法学增量

1. **守护"副作用必须发生"的用例，要证明它在默认并行下也能抓住变异。** 单独跑能抓住不够；
   共享的全局副作用（缓存清空、限流计数）会被并行用例互相掩盖。
2. **"未变异代码上就红"这次指向了真缺陷。** 第三十轮它指向的是我的前提错误；这次前提也错了
   （直接改库），但把路径换成 API 之后，坏状态依然到得了——所以换路径重试是必要的一步，
   不能停在"是我写错了"。
3. **提交闸必须以 `SQLX_OFFLINE=true` 跑。** CI 是离线编译的，本地连实库编译会把"缺 `.sqlx` 缓存"
   整个掩盖掉。本轮离线一跑，测试里新写的两条 `query!` 都没有缓存——推上去 CI 必挂。一条改成
   与既有缓存逐字相同的查询文本，一条（仅测试用）改成运行期检查的 `query_scalar`。
   第三十一轮那次提交闸是连实库跑的，当时恰好没新增查询才没出事——闸的步骤本身不完整。
