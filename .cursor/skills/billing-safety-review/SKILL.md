---
name: billing-safety-review
description: 审查触及计费路径（okapi-domain / okapi-pricing / okapi-ledger、Redis Lua 脚本、billing 相关迁移）的改动。在修改这些区域之后、或被要求 review 计费相关 PR 时使用。
paths: "crates/okapi-domain/**,crates/okapi-pricing/**,crates/okapi-ledger/**,**/*.lua,migrations/**"
---

# 计费安全审查清单

依次执行以下检查，最后输出逐项 ✅/❌ 报告；任何 ❌ 都必须修复或明确说明豁免理由。

## 1. 红线扫描（静态）

```bash
# 浮点与 panic 类（排除测试文件）
rg -n "f32|f64|as f64|\.unwrap\(|\.expect\(|panic!|todo!|unimplemented!" \
  crates/okapi-domain/src crates/okapi-pricing/src crates/okapi-ledger/src \
  -g '!*test*' -g '!*/tests/*'

# 状态转移兜底分支（人工确认命中处是否为状态机 match）
rg -n "_ =>" crates/okapi-domain/src crates/okapi-ledger/src
```

- 金额算术必须是 `checked_*` / `saturating_*`，裸 `+ - *` 视为违规。

## 2. 语义核对（对照文档）

- pricing_snapshot 字段完整：epoch、model_ratio、completion_ratio、cache_ratio、group + group_ratio、user_multiplier、rules 数组（每步 code/type/multiplier）、final 单价 —— 对照 `DESIGN.md` §3.4。
- 公式改动是否三同步：DESIGN §3 文档、parity fixtures、proptest 断言。
- 金额四列（amount / original / discount / upstream_cost）：若增删改，确认 PG `billing_records`、CH `request_log_raw`、事件 payload、`docs/database.md` 四处一致。
- reserve / commit / refund 幂等性：重复调用不会二次动账；新事件类型已接入 事件枚举 + chsink + 对账 + 文档。

## 3. Lua 契约（若涉及）

- KEYS 全部带同一 `{uid}` hash-tag；跨槽键（KPI、channel 计数）不在脚本内。
- 返回值结构与 `docs/database.md` §2.2 一致；文档已同步更新。

## 4. 测试执行

```bash
cargo test -p okapi-domain -p okapi-pricing -p okapi-ledger
# parity 对拍（老仓库黑盒套件，若环境可用）
pytest --scope p0
```

- proptest 全绿；new-api 公式对拍零差异；空回复不计费、负余额拒绝两个专项用例通过。

## 5. 输出格式

按上述 1–4 节输出清单，每项 ✅/❌ + 一句话证据（命令输出摘要或文件行号）。
