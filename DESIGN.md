# Okapi（ok-api v2）重构设计：倍率计费模型 + 高并发计费统计架构

> 状态：调研 + 设计稿（2026-08-29）
> 前身：[qiaojinxia/ok-api](https://github.com/qiaojinxia/ok-api)
> 调研来源：[new-api 倍率设置文档](https://www.newapi.ai/zh/docs/guide/console/settings/rate-settings)、
> [new-api relay/helper/price.go](https://github.com/QuantumNous/new-api/blob/main/relay/helper/price.go)、
> ok-api `OK_API_SYSTEM_DESIGN.md` / `internal/billing/README.md` / `docs/adr/scale-architecture-500m.md`

## 0. 结论速览

1. **计费对外视图切换为行业通用的"倍率制"**（模型倍率 / 补全倍率 / 分组倍率，与 new-api 公式对齐、配置可直接互导），**内部记账保持 ok-api 现有的强类型金额制**（micro-USD 整数），两者只是同一价格的两种视图，换算关系固定。
2. **现有计费流水线资产全部保留**：billing-core Pipeline（预扣/Commit/Refund）、`billing_events_v2` 事件溯源、outbox、DLQ、chsink → ClickHouse。重构的是**定价域（pricing）**，不是记账域（ledger）。
3. **现有灵活性（用户专属定价、折扣/加价/量级/时段规则叠加）保留**，重新定义为倍率公式之后的"修饰器栈（modifier stack）"，每一步落审计快照。
4. 千万级/日的计费统计在现架构容量之内（ok-api ADR 已按 5 亿/天设计）；本次重构顺带做三件性能事：**定价解析编译缓存**、**Redis 预扣/结算 Lua 合并**、**Redis Cluster 分片预留**。
5. 命名推荐 **Okapi**（霍加狓），品牌延续 ok-api，详见 §7。
6. 后端可选 **Rust 重写**（§8）：7 微服务合并为 3 角色（gateway/console/worker），单二进制多角色；前端 React SPA + shadcn/ui，构建产物嵌入二进制单文件部署（§9）。
7. **配套文档**：实施定案、里程碑与验收见 [IMPLEMENTATION.md](IMPLEMENTATION.md)；存储层全量 schema 与契约见 [docs/database.md](docs/database.md)（本文 §4 为定价域示意）。

---

## 1. 调研：new-api 的计费模式（行业事实标准）

new-api（及其上游 one-api、衍生的 one-hub/done-hub 等）已经把"倍率"变成了中转站行业的价格语言——站长和用户都用"这个站 gpt-4o 倍率多少"来沟通价格。核心机制：

### 1.1 配额（quota）体系

- 内部记账单位是整数 quota，**$1 = 500,000 quota**（`QuotaPerUnit`）。
- 倍率基准：**倍率 1.0 = $0.002 / 1K tokens = $2 / 1M tokens**（1 token × 倍率1 = 1 quota）。

### 1.2 三层倍率公式（按 token 计费）

```
配额消耗 = (输入tokens + 输出tokens × 补全倍率) × 模型倍率 × 分组倍率
```

| 倍率 | 作用域 | 含义 |
| --- | --- | --- |
| 模型倍率 ModelRatio | 模型 | 相对基准单价（$2/1M input）的倍数，反映模型成本差异 |
| 补全倍率 CompletionRatio | 模型 | 输出 token 相对输入 token 的倍数（如 GPT-4o 为 4） |
| 分组倍率 GroupRatio | 用户组 | 分组差异化定价（default/vip/svip…） |

优先级：**用户专属倍率 > 分组倍率 > 默认倍率（1.0）**。

### 1.3 其他计费形态

| 形态 | 公式 / 机制 |
| --- | --- |
| 按次计费 | `模型固定价格 × 分组倍率 × 500,000`（quota） |
| 缓存 token | 缓存命中部分按 `CacheRatio` 打折（如 0.25/0.5） |
| 图像 | `ImagePriceRatio` 修正固定价 |
| 阶梯计费 | `billing_expr` 表达式（$/1M 价格表达式 → quota），按 token 区间分段 |
| 免费模型 | 模型倍率/固定价为 0 或分组倍率为 0 → 免预扣 |

### 1.4 两阶段扣费

1. **预消费**：请求前按预估 tokens × 倍率预扣 quota；
2. **结算**：上游返回真实 usage 后按实际 tokens 重算，多退少补。

与 ok-api 现有 reservation（预占）→ Commit/Refund 模型同构，**这一块两边思路一致，无需改**。

---

## 2. 现状盘点：ok-api 的计费模型

### 2.1 现有定价结构（绝对价格制）

```
最终价格 = 基础费用 × 用户倍率 × 负载系数 − 折扣
基础费用 = input_price × 输入tokens + output_price × 输出tokens   （$/1K，绝对美元价）
```

| 层 | 载体 | 说明 |
| --- | --- | --- |
| 模型基础价 | `models.input_price / output_price / request_price / image_price / audio_price / cached_*` | 6 种 pricing_type：token/request/time/hybrid/image/audio |
| 用户专属价 | `user_pricing`（user × model 覆盖价，优先级最高） | 完全替换模型基础价 |
| 用户倍率 | `users.price_multiplier`、子账户 `price_multiplier` | 0.8 = 八折 |
| 规则叠加 | `pricing_rules`：base/discount/surge/volume/time_based | 夜间折扣、量级折扣、加价等 |

### 2.2 与 new-api 的差异

| 维度 | new-api | ok-api 现状 | 结论 |
| --- | --- | --- | --- |
| 价格表达 | 倍率（行业习惯） | 绝对美元价 | **改**：倍率为主视图 |
| 记账单位 | int quota（$1=500k，精度 $2e-6） | `money.Amount` 6 位小数（精度 $1e-6） | **保留金额制**，精度更高且天然对齐美元 |
| 输出定价 | 补全倍率（相对输入） | 独立 output_price | 改为补全倍率，换算无损 |
| 分组定价 | 分组倍率 | 无分组概念（只有用户倍率） | **新增用户分组** |
| 用户专属 | 用户专属倍率 | user_pricing 专属绝对价 | 保留，统一表达为"专属倍率或专属价" |
| 规则引擎 | 无（只有倍率乘法） | 5 类规则可叠加 | **保留**，ok-api 的差异化优势 |
| 阶梯计费 | billing_expr 表达式 | 无 | 借鉴，纳入规则栈 |
| 记账链路 | 直接 UPDATE 余额 | 事件溯源 + outbox + DLQ + CH | **保留 ok-api**，明显更强 |

### 2.3 保留的资产（不重写）

- `internal/billing/` 全套：`money/`（强类型金额）、`tokens/`、`state/`（状态机）、`core/`（Pipeline 唯一入口）、`eventstore/`、`chsink/`、`dlq/`、`projector/`；
- 同步路径 Redis 预扣 / Commit / KPI，异步路径 NATS JetStream → ClickHouse 6 张 AggregatingMergeTree MV；
- 500M/天 扩容 ADR 的存储分层与 MV 矩阵设计。

> 若采用 §8 的 Rust 重写方案，本节"保留"指**协议、语义与黑盒资产**保留：pytest API parity 套件、
> SQL migration / CH schema、Redis Lua 脚本、K8s/compose 编排全部语言无关可直接复用；
> Go 代码本身按 §8.3 的 crate 映射做语义移植。

---

## 3. 目标计费模型 v3：倍率为视图，金额为真理

### 3.1 设计原则

1. **单一真理源**：模型定价的真理源是倍率组（model_ratio / completion_ratio / cache_ratio / cache_write_ratio），绝对价格（$/1M）是派生视图，管理后台双向换算展示、双向可编辑（编辑任一侧，落库为倍率）。
2. **记账不变**：`engine.Calculate` 的输出仍是 `money.Amount`（micro-USD int64）；quota 视图 = USD × 500,000，仅用于对外展示/导出兼容 new-api 生态。
3. **解析与计算分离**：请求路径上只做 O(1) 的"已编译价格表"查找 + 纯乘加运算；所有规则匹配、优先级仲裁在配置变更时离线编译完成。
4. **每笔账可解释**：计费记录携带 pricing_snapshot（命中的倍率、分组、规则链、每步乘数），审计可回放。

### 3.2 统一计费公式

```
基准价 base_unit = 已发布的 pricing_base_per_1m_micro / 1M tokens（micro-USD；默认 $2 / 1M，与 new-api/one-api 对齐）

token 费用 = base_unit × model_ratio
           × ( prompt_uncached                              ← 常规文本输入（五段互斥）
             + cached_text()          × cache_ratio         ← 文本缓存读取
             + cache_write_text()     × cache_write_ratio   ← 文本缓存写入
             + cached_image           × image_cache_read    ← 图片缓存读取
             + cached_audio           × audio_cache_read    ← 音频缓存读取
             + written_image          × image_cache_write   ← 图片缓存写入
             + written_audio          × audio_cache_write   ← 音频缓存写入
             + audio_prompt_tokens    × audio_ratio         ← 音频输入（官方 16×）
             + image_prompt_tokens    × image_ratio         ← 图片输入
             + text_completion        × completion_ratio    ← 文本输出
             + image_completion_tokens× image_output        ← 图片输出
             + audio_completion_tokens× audio_out_ratio )   ← 音频输出（见下）
           × group_ratio
           × user_multiplier
           × Π rule_modifier_i        ← 规则修饰器栈（可为空）

其中 audio_out_ratio = audio_ratio × audio_completion_ratio（与 new-api 同语义：
音频输出相对音频输入再乘一档）；**但两轴均未配置（都是 1.0）时回落为 completion_ratio**
——否则文本输出按 completion_ratio（如 4×）而音频输出按 1× 计，会把既有音频输出
悄悄打折，与"模态轴缺省应零影响"的约定相悖（回归断言见 parity.rs
`openai_audio_official_pricing_parity`）。

按次费用   = per_call_price × group_ratio × user_multiplier × Π rule_modifier_i
媒体/时长  = media_price × 数量 × group_ratio × …（同上）
阶梯       = tier_expr(tokens) 求得 $/1M 后代入上式的 model_ratio 位置
```

倍率与绝对价换算（无损、双向）：

```
model_ratio            = input_price_per_1M / 2
completion_ratio       = output_price_per_1M / input_price_per_1M
cache_ratio            = cache_read_price  / input_price
cache_write_ratio      = cache_write_price / input_price
audio_ratio            = audio_input_price / input_price
audio_completion_ratio = audio_output_price / audio_input_price
image_ratio            = image_input_price / input_price
例：GPT-4o            $2.5/$10 per 1M  →  model_ratio=1.25, completion_ratio=4
例：claude-3-5-sonnet $3/$15，缓存读 $0.3、写 $3.75
    →  model_ratio=1.5, completion_ratio=5, cache_ratio=0.1, cache_write_ratio=1.25
```

**模态分轴（多模态模型的必需项）**：多模态模型各模态**不同价**。以
gpt-4o-audio-preview 官方价为例（text in $2.5/1M、text out $10/1M、audio in $40/1M、
audio out $80/1M）：反解得 model_ratio=1.25、completion_ratio=4、audio_ratio=16、
audio_completion_ratio=2。若不分轴而全按文本计，实测该场景漏收约 **80%**。

**缓存与模态的交叉**：`cached_tokens` / `cache_write_tokens` 是包括模态子集的总量，
`cache_read_modalities` / `cache_write_modalities` 保存其中的图片、音频计数，文本取余。
`image_prompt_tokens` / `audio_prompt_tokens` 只计未缓存部分，不能再包含缓存子集。
`modality_ratios` 的上述五项均相对文本输入价，以十进制字符串配置；显式配置优先，
缺省缓存模态价为对应输入倍率 × 缓存读/写倍率（定点乘积 floor），图片输出缺省为
`completion_ratio`，保留原有图片定价。仅计算实际发生的轴，未使用的轴不应因乘积溢出
影响普通请求。实际使用的交叉价格和计数进入 `pricing_snapshot`，PG/outbox 同源。
领域/引擎已支持矩阵；直接 Images 接口接入。Chat/Responses/Realtime 原探针仍有
按缓存优先分配的旧路径，不能据此宣称全部协议的交叉用量已完整采集。

**直接 Images API 的映射**：`input_tokens_details.text_tokens` 与 `image_tokens`
之和必须等于 `input_tokens`，包括缓存部分。兼容上游的 `cached_tokens_details`
明确拆分图文缓存；给总量和其中一个子集可唯一推导另一项，给两个子集可推导总量。
混合图文只有非零缓存总量且不能唯一拆分时拒绝，不能默认为文本缓存；纯文本、纯图片、
零缓存和全部输入已缓存时可唯一确定。缓存写入使用 `cache_write_tokens` 与
`cache_write_tokens_details`，与读取分别校验，不得重叠或超过模态输入总量。
若提供 `output_tokens_details`，按图文实际拆分；缺少该字段时沿用全部为图片输出的
直接 Images 契约。`image_output` 独立配置图片输出，文本输出用 `completion_ratio`。
按响应用量计算一次，不再乘 `n`，
实际图片张数仅写入 `media_units`。`image_usage` 快照保存文本输入、图片输入和图片输出
总计数，有文本输出时另存文本输出；`image_cache_usage` 保存图文读写缓存和采集状态。
PG 用量、CH outbox 的总输入/输出同源。按张定价也采集上报用量，金额仍按
成功张数计算。缺失用量与明确的零用量分别记录；Token 定价缺失/损坏用量返回 502 并退款。
数值依据见 `tests/fixtures/image_cache_parity.json`：固定 Sub2API 图片样本在取整前
为 6462.5 micro，Okapi 按既有整 micro 规则取 6462；双倍用量为整数 12925 micro。

Token 预扣以 prompt UTF-8 字节数估计文本、每个输入图片/mask 引用 8192 Token、
`models.max_output`（缺省 8192）乘请求张数估计输出；此估计用于余额/TPM 准入，
不是实际用量或费用上限。实际结算使用请求开始时固定的价簿和规则上下文。

**prompt 三段互斥（不可省的一段）**：`prompt_tokens` = 常规 + 缓存读 + 缓存写，
`prompt_uncached = prompt − cached − cache_write`。缓存读打折（Anthropic 0.1×）与缓存写加价
（1.25×@5m TTL / 2.0×@1h）方向相反，**必须分轴**：只有单一 cache_ratio 时，写入段会被混入常规
输入按 1.0× 计 —— 对 claude 缓存写入场景每笔漏收约 20%（回归断言见
`crates/okapi-pricing/tests/parity.rs::anthropic_cache_write_is_billed_as_separate_segment`）。
`cache_write_ratio` 缺省 1.0 = 退化为旧行为，故对无缓存写入概念的 provider（OpenAI 隐式缓存、
Gemini 显式缓存走独立 API 计费）无副作用。

### 3.3 定价解析管线（配置时编译，请求时查表）

```mermaid
flowchart LR
    subgraph 配置面["配置面（admin 变更时触发）"]
        MP[model_pricing 倍率三元组] --> C[PriceBook 编译器]
        PG[price_groups 分组倍率] --> C
        UP[user_pricing 专属倍率/价] --> C
        PR[pricing_rules 规则栈] --> C
        C -->|"epoch+1 全量快照"| PB[(PriceBook vN\nRedis + PG)]
    end
    subgraph 请求面["请求面（每请求 O1）"]
        RQ[请求 model+user] --> L1[进程内 L1 缓存\nepoch 校验]
        L1 -->|miss| PB
        L1 --> CALC["engine.Calculate\n纯乘加 → money.Amount"]
    end
    PB -.->|NATS pricing.epoch 广播失效| L1
```

- **PriceBook**：`(model, group) → 已编译费率行`（micro-USD/token 定点数），用户级覆盖单独一张小表；带全局单调 `pricing_epoch`。
- 运行期动态因素的处理：
  - `time_based`（时段折扣）：编译为带生效时间窗的修饰器，请求时本地时钟比较，零 IO；
  - `volume`（量级折扣）：用户月用量走现有 Redis KPI 计数器，请求时一次读缓存值与预编译阈值比较；跨过阈值不需要重编译；
  - `surge`（负载加价）：网关本地负载指标，本地判断。
- 失效路径：admin 保存 → 事务内 epoch+1 并写 PriceBook 快照 → NATS 广播 → 各实例 L1 失效重拉。广播丢失兜底：L1 每 30s 对 epoch 做一次轻量校验。

### 3.4 优先级与规则栈（保留现有灵活性）

解析顺序固定、可审计：

```
1. 模型倍率三元组          （model_pricing，真理源）
2. 用户专属覆盖            （user_pricing：专属倍率 或 专属绝对价 → 内部统一转倍率）
3. 分组倍率                （用户所属 price_group；未分组 = default 1.0）
4. 用户/子账户 multiplier   （users.price_multiplier，保留）
4.5 service_tier 档位修饰（Sub2API 对齐）：有效 model_ratio = model_ratio × tier_ratio(结算档)。
    tier_ratio 配置在 model_pricing.tier_ratios（JSONB，如 {"flex":"0.5","priority":"2.0"}；NULL=全档 1.0）；
    结算档 = 请求声明档与上游响应报告档中倍率较低者（**只降不升**：不为未享受的档位付费，也不因上游擅自
    升档而多收）；未配置档位名按 1.0。快照记录 service_tier 与 tier_ratio（账单可解释）。
5. 规则修饰器栈             （按 rule_type 固定序 volume → time_based → discount → surge，
                            同类按 priority、rule_code 排序；每步输出乘数或增量，写入快照）
   四类规则的触发输入：volume 读 Redis `tok:{uid}:<yyyymm>`（本月累计 token，结算后累加）；
   time_based 读站点本地分钟窗；discount 无条件；surge 读单 gateway 进程在途计费请求数与
   `settings.surge_inflight_threshold` 比较（缺省 0 = 永不触发）。volume 与 surge 的输入采集
   均由价簿内是否存在该类启用规则门控，无规则时热路径不产生任何额外读取。
```

pricing_snapshot（存入 billing_records，jsonb）示例：

```json
{
  "epoch": 1042,
  "model_ratio": 1.25, "completion_ratio": 4, "cache_ratio": 0.5,
  "cache_write_ratio": 1.25, "audio_ratio": 16, "audio_completion_ratio": 2,
  "group": "vip", "group_ratio": 0.9,
  "user_multiplier": 1.0,
  "rules": [
    {"code": "night-discount", "type": "time_based", "multiplier": 0.8}
  ],
  "final_unit_price_input_per_1m_usd": 1.8,
  "requested_model": "gpt-4o"
}
```

`requested_model` 仅在**发生模型级降级**时出现（否则与账单的 model 相同，属冗余）。

#### 结算价格版本与失败金额

携带报价快照的结算记录，其 `pricing_epoch` 必须来自同一份报价的 `pricing_snapshot.epoch`。请求等待上游期间发布新版 PriceBook，不得只更新账单版本标签；沿用该路径实际报价时的金额、倍率和版本。音频、视频及自定义透传不能在写账时再次读取当前 PriceBook 来标注旧报价。PG、outbox、CH 与持久结算回执保留同一版本；本约束不改变现有各路径的报价时点，也不修改旧账单。

自定义透传失败后已释放预扣，不得将预报价计入原消费或优惠：消费、原金额、优惠和上游成本均按失败零消费语义记录，报价快照仍可保留作为尝试所用配置的证据。钱包与两池归属、退款幂等和重试策略维持已有契约。真实 HTTP 上游等待与生产价簿切换原语并发、成功/失败金额已在固定源码验证，财务三包与已选关联套件分别通过；范围与未完成项见 [价格版本一致性核对](docs/pricing-epoch-consistency-audit.md)，不能将其扩大为所有价格发布链路和历史统计已验收。

### 3.4.0 字符计费与 Token 单位分离

统计投递也必须与账本幂等分开验证：outbox ID 或 JetStream 序号的首尾区间不能代表不可变批次。新的统计投递协议先在 PG 提交事件回执与冻结批次（最多 500 行），再发送同一份 CH 行、同一个随机批次 token；PG 完成标记失败时不得重新组批。outbox 增加服务端事件 UUID，relay 将其作为发布去重 ID 和消息中的内部身份，直连、NATS 重投及形态切换共用 PG 回执。老 JS 消息缺身份时仅以流创建时刻与原序号识别同条消息，无法推断历史重复发布是否属于同一事件。

NATS ack 的新边界是 PG 已持久接管，而不是尚未持久保存的消费批次：PG 批次负责后续 CH 重试、退避和 DLQ，消息 ack 丢失不产生新统计批次。已完成回执保留身份，完成事务可清除大载荷；待处理和 DLQ 批次保留完整冻结行。DLQ 重投必须恢复原整个批次，不能抽出其中一行换 token，操作影响的批次成员数必须如实返回；MCP 确认前预览完整成员数量。丢弃批次必须显式选中全部未处理成员，部分选择时拒绝且无写入，不能自动丢弃其它成员。列表返回批次大小并支持按 batch_id 查看至多 500 个原成员。不可解析消息应入 DLQ，不能默认为空事件写入统计。

此协议不承诺跨存储无限期 exactly-once：CH 写成而 PG 未完成的模糊窗口仍依赖 CH 去重记录；当前表窗口为 1000 个块，记录被驱逐、CH/MV 部分写失败、历史旧协议切换及数据库恢复/目标库更换需要对账与专门修复，不能盲目当作已验证。实施证据见 [统计投递幂等核对](docs/billing-delivery-idempotency-audit.md)。

新的 relay 确认发布时设置 outbox `stats_protocol=1`；旧已发布行保持 0，不能因缺少新回执就盲目重放历史。NATS 回退直连可接管标记为新协议且尚无批次的已发布行；NATS 形态超过五分钟未持久接管时也从仍保留的 PG outbox 恢复，以免总线过期造成永久漏统计。恢复仍使用原事件身份，晚到消息不新增统计；健康积压必须包含此阶段。

历史 speech 校准采用独立、无 TTL 的统计证据表，不修改 PG 财务账单、金额或已存在的主聚合。只有精确 speech 端点、旧快照形状与版本/分组、非流式成功记录、无上游 Token 证据且其余 Token 轴为零时，原 `prompt_tokens` 才解释为字符。显式新单位、冲突数量/版本或缺失证据不能猜测。校准证据按原请求/时间/全部筛选维度保留，重跑幂等；查询在同范围主输入与总 Token 中排除该数量，字符独立恢复。历史 raw 仍在时由 worker 分页校准并保存证据；raw 过期后仍从保存证据校准，未保存且不可识别的历史继续提供未知覆盖。分析的排序、Top N、占比和流向必须在校准后计算，不能只在输出端改数字。分析可在完整当前/上期窗口和基础归属范围以实时稀疏探针确认无证据后跳过校准查询及表达式；该缺省判断不能缓存，存在证据时仍必须启用校准，查询护栏不变。确认的旧字符同时补足速度统计中的非 Token 单位覆盖，但不增加 Token 请求、输出或耗时配对样本；旧字符耗时不能进入 Token 速度分母。覆盖计数必须按同范围校准，冲突超出账单人口时拒绝而非截断。实施和实际 API 回归见 [历史 speech 校准](docs/historical-speech-unit-audit.md)，未完成项保持显式待验证。

当前窗口与上一窗口必须分别按各自时间及归属范围判定每个粒度的观察覆盖与历史字符证据；不能因当前窗口缺少某个旧聚合，就让已经完整的上一窗口也执行历史恢复。缓存恢复和来源/单位/速度统计均使用对应窗口的判定，单查询的时间和内存护栏保持不变。

分析查询在主聚合子查询内先限定时间和归属，再以显式基表键连接每类独立观察；同粒度元数据只择一并以原请求数核对，不叠加或改变主请求、金额。连接后主键及基础聚合列显式引用基表，避免多重 USING 对同名键的合并展开；仍使用原数据库分析器和查询护栏。

`/v1/audio/speech` 的现有字符定价继续按 Unicode scalar 数量计算（空格和组合字符各按当前 Rust `chars()` 计数），保留已有 ratio、tiered、per_call、基准价、分组及规则取整结果。通过独立 `calculate_characters` 报价入口复用现有定点价格链；在定价域内将字符数量作为输入计价单位，快照明确 `input_unit=characters`、`input_characters=N`。单价字段此时是每百万字符的输入价格，不能解释为每百万 Token。

离开定价域后 `TokenUsage` 始终只表示 Token；没有上游 Token 用量的二进制 TTS 不把本地字符数放进输入、总量、TPM 预估、Token 规则累计或实时 Token 计数。仍照常计请求、并发、花费和余额。字符单独随 PG 使用详情、定价快照、outbox 和 CH 明细保存，明确零与未记录分开；数据明细 API 提供相同单位字段。

已有存量可能把字符放在 `prompt_tokens`，必须与新口径区分，不能用模型名猜历史单位，也不能改历史金额或直接重写财务账本。仅有明确 speech 端点且符合旧字符计费契约的旧数据才可在读取/重放层识别；缺少证据或数量的历史返回未知与覆盖信息。无 TTL 的独立字符聚合与跨端点单位覆盖已按 [字符单位核对](docs/character-unit-audit.md) 接入并验证；历史主统计的单位校准仍待后续核对；Token 速度分母已按下述独立单位契约接入并验证，不能仅凭新账单归零宣称所有历史看板正确。

#### Token 输出速度的单位约束

平均总耗时与耗时分位数继续统计全部有效测时请求，包括字符计费和失败请求。`tokens_per_1k_sec` / `avg_output_tps_milli` 则使用明确 Token 单位且已测总耗时的同一批请求：输出总数 / 耗时毫秒总和 × 1,000,000；字符请求不进入分子或分母，明确零输出仍有效。这是结算 Token 在整个请求耗时上的速度，含等待/会话停顿，不能称为纯解码速度或全是供应商实报。

独立无 TTL 的 `mv_output_rate_5min` 保存全部请求覆盖、已知单位数、Token 请求数、配对测时样本、输出和耗时。旧耗时 MV 不改写；查询在同粒度的新聚合和 raw 中择一，禁止重叠相加。只要历史覆盖或单位识别不完整，全范围速度返回 null，另提供已识别配对子集的速度、样本、耗时和单位覆盖，不能把未知单位当成 Token 或字符零。全部字符或有效 Token 样本总耗时为零时速度为 null。

`output_tps_history_complete` 仅表示请求历史覆盖完整，`output_tps_unit_complete` 表示全范围单位已知，两者独立；请求历史齐全但单位未知时不能返回全范围速度。测时样本覆盖以明确 Token 请求数为分母单独提供，缺失测时不计为零耗时。管理趋势、拆分/堆叠、用户/密钥行内用量、模型/渠道/时间线、个人按日/活动/图表与两类日志汇总使用此契约。

此设计已按 [输出速度单位核对](docs/output-rate-unit-audit.md) 实施；固定源码的八组关联 191 项及严格全目标检查通过。历史主 Token 校准、界面小时合桶与未知值显示、真实账单和大规模性能仍需后续验证，本段不表示整体目标已完成。

### 3.4.1 模型级降级的计费口径

`models.fallback_models` 允许"本模型无任何可用候选时改投另一个模型"。这引出一个必须先定死的问题：**按谁计费？**

定为**按实际服务的模型计费**，理由是反过来会算错钱：gpt-4o 降级到 gpt-4o-mini 后若仍按 4o 计价，用户为 mini 的输出付了 4o 的价（超收）；反向降级（mini 顶不住改投 4o）若按 mini 计价则站方倒贴（少收）。按实际模型计价两个方向都不会错。

配套三条约束：

1. **触发条件收窄**：仅当请求模型在当前池内**零可用候选**（渠道停用/冷却/超限/无 key）时降级。上游 4xx、用户参数错误、内容审查拒绝都不触发——换个模型同样会失败，只会把真实错误藏起来，让用户为两次调用付钱。
2. **单跳**：降级链只走一层，不递归。`a → b → c` 只尝试 a、b；否则一次请求可能横跨多个模型，账单与时延都不可预期。
3. **对客户端可见**：响应体 `model` 字段返回实际服务的模型（OpenAI 兼容语义本就如此），账单同时记 `requested_model` 与 `model`，用户能自己核对"我要的是 A、实际用了 B、按 B 计价"。

### 3.5 生态兼容

- **导入**：支持直接粘贴 new-api / one-api 的 ModelRatio / CompletionRatio / CreateCacheRatio / GroupRatio JSON，一键生成 model_pricing（老站迁移零成本；`create_cache_ratio` 键映射到本站 `cache_write_ratio`）。
- **导出/展示**：用户端价格页同时展示 倍率 与 $/1M 两列；`/api/pricing` 端点输出 new-api 兼容格式，便于聚合比价工具收录。
- **quota 视图**：用户余额展示可切换 USD / quota（×500,000），记账仍是 micro-USD。

### 3.6 结算来源池：订阅池优先（IMPLEMENTATION §11.28）

订阅套餐给用户一个按周期重置的**独立额度池**，与钱包并列。它**不参与定价**：§3.2 公式、
§3.4 规则栈、pricing_snapshot 对两个池完全一致，池只决定"这笔钱从哪扣"。选池发生在预扣
（reserve）那一刻：订阅池有余且在窗口内就走订阅池（允许最后一笔越界，避免估算偏大导致的
搁浅额度），否则走钱包（fail-closed）。同一请求的预扣 / 结算 / 退款都落同一个池，
`billing_records.pool` / `billing_events.pool` 记录归属，两池各自满足"热余额 + 在途 = 事件和"
的对账不变式。

---

## 4. 数据模型（定价域 v3）

> 本节为定价域示意；全量 DDL、索引与分区以 [docs/database.md](docs/database.md) 为准。

```
model_pricing                    -- 真理源：倍率制
  model_id            PK/FK
  pricing_mode        enum: ratio | per_call | tiered | media | time
  model_ratio         decimal(12,6)      -- 倍率 1.0 = $2/1M input
  completion_ratio    decimal(12,6)      -- 默认 1
  cache_ratio         decimal(6,4)       -- 缓存读取；默认 1（无缓存优惠）
  cache_write_ratio   decimal(6,4)       -- 缓存写入；默认 1（= 按常规输入计）
  audio_ratio         decimal(12,6)      -- 音频输入（相对文本；gpt-4o-audio = 16）
  audio_completion_ratio decimal(12,6)   -- 音频输出（叠乘在 audio_ratio 之上 = 2）
  image_ratio         decimal(12,6)      -- 图片输入（相对文本）
  per_call_price_micro bigint            -- micro-USD/次（per_call 模式）
  tier_expr           text               -- 阶梯表达式（tiered 模式）
  media_prices        jsonb              -- image/audio/video 单价
  effective_from      timestamptz        -- 支持定价生效时间（价格调整预告）

price_groups                     -- 新增：用户分组倍率
  group_code          PK（default/vip/svip/enterprise…）
  group_ratio         decimal(6,4)
  description, is_default

user_groups                      -- 新增：user ↔ price_groups 多对多
  user_id + group_code PK
  priority            int                -- 定价取优先级最高组；渠道可见性取并集
users.price_multiplier                    -- 保留（个人级微调）

user_pricing                     -- 保留：用户×模型专属（最高优先级）
  user_id + model_id  UK
  override_kind       enum: ratio | absolute
  custom_model_ratio / custom_completion_ratio        -- ratio 模式
  custom_input_price / custom_output_price（$/1M）    -- absolute 模式（落库时同步换算 ratio）
  reason, expires_at

pricing_rules                    -- 保留：修饰器栈
  rule_code PK, rule_type enum(volume|time_based|discount|surge)
  scope        jsonb   -- 作用域：全局/分组/模型/用户 选择器
  params       jsonb   -- {"threshold":1e6,"discount_rate":0.1} 等
  priority, enabled, valid_from/valid_to

pricing_epochs                   -- PriceBook 版本
  epoch bigserial, published_at, published_by, diff_summary jsonb

billing_records                  -- 已有表，新增
  + pricing_snapshot  jsonb
  + pricing_epoch     bigint
```

迁移：写一次性 converter，把现有 `models.input_price/output_price/cached_*` 换算为倍率三元组灌入 `model_pricing`；老列保留只读一个版本周期后下线。

---

## 5. 高并发计费统计架构（千万级/日，向亿级平滑扩展）

### 5.1 容量定位

| 量级 | 平均 QPS | 峰值 QPS（×8） | 结论 |
| --- | --- | --- | --- |
| 1000 万/天 | ~116 | ~1,000 | 单体/小集群即可，现架构裕量巨大 |
| 1 亿/天 | ~1,160 | ~9,000 | 现微服务架构 + 单 Redis 已接近上限（16k QPS） |
| 5 亿/天 | ~5,800 | ~46,000 | 需 Redis Cluster + gateway 水平扩容（ADR 已有方案） |

> ok-api 现有瓶颈公式：Redis 单节点 80k ops/s ÷ 每请求 5 次操作 ≈ 16k QPS。
> 本次重构把"每请求 5 次 Redis 操作"压到 2 次（见 5.2），单节点上限即提升到 ~40k QPS；
> 再配合 Redis Cluster 按 user_id hash-tag 分片，容量线性扩展。

### 5.2 同步路径（毫秒级，决定用户体验）

```
Client → api-gateway（内嵌 billing engine）
  1. 鉴权 + 限流 + PriceBook L1 查表          （进程内，0 IO）
  2. Lua-1 预扣：EVALSHA reserve(user, est)   （余额检查+预占+TPM 计数，原子）
  3. → proxy-service → 上游 LLM（SSE 透传）
  4. Lua-2 结算：EVALSHA commit(user, actual) （多退少补，原子；KPI 走同连接 pipeline）
  5. PG 事务：billing_records + billing_events + outbox（同事务）
```

变化点（相对现状）：

- 5 次散装 Redis 操作合并为 **2 个 Lua 脚本**（reserve / commit），减少 RTT 与竞态窗口；
- 定价解析从"每请求查规则表"变为"查 L1 编译价格表"，规则引擎脱离热路径；
- 余额 key 采用 `{user_id}` hash-tag，为 Redis Cluster 分片预留（单机模式无感）。

### 5.3 异步路径（1–3 秒新鲜度，承载千万级统计）

```mermaid
flowchart LR
    G[api-gateway<br/>billing engine] -->|"同事务 outbox"| PGDB[(PostgreSQL<br/>billing_events = 真理源)]
    PGDB -->|"SKIP LOCKED worker 重投"| N
    G -->|"billing.completed<br/>批量 publish 100ms 窗"| N[(NATS JetStream R=3)]
    N --> CHS[chsink 批写<br/>失败落盘 spill]
    N --> AUD[settle-audit 对账]
    N --> NOTI[notification 余额告警]
    CHS --> CH[(ClickHouse<br/>request_log_raw<br/>+ 6 张 AggregatingMergeTree MV)]
    ADM[admin dashboard] -->|"60s~10min 查询缓存<br/>singleflight"| CH
    ADM --> RK[(Redis KPI<br/>秒级实时计数)]
```

- 事件量 = 请求量（千万级/天 ≈ 峰值 ~1k msg/s），JetStream 与 CH async_insert 轻松承载；亿级时靠 chsink 批量参数（batch 5000 / flush 1s）与 CH 分片；
- 统计三档新鲜度（沿用 ADR）：Redis KPI 秒级 → CH MV 1-3s → CH 明细 ad-hoc（15s 超时护栏）；
- 对账三方不变：Redis 余额 ↔ PG 事件流 ↔ CH 汇总，reconciler 周期巡检。

### 5.4 微服务拓扑（沿用 7 服务，职责微调）

| 服务 | 变化 |
| --- | --- |
| api-gateway | 不变：内嵌 billing engine（同步路径低延迟的关键决策，保留）；新增 PriceBook L1 |
| billing-service | 继续跑 chsink / 对账 / DLQ / 计费 gRPC；**新增 PriceBook 编译器 + epoch 发布** |
| admin-service | 定价 CRUD 改为倍率制双视图；新增 new-api 配置导入/导出 |
| proxy/auth/user/notification | 不变 |

单体模式（monolith）继续保留同构入口，部署形态矩阵（BT 单机 → Compose 多机 → K8s）沿用 ADR 方案 A–D。

---

## 6. 实施路线

> 本节为**老 Go 仓库就地改造**的备选路线；Rust 全新实现的正式里程碑（M0–M4 与验收）以 [IMPLEMENTATION.md](IMPLEMENTATION.md) §13 为准。

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| P0 定价域 | model_pricing/price_groups/pricing_epoch 表 + 换算 converter + PriceBook 编译器 + L1 缓存 | 新老引擎双跑，pytest 计费 parity 套件（现有 `--scope p0`）分差为 0 |
| P1 规则栈 | pricing_rules 迁入修饰器栈 + pricing_snapshot 落库 | pricing_stacking 用例全绿；每笔账可回放 |
| P2 热路径 | reserve/commit Lua 合并、批量 publish、hash-tag 分片 key | 压测：单 Redis 峰值 QPS ≥ 2.5 倍现状 |
| P3 生态 | 倍率双视图 UI、new-api JSON 导入导出、/api/pricing 兼容端点 | 用 new-api 官方 ratio 配置一键导入成功 |
| P4 清理 | 老价格列下线、旧计费公式代码删除 | guard 脚本 + CI 拦截旧路径 |

回滚策略：P0–P1 期间新老引擎同进程双算（新引擎影子记账），对账差异告警为零后再切正式，随时可回退到老引擎。

---

## 7. 命名

推荐：**Okapi**（folder：`okapi`，Go module：`github.com/qiaojinxia/okapi`）

- ok-api 去掉连字符即是 okapi，品牌无缝延续，老用户零认知成本；
- okapi = 霍加狓，真实动物（长颈鹿科、腿部条纹），有现成吉祥物/logo 题材，"条纹"还暗合"倍率分层"的视觉隐喻；
- 简短、可读、可作 CLI 名（`okapi serve`）。
- 注意：FOLIO 项目（图书馆领域）有同名网关、Okapi Framework 是本地化工具，均为不同领域，冲突风险低；GitHub 仓库名 `qiaojinxia/okapi` 可用即可。

备选：

| 名字 | 理由 | 顾虑 |
| --- | --- | --- |
| Tollgate | 收费站 = 网关 + 计费双关 | 中文社区不直观 |
| RelayKit | 直白表达中转 | 平淡，one-api 系命名俗套 |
| 保持 ok-api | 零迁移成本，文件夹改回 `ok-api` | 放弃品牌升级机会 |

> 当前工作区文件夹为 `o-api`，确定名字后建议直接改名为 `okapi`（在 IDE 外执行 `mv ~/o-api ~/okapi` 后重新打开工作区）。

---

## 8. 后端技术栈选型：Rust 方案

### 8.1 结论：适合，但要认清买到什么、付出什么

**2026 年的生产先例已充分**：TensorZero（Rust + axum + ClickHouse，与本项目架构选型几乎同构，实测 10k QPS 下网关自身 P99 开销 <1ms）、Helicone AI Gateway（Rust + axum，约 15MB 二进制 / 64MB 内存跑 3k RPS）、Cloudflare Pingora。Rust 做 LLM 网关已过验证期，不是冒险。

**买到的**：

| 收益 | 说明 |
| --- | --- |
| 长连接密度 | SSE 流式代理是本项目主要资源形态。无 GC、每连接内存小，单节点可稳定持有 10 万级并发 SSE（现 Go proxy HPA max=20 全集群才 15-25k in-flight） |
| 尾延迟 | 无 GC 停顿，网关自身开销 P99 可压到 1-2ms 内，SSE 首字节更稳 |
| 计费正确性 | i64 micro-USD newtype + enum 状态机在**编译期**封死"裸 float 进计费路径""非法状态转移"——现 Go 仓库靠 guard 脚本 + CI 拦的问题变成编译错误 |
| 部署足迹 | 静态单二进制 ~20MB，嵌前端后仍 <50MB，比 7 个 Go 镜像 + Consul 轻一个量级 |
| 产品差异化 | new-api 系全是 Go；"Rust 高性能网关"在中转站圈是可感知的卖点 |

**付出的**：

| 成本 | 缓解 |
| --- | --- |
| 前期开发速度约为 Go 的 1/2–2/3（async Rust 曲线、tower 中间件泛型） | 范围收敛：M1 只做 chat completions 最小闭环 |
| provider 适配器要自己写（Go 有 one-api 系海量参考实现可抄） | 参考 async-openai / TensorZero 适配层；逻辑照抄老仓库 Go 实现 |
| 145 commits 的 Go 代码不能直接复用 | **黑盒资产全部保留**：pytest parity 套件、SQL migration、CH schema、Lua 脚本、编排文件，全部语言无关，直接做新实现的验收标准 |
| 协作者门槛更高 | 单人/小团队影响有限 |

**判断标准**：目标是"几周内在老仓库上线倍率计费"→ 留 Go，直接实施 §1–6；全新仓库、把性能作为产品卖点、接受 1.5–2 倍前期投入 → Rust 值得。**不建议 Go/Rust 混合**（双工具链的运维与心智成本超过收益，Rust 完全胜任控制面 CRUD）。

### 8.2 服务拓扑：7 微服务 → 3 角色

Rust 单节点能力强，不再需要按功能拆小服务来腾资源；拆分维度改为**故障域 + 发布节奏**：

| 角色 | 职责 | 吸收原服务 | 扩缩容 |
| --- | --- | --- | --- |
| **gateway**（数据面） | 鉴权、限流、预扣/结算、路由、SSE 透传、KPI 计数 | api-gateway + proxy-service | 按连接数/CPU HPA，无状态 |
| **console**（控制面） | 管理后台 + 用户门户 API、JWT/OAuth、定价 CRUD、PriceBook 编译发布 | admin + auth + user-service | 2 副本足够 |
| **worker**（异步面） | chsink→CH、outbox relay、DLQ、对账 reconciler、通知 | billing + notification-service | 按 JetStream consumer 分区 |

关键简化：

- **热路径零跨服务调用**：鉴权走 Redis 缓存 + 本地 JWT 验签，定价走进程内 PriceBook，gateway 每请求只碰 Redis 和上游 LLM；
- 服务间无常驻 gRPC 依赖（控制面 → 数据面通过 PG + NATS epoch 广播通信），**Consul 直接砍掉**（K8s DNS / 静态配置）；
- **单二进制多角色**：`okapi all`（单机模式，对齐现 monolith 哲学）/ `okapi gateway|console|worker`（分布式），部署形态矩阵沿用 ADR 方案 A–D。

```mermaid
flowchart LR
    C[Client] -->|OpenAI 兼容| GW[gateway ×N<br/>axum, 内嵌 pricing+ledger]
    GW -->|Lua reserve/commit| R[(Redis Cluster<br/>hash-tag user)]
    GW -->|SSE 透传| U[[上游 LLM]]
    GW -->|同事务 outbox| PG[(PostgreSQL<br/>billing_events 真理源)]
    GW -->|billing.completed| N[(NATS JetStream)]
    N --> W[worker ×M<br/>chsink/DLQ/对账/通知]
    W --> CH[(ClickHouse MV)]
    PG -->|SKIP LOCKED 重投| N
    CON[console ×2<br/>admin+portal API] --> PG
    CON -->|epoch 广播| N
    N -.->|PriceBook 失效| GW
    CON -->|15s 护栏| CH
    FE[React SPA<br/>rust-embed 内嵌] --- CON
```

### 8.3 Cargo workspace 布局

```
okapi/
├── crates/
│   ├── okapi-domain     # money（i64 microUSD newtype，禁 float）/ tokens / 计费状态机（enum + 穷举转移）
│   ├── okapi-pricing    # PriceBook 编译器 + ArcSwap L1 缓存 + epoch 订阅
│   ├── okapi-ledger     # Redis Lua 预扣/结算 + PG 事件溯源 append + outbox（对应现 billing-core Pipeline）
│   ├── okapi-providers  # Provider trait + openai/claude/gemini/deepseek SSE 适配器
│   ├── okapi-store      # sqlx(PG) / fred(Redis) / clickhouse / async-nats 薄封装
│   └── okapi-api        # OpenAI 兼容 DTO（serde）+ utoipa OpenAPI 文档
├── bins/okapi           # 单二进制多角色入口（clap 子命令）
└── frontend/            # React SPA，构建产物 rust-embed 进二进制
```

### 8.4 crate 选型

| 用途 | 选型 | 理由 |
| --- | --- | --- |
| HTTP 服务 | axum + tower + hyper 1.x（rustls） | TensorZero / Helicone 同款，中间件生态最全 |
| 上游客户端 | reqwest 流式（HTTP/2 连接池） | SSE 透传、超时/重试分层控制 |
| Redis | fred | cluster / pipeline / Lua 脚本缓存支持最好 |
| PostgreSQL | sqlx | 编译期 SQL 校验，契合现有 fail-fast 文化 |
| ClickHouse | clickhouse crate | RowBinary 批写 + async_insert |
| 消息 | async-nats | 官方维护，JetStream 完整支持 |
| token 计数 | tiktoken-rs | — |
| 配置热切换 | arc-swap（PriceBook）+ moka（TTL 缓存） | 读路径无锁 |
| 限流 | governor（本地兜底）+ Redis GCRA（全局） | 对齐现有两级限流 |
| 可观测 | tracing + OTLP + metrics-exporter-prometheus | 对齐现有 Jaeger/Prom |
| 金额 | 自研 i64 micro-USD newtype；rust_decimal 仅展示层 | 计费路径禁浮点，编译期保证 |

### 8.5 分布式关键设计（语言无关部分沿用 §5）

- **优雅下线**：SIGTERM → 停接新请求 → 在途 SSE 排水（上限 5min）→ flush CH/PG 批写 → 退出；
- **PriceBook 热更新**：console 发布 epoch → NATS 广播 → gateway ArcSwap 原子替换指针，读路径零锁零 IO；
- **背压**：JetStream max_ack_pending + chsink 失败 spill 落盘重放（沿用现设计）；
- **演进路径**：单机 `okapi all` + compose → 上量后 gateway 先独立水平扩 → console/worker 再拆，任一阶段不改代码只改部署。

### 8.6 Rust 实施里程碑（验收标准与 §6 相同）

> 已在 [IMPLEMENTATION.md](IMPLEMENTATION.md) §13 展开为 M0–M4 并细化验收，以该文档为准。

| 里程碑 | 内容 | 验收 |
| --- | --- | --- |
| M0 | okapi-domain + okapi-pricing 纯逻辑 crate | property test + 与 new-api 公式对拍（同输入同输出） |
| M1 | gateway 最小闭环：/v1/chat/completions（流式+非流式）+ API key 鉴权 + 预扣/结算 + PG 记账 | 老仓库 pytest p0 parity 套件直接打新服务，全绿 |
| M2 | worker（outbox/chsink/DLQ/对账）+ console 定价 CRUD + PriceBook 发布 | p1 套件 + 三方对账零差异 |
| M3 | 多 provider / image / audio / embeddings + 前端门户 | 全量 pytest + 压测报告（对标 §5.1 容量表） |

---

## 9. 前端设计

### 9.1 调研：中转站都在用什么 UI（2026-08 实测各仓库依赖）

| 流派 | 代表 | 星数 / 活跃度 | 实际 UI 栈（读自 web/package.json） |
| --- | --- | --- | --- |
| **new-api 官方新版**（行业默认） | QuantumNous/new-api | 46.7k，日更 | React + **Tailwind + shadcn 系**（Base UI、cva、cmdk、sonner、vaul、lucide）+ TanStack Router/Query/Table/Virtual + Recharts/VChart + @lobehub/icons + 多主题系统（default / classic / zr 科技风） |
| 老版保守魔改 | Veloera/Veloera | 1.6k | Semi Design（沿用旧版 new-api UI + 自定义 semi 主题），自称"原汁原味 New API 体验" |
| one-hub 系 | MartialBE/one-hub 2.9k、deanxv/done-hub 0.8k | done-hub 活跃 | MUI（Material UI）Berry 后台模板风 |
| 闭源高颜值 | VoAPI/VoAPI | 1.1k | **闭源**（仓库只有 docker 编排 + main.go），社区公认"最好看"，实时 RPM/TPM 看板 + 自定义 SEO/主题色/全局样式，Pro 商业版 |
| 周边工具 | tbphp/gpt-load | 6.3k，活跃 | Vue 3 + Naive UI |
| 社区自制美化 | openclaw-new-ui 等 | — | Next.js + Tailwind + shadcn + 玻璃拟态（Glassmorphism） |

三条结论：

1. **行业审美的默认基准就是 new-api**，而 new-api 官方已经自己完成了 Semi Design → Tailwind + shadcn 的整体迁移，并把主题系统做成一等公民（zr 主题主打发光渐变、网格背景、玻璃态卡片的"科技 AI 风"）。"组件库默认风 → Tailwind 定制现代风"是全行业明确趋势，Semi/MUI 流派属于存量。
2. **魔改站"看起来不错"的三要素**可以归纳为：暗色科技感主题、实时图表看板（RPM/TPM/趋势）、模型厂商图标墙 + 公开价格/测速页。VoAPI 正是靠这三件套 + 精细统计做出溢价（且闭源收费）。
3. 本设计 §9.2 的选型与行业收敛点一致（等于和 new-api 新版同流派，用户零适应成本），在此基础上补齐三件套即可形成"开源里最好看"的定位：@lobehub/icons、多主题 + 站长自定义主色/SEO、实时 RPM/TPM 看板（数据源 §5 的 Redis KPI 已具备，别人要额外做，我们是白送）。

### 9.2 技术栈

| 层 | 选型 | 说明 |
| --- | --- | --- |
| 框架 | Vite + React 19 + TypeScript | 纯 SPA（自托管后台无 SSR 需求），沿用现有技术栈心智 |
| 路由/数据 | TanStack Router + TanStack Query | 类型安全路由；服务端状态缓存、自动重试 |
| UI | Tailwind CSS v4 + shadcn/ui | 现代后台风格、暗色模式默认、深度可定制 |
| 表格 | TanStack Table + 虚拟滚动 | 用量日志/账单等数据密集场景 |
| 图表 | Recharts（复杂图用 VChart） | shadcn 生态默认搭配，new-api 新版同款 |
| 模型图标 | @lobehub/icons | LLM 厂商/模型图标标准库，价格页/渠道页的视觉基础 |
| 主题 | next-themes + CSS 变量多主题 | 亮/暗 + 科技风主题；站长可自定义主色/Logo/SEO（对标 VoAPI 卖点） |
| 国际化 | i18next（中/英） | 中转站用户群双语 |
| 部署 | 构建产物经 rust-embed 嵌入 okapi 二进制 | 单文件部署（new-api 同款运维体验） |

备选已排除：Semi Design（Veloera 等老版流派沿用，new-api 官方已迁出）、MUI Berry（one-hub/done-hub 系，后台模板感重）、Ant Design Pro（与国产后台同质化严重）。

### 9.3 信息架构（用户门户 + 管理后台同一 SPA，按角色分区）

**用户侧 `/console`**：

| 页面 | 要点 |
| --- | --- |
| 概览 | 余额（USD/quota 双显）、今日消耗、7/30 天费用趋势面积图、Top 模型 |
| API Keys | 创建/限额/过期/分组绑定、用量 sparkline、一键复制 |
| 模型价格 | **倍率 + $/1M 双列**、按分组切换视角、搜索/能力标签 |
| 用量日志 | 明细表（模型/tokens/费用/耗时/状态）+ 筛选导出，行展开显示账单解释 |
| 充值/账单 | 在线充值、额度流水 |
| Playground | 聊天测试台（可选，提升粘性） |

**管理侧 `/admin`**：

| 页面 | 要点 |
| --- | --- |
| 运营仪表盘 | 实时 QPS/在途 SSE（5s 轮询 Redis KPI）、收入/成本/毛利、错误率、渠道健康红绿灯 |
| 渠道管理 | 上游 key 池、权重/优先级、熔断状态、一键测活、上游余额抓取 |
| 模型 & 定价 | 倍率三元组编辑器（**倍率 ↔ $/1M 双视图实时换算**）、new-api JSON 导入向导、定价生效时间、epoch 版本历史 diff |
| 用户 & 分组 | price_groups 分组倍率、用户余额/分组/multiplier、子账户 |
| 计费规则 | 修饰器栈可视化（拖拽排 priority）+ 规则命中模拟器 |
| 计费记录/对账 | pricing_snapshot 展开、DLQ 列表与 requeue、三方对账差异报表 |
| 系统设置 | 站点/SMTP/OAuth/默认限流 |

### 9.4 三个差异化界面（相对 new-api 的体验优势）

1. **账单解释器**：任意一笔计费记录展开为逐步算式（基准价 × 模型倍率 × 补全倍率 × 分组倍率 × 规则链 = 最终价），数据来自 pricing_snapshot，"每一分钱可解释"；
2. **定价模拟器**：管理员改倍率/规则前，输入 user + model + token 量即时预览新旧账单差异，防改错价；
3. **公开价格页**：`/pricing` 免登录页 + new-api 兼容 JSON 端点，方便聚合比价工具收录（中转站获客习惯）。

### 审计修复约定

- 时段折扣的分钟窗与星期均使用运行机器的本地时区；有效期仍比较真实 UTC Unix 秒。请求准入时固定价簿、时钟偏移和规则输入，Realtime 最长 480 秒。
- 用户变体专属价优先于基座专属价；变体未设专属价时继承基座协议价，最后使用公开变体价。绝对协议价继承时仍独立于站点基准价。
- `/pass` 与视频只能使用 `per_call`；视频创建成功先收费，上游最终失败或取消全额退款。任务映射与原始账单同一 PG 事务，worker 不依赖用户主动轮询。
- PG 结算写入前保存 Redis 重试日志，PG 恢复后按 request_id 幂等补写；Redis 需按账本要求持久化。到期扣除通过 PG 事件与 fund_transfers 原子接受后应用 Redis。
- 统计日界跟随机器时区，PG 连接显式设置同一时区；ClickHouse 事实时间始终声明 UTC，查询用小时事实重建本地日期，不直接把历史 UTC 日桶改名为本地日桶。
