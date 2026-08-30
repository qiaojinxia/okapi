---
name: add-provider
description: 为 Okapi 新增上游 provider 适配或新协议转换方向时的标准流程（新渠道类型、新模型厂商、协议双向转换）。
paths: "crates/okapi-providers/**"
---

# 新增 Provider 标准流程

前置阅读：`IMPLEMENTATION.md` §4（providers 规范）与 §3.7（SSE 转发器语义）。

## 1. 模块落位（按方向拆，禁统一 IR）

- 原生适配：`okapi-providers/src/<provider>/`（请求构造、SSE 事件解析、usage 解析）。
- 协议转换：`okapi-providers/src/convert/<from>_to_<to>.rs`，显式转换函数，不引入中间表示。
- OpenAI 兼容上游优先复用 `deepseek/` 同款薄封装，不新建目录。

## 2. 实现 `Provider` trait

- `id()`、`capabilities()`（tools / vision / audio / cache 如实标注——能力感知路由依赖它）、`count_tokens()`（预扣估算）、`chat()`（返回统一 SSE 事件流）。
- 凭证：选择 `credential_kind`（static_key / oauth_refresh / cloud_sts）；非 static 走 `CredentialProvider` 四步锁（进程锁 → Redis 锁 → DB 重读 → 竞争恢复）。

## 3. usage 与计费对接

- 解析 prompt / cached / completion / reasoning tokens 与媒体单位（media_units）；缺 usage 时按本地 tiktoken 复核；尊重渠道 `trust_upstream_usage` 开关。
- **cache 相关请求/响应头与 usage 字段原样透传**（Codex CLI cache 失效是已知行业事故，见 IMPLEMENTATION §3.7-5）。

## 4. 流式语义

- 适配首字前缓冲：首个内容事件前不得让 gateway 写响应头；错误事件映射到重试矩阵类别（§3.6）。
- 心跳、客户端断开取消、空回复零计费语义由转发器统一处理，适配器只需正确产出事件流。

## 5. 数据与配置

- `models` / `model_pricing` 种子数据（倍率三元组或 per_call/media 价）；`channels.capabilities` 标签。
- 若新增端点：`IMPLEMENTATION.md` §4.4 协议矩阵加行。

## 6. 验收

- parity 用例：非流式 + 流式 + cache 透传 + 错误映射 各至少 1 条。
- `cargo test -p okapi-providers` 全绿；对照 checklist 输出完成报告。
