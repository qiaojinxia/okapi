# Okapi

AI API 中转网关（[ok-api](https://github.com/qiaojinxia/ok-api) v2 重构）：Rust 单二进制三角色
（gateway / console / worker），倍率计费模型，PostgreSQL + Redis（+ 可选 ClickHouse / NATS）。

一句话：把多家上游 LLM 供应商收敛成一个 OpenAI 兼容入口，按倍率精确计费，每一分钱可解释。

## 能做什么

- **多协议双向**：入口支持 OpenAI / Anthropic `/v1/messages` / Gemini / Responses；
  上游渠道同样是这三种协议，入口 × 渠道四象限互转。另有 embeddings / rerank / images /
  audio / videos / realtime WS / custom_pass 透传。
- **reasoning 归一**：`reasoning_effort`（OpenAI）/ `thinking`（Anthropic）/ 统一
  `reasoning:{effort,max_tokens}`（OpenRouter 形状）三种写法归一成一个内部指令，
  再按渠道方言展开成上游各自的参数——客户端不必知道这次会路由到哪家。
  只能填模型名的客户端另有 `模型名@effort:high` 语法，且变体可单独定价。
- **计费可解释**：金额全程 i64 micro-USD 定点（计费路径禁浮点，有 CI 守卫）。
  三种计价模式（倍率 / 阶梯 / 按次）× 分组倍率 × 用户系数 × 四类修饰器规则
  （量级 / 时段 / 折扣 / 负载加价），每笔账落一份可回放的定价快照。
- **路由与容错**：渠道池 + 优先级加权 + 能力感知 + 成本感知；三层会话粘性；
  key 级状态机（冷却 / 限流 / 配额耗尽 / 失效）与首字前 failover。
- **账目安全**：Redis Lua 原子预扣 → 上游调用 → usage 结算 → PG 事件溯源 + outbox；
  失败原额退款；三方对账（事件重放 ↔ Redis 热余额 ↔ PG 快照）与按账本修复。
- **运营面**：管理后台（渠道 / 定价 / 用户 / 分组 / 套餐 / 兑换码 / 团队 / RBAC）、
  用户门户、用量分析（ClickHouse）、审计、通知多路、双源迁移工具。

## 文档

| 文档 | 内容 |
| --- | --- |
| [DESIGN.md](DESIGN.md) | 调研结论、倍率计费模型 v3、前端设计 |
| [IMPLEMENTATION.md](IMPLEMENTATION.md) | 选型定案、架构、里程碑与逐项验收（§11 是改动流水账） |
| [docs/database.md](docs/database.md) | 存储层唯一权威（PG / Redis / ClickHouse / NATS） |
| [docs/perf-report.md](docs/perf-report.md) | 压测方法与结论 |

## 工作区布局

```
crates/
  okapi-domain     # 金额（i64 micro-USD）/ tokens / 计费状态机
  okapi-pricing    # PriceBook 编译器 + 修饰器栈 + ArcSwap 热更
  okapi-ledger     # Redis Lua 预扣/结算 + PG 事件溯源 + outbox
  okapi-providers  # Provider trait + 按方向拆的协议转换
  okapi-store      # sqlx / fred / clickhouse / async-nats 薄封装
  okapi-api        # OpenAI 兼容 DTO + OpenAPI
bins/okapi         # 单二进制多角色入口：okapi gateway|console|worker|all
frontend/          # React 19 SPA（管理后台 + 用户门户）
```

## 本地开发

```bash
bash scripts/dev-deps.sh up     # 起 PG + Redis + CH + NATS 开发容器（不在本机装服务）
cp .env.example .env            # sqlx 编译期校验与运行时共用
bash scripts/dev-reset.sh       # 重建库 + 灌演示数据（改了 migrations 后必跑）
cargo run --bin okapi -- all    # 单进程跑齐三角色
```

控制台 http://127.0.0.1:8081（`dev-reset.sh` 会打印演示超管账号与管理 key）。

验证：

```bash
cargo test --workspace                                   # 全量用例（依赖上面的开发容器）
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/guard-no-float.sh                           # 计费红线：禁浮点/禁 panic
bash scripts/guard-i18n.sh && python3 scripts/guard-i18n-keys.py   # 前端文案守卫
cd frontend && pnpm build && npx playwright test smoke   # 前端构建 + e2e 冒烟
```

## 部署

```bash
docker compose -f deploy/docker-compose.yml --profile single up -d   # 单机（okapi all）
docker compose -f deploy/docker-compose.yml --profile multi  up -d   # 多角色
kubectl apply -f deploy/k8s/okapi.yaml                                # K8s（gateway 带 HPA）
```

`deploy/nginx-sse.conf` 是前置反代模板（SSE 不缓冲、长超时、来源 IP 头归一化）。

**横向扩展前必须核两件事**：

1. **连接数**。每个 pod 各开一套 PG 池，总连接 = Σ(副本数 × `OKAPI_PG_POOL`)。
   走缺省 16 时 gateway 10 + console 2 + worker 1 就是 208 条，而 PG 缺省
   `max_connections=100`——扩到一半开始耗尽，且现象是 acquire 超时而非"连不上"。
   manifest 里已按副本上限配死（12 / 8 / 16），改 HPA 上限时记得一起改。
2. **信任来源**。不配 `OKAPI_TRUSTED_PROXIES`，转发头一律不作数（只认 socket 对端），
   `client_ip` 会变成 Ingress 的地址、key 级 IP 白名单会误拒。容器里反代与网关不同 IP，
   必须显式配网段；CDN 场景可改用 `OKAPI_EDGE_KEY`。

worker 保持单副本即可：周期任务（对账 / 清扫 / 分区 / 冷却恢复）不互斥，多副本只是重复劳动；
只有 chsink 吞吐不够时才值得加（relay/chsink 走 `SKIP LOCKED` + JetStream durable，多副本安全）。

主要环境变量见 [.env.example](.env.example)，每项都带了"为什么要有它"的注释。

## MCP 接入

console 角色内置 MCP 服务（Streamable HTTP，协议 `2025-06-18`），可直接挂到任意 MCP 客户端：

```bash
claude mcp add --transport http okapi http://127.0.0.1:8081/mcp \
  --header "Authorization: Bearer <你的 API key>"
```

工具按调用方的 RBAC 权限点过滤：普通用户 key 看到 5 个只读工具（余额 / 用量 / 密钥 /
价目 / 账单解释），超管看到 22 个（含平台 KPI、渠道健康、日志检索、对账、DLQ、全链路诊断）。

写工具走三道闸：站点级 `mcp_write_enabled` 总开关（**缺省关**）+ `mcp.write` 权限点 +
dry-run/confirm 两段式。总开关关闭时，写工具既不出现在 `tools/list`，直接调用也会被拒。

## 进度

M1–M3 主线与 M4 范围内可本机交付项均已收口，逐项验收记录见 IMPLEMENTATION §11 / §13。
已落地的能力粗分：

| 面 | 状态 |
| --- | --- |
| 数据面 | 四象限协议互转、SSE 透传、failover、限流四件套、Realtime WS、videos 异步任务 |
| 计费 | 三种计价模式、修饰器栈、service_tier 档位、上游成本与毛利、退款与对账修复 |
| 控制面 | 渠道/池/定价/用户/分组/套餐/兑换码/团队/RBAC、审计、通知多路、MCP 工具面 |
| 异步面 | outbox → NATS JetStream → chsink → ClickHouse、对账、清扫、分区、冷却恢复 |
| 前端 | 管理后台 + 用户门户 + 公开价格页，i18n zh-CN/en，暗色主题 |
| 迁移 | `okapi migrate newapi` / `okapi migrate okapi-old` 双源 |
| 压测 | 容器 8 vCPU：json 10874 / stream 10402 RPS 全 0 错误；SSE 2 万条持有 120s 无泄漏 |

未完成项：裸金属正式压测、gateway 微批组提交（等压测确认 PG 写瓶颈）、
`reserve` 侧 Redis 缺键回源兜底、§11.4 backlog 小项。
