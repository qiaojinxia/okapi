# Okapi

AI API 中转网关（[ok-api](https://github.com/qiaojinxia/ok-api) v2 重构）：Rust 单二进制三角色（gateway / console / worker），倍率计费模型，PostgreSQL + Redis（+ 可选 ClickHouse / NATS）。

## 文档

| 文档 | 内容 |
| --- | --- |
| [DESIGN.md](DESIGN.md) | 调研结论、倍率计费模型 v3、前端设计 |
| [IMPLEMENTATION.md](IMPLEMENTATION.md) | 选型定案、架构、里程碑 M0–M4 与验收标准 |
| [docs/database.md](docs/database.md) | 存储层唯一权威（PG / Redis / ClickHouse / NATS） |

## 工作区布局

```
crates/
  okapi-domain     # 金额（i64 micro-USD）/ tokens / 计费状态机（M0）
  okapi-pricing    # PriceBook 编译器 + 修饰器栈 + ArcSwap 热更（M0）
  okapi-ledger     # Redis Lua 预扣/结算 + PG 事件溯源 + outbox（M1）
  okapi-providers  # Provider trait + 按方向拆的协议转换（M1+）
  okapi-store      # sqlx / fred / clickhouse / async-nats 薄封装（M1）
  okapi-api        # OpenAI 兼容 DTO + OpenAPI（M1）
bins/okapi         # 单二进制多角色入口：okapi gateway|console|worker|all
frontend/          # React 19 SPA（M3）
```

## 开发

```bash
bash scripts/dev-deps.sh up     # 启动 PG + Redis 开发容器（不在本机装任何服务）
cp .env.example .env            # sqlx 编译期校验与运行时共用
cargo test --workspace          # 全部测试（new-api 对拍 / proptest / M1 集成用例）
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/guard-no-float.sh  # 计费红线守卫（禁浮点/panic）

# 单机试跑（单用户模式：启动日志打印 root key）
OKAPI_SINGLE_USER_MODE=true cargo run --bin okapi -- gateway
```

当前进度：**M1 / M2 完成**——M1 闭环：鉴权 → 估价预扣 → 渠道 failover（首字前缓冲）→ SSE 透传 → usage 结算 → PG 记账+outbox；M2：worker 异步面（chsink/对账/清扫/分区/冷却恢复 + NATS relay 多机传输：outbox → JetStream → chsink 消费者，seq 区间幂等 + DLQ）、console 控制面（CRUD/RBAC 权限点/退款/门户 API + pricing.epoch 广播即时热更）、渠道状态机/三层粘性/双层并发/别名/可见性矩阵。M3 进行中：Anthropic 双向（/v1/messages 入口 + 上游方向，入口协议 × 渠道协议四象限）、Gemini generateContent 方向、/v1/embeddings、内置 MCP 只读工具面（`/mcp`，11 工具 + RBAC 过滤）、前端 SPA 第一批（登录/门户/管理后台骨架 + i18n zh-CN/en + 暗色主题 + console 静态托管 + i18n 守卫/CI）、/v1/responses 降级（事件骨架合成 + 两跳）、custom_pass 透传（路径白名单 + per_call 计费）、reasoning 模型名后缀（三向注入）、thinking-to-content、渠道测活端点 、能力感知路由 + 成本感知权重（§3.8）、/v1/images 媒体计费（per_call×n 入快照）、new-api ratio JSON 一键导入、公开价格页 + 账单解释器（前端） 、Setup 初始化向导（空库排他建超管 + 一次性 key）、定价模拟器 + 导入向导 UI（前端）、邮箱密码注册/登录 + TOTP 2FA（argon2id + RFC6238 + AES-GCM 信封；web session 只服务 /auth/*，门户/数据面保持 key 单轨） 、通用 OAuth/OIDC（github/discord/linuxdo 预设，配置驱动任意标准上游，state 一次性键 + 首登自动注册）、前端登录页三方式（邮箱密码/API Key/OAuth，TOTP 感知） 均已落地——**M3 主线收齐**（顺延项见 IMPLEMENTATION §13 状态注记）。M4 进行中：发布形态已就绪——`deploy/Dockerfile`（前端嵌入单二进制，`--features embed-web`）、compose（single/multi 双 profile）、K8s manifests（gateway HPA）、Nginx SSE 模板、`scripts/smoke-all.sh` 单机冒烟（验收项 ✓）、/v1/rerank 补齐；MCP 写工具面（三道闸 + confirm 两段式 + diagnose）与缩尺压测报告（docs/perf-report.md，含两处压测驱动的热路径修正：json 674→4016 RPS / stream 360→3098 RPS）已交付；迁移工具（`okapi migrate newapi`，样本库演练全量校验 ✓）与兑换码体系（批量生成/原子核销/MCP 工具）已交付；支付闭环（epay + Stripe，回调幂等验签，双网关端到端用例）已交付；audio 端点（speech 字符计费 / transcriptions per_call + multipart 重组 + duration 入快照）与 Playwright e2e 冒烟（3 链路全绿）已交付；Team 层（team 即 user 主体零热路径侵入、成员月度限额软实时计数、自助发团 key、按成员分账，全生命周期用例 ✓）已交付；生态对齐批（§11 复核 new-api rc.22–27 / Sub2API 0.1.164–183）：SSRF 校验（缺省禁私网+仅 https）、单用户 release 二次确认、new-api 兼容余额端点（/v1/dashboard/billing/*）、client_ip CDN 头采集落列、用户×模型 RPM、渠道上游模型发现（8MB 上限）已交付；Realtime WS 桥接（/v1/realtime 双向泵：连接预扣 → response.done 累计 usage → 断开 commit/零产出退款；per-key 连接租约 + 首消息/空闲超时，4 用例 ✓）已交付；videos 异步任务面（提交 per_call×seconds 计费 + 任务映射回源轮询/流式下载，3 用例 ✓）已交付；余额有效期（worker 到期原子清零 + expire 事件 + console 设置端点，用例 ✓）与邀请返利 aff（邀请码/注册绑定/充值返利/门户统计，用例 ✓）已交付；通知多路（webhook 通道 + drift/冷却/余额低三事件 + Redis 频率闸，2 用例 ✓）与套餐×兑换码增强（plans 模板/限用户/限 IP，2 用例 ✓）已交付；运维界面批（/admin/ops：用户消耗排行 CH 聚合 + 保留策略 worker 分区裁剪 + 通知多路配置，前后端 + 用例 ✓）已交付——**M4 范围内可本机交付项全部收口**；生态对齐补充批（复核 rc.27 / Sub2API 0.1.183 后）：关键接口每 IP 限流（login/register/totp/redeem，rc.24 对齐，用例 ✓）、按上游响应模型计费（渠道 opt-in，Sub2API 0.1.175 对齐，4 用例 ✓）、时段规则 rc.27 #6934 对拍（无同类缺陷 + 回归用例）、注册 UI（登录页第三 tab，?aff= 邀请链接闭环）与门户邀请页已交付；Linux 压测已落地容器限额复测（8 vCPU：json 10874 / stream 10402 RPS 全 0 错误，≥3k 验收线超 3 倍；并实锤修复了结算写入无背压的记账雪崩——12 处落点统一信号量闸 + 退避重试，散见 docs/perf-report.md 修正 #3；裸金属复测与 10 万 SSE 持有专项待办）；字段透传控制（rc.23 #6847，渠道 strip_request_fields，2 用例 ✓）已交付；service_tier 价格轴（Sub2API 0.1.179/180 对齐：档位倍率表 + 只降不升结算 + 快照档位，4 用例 ✓）已交付——**§11.3 生态吸收清单全部收口**；SSE 持有专项（容器缩尺 2 万条 × 120s：0 掉线 0 失败、RSS/fd 恒定无泄漏，10 万外推 8.7GB/20 万 fd 单机可承载，loadgen hold 模式 + soak 脚本入库）已交付；**老 ok-api API 面核对已完成**（源码 zip 到手：架构同源确认、能力对照表见 IMPLEMENTATION §11.4，本次补齐 /v1/audio/translations，差距项全部注记处置）——**外部依赖清零**；**老 ok-api 迁移工具**（`okapi migrate okapi-old`：五表 JSONL；bcrypt 密码双轨免重置登录、key_encrypted AES-GCM 解密重算 SHA-256、providers×keys→channels 展开、USD 单价→倍率换算；演练用例含 dry-run/幂等二跑/改密不回退/无口令降级四场景 ✓）已交付——**双源迁移（new-api + 老 ok-api）收口**；剩余为裸金属正式压测与 §11.4 backlog 小项。
