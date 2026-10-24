# 价目目录建议（内置 LiteLLM 社区价目表）

新模型上线时，价格、缓存倍率、上下文上限、能力标签原本都要管理员对着厂商文档手工录入（前端只有 32 个人工核对过的预设）。
内置一份 [LiteLLM](https://github.com/BerriAI/litellm) 社区维护的价目表快照，在模型表单里**按名字带出建议**，让管理员确认。

## 边界：它是输入辅助，不是价簿

- 建议只在管理员点击「填入建议价」「带入规格」之后才进入**表单草稿**，照常保存、发布；发布后的价簿快照才是计费依据，
  账单可回放的语义不变。目录没有、目录写错，都不影响任何一笔计费。
- 目录不参与请求时的价格解析（不像 Sub2API 的 Group → Channel → LiteLLM → Fallback 四级链）：请求路径上没有目录。
- 目录只含**标准档**价格。长上下文加价、batch / flex / priority 档、推理与音频 token 另价、按秒计价只作为备注列出，不自动配置。

## 数据

- 来源：`BerriAI/litellm` 的 `model_prices_and_context_window.json`，MIT 许可，原文见 `crates/okapi-pricing/data/LICENSE-litellm.txt`。
- 快照：`crates/okapi-pricing/data/litellm-catalog.json`，编译进二进制（`include_str!`）。头部 `meta` 记录上游 commit、提交时间、生成日期、条数；
  接口响应里带 `snapshot`，建议卡上显示「快照 日期」。
- 裁剪（`scripts/refresh-pricing-catalog.py`）：只保留 Okapi 能用上的 mode（chat / responses / completion / embedding / rerank / 图像 / 音频 / 实时 /
  视频 / 审核 / 搜索），丢掉按区域复制的条目（`bedrock/us-east-1/…`、`azure/eu/…`），每条只留建议逻辑读的字段；数字原样写成十进制文本，
  价格不经过浮点。约 3900 条、1 MB。

### 刷新

```bash
python3 scripts/refresh-pricing-catalog.py                 # 取上游最新 commit
python3 scripts/refresh-pricing-catalog.py --source FILE   # 用本地副本（离线）
cargo test -p okapi-pricing catalog                        # 解析 + 几个有人工核对依据的模型
```

快照里出现代码不认识的字段、负价格、条数不足 1500，解析或脚本都会直接失败，不会半截载入。刷新后看 `git diff --stat`，再抽查几个常用模型。

## 匹配规则：只认同一个名字

大小写、`@` 与 `-`（Vertex 的 `claude-3-5-sonnet@20240620` 与 API 的 `…-20240620` 是同一快照）、`厂商/` 前缀不计较；其余一概不猜：
没有按前缀推断，也没有「最接近的版本」（`gpt-4o-2024-08-06` 查不到就是查不到）。与前端预设的原则一致。

同名条目分散在多个厂商 / 云上时，取第一方（OpenAI、Anthropic、Google AI Studio、DeepSeek、xAI、Mistral、Moonshot、阿里、字节、智谱……）的那条，
再是云厂商（Vertex、Azure、Bedrock），其余最后；同级取不带路径前缀、更短的。其余同名条目作为「备选」列在建议里。

## 换算

整数十进制（目录数字最多 18 位小数），无浮点：

- `input_per_1m` / `output_per_1m`：USD / token → USD / 百万 token，最多 6 位小数，四舍五入；
- `cache_read_ratio` / `cache_write_ratio` / `cache_write_1h_ratio`：缓存价格相对**输入价**的倍率，6 位小数（DeepSeek 的 1/30 取 `0.033333`，与价簿倍率精度一致）；
- 嵌入 / 重排 / 审核的输出价为 0 时不给输出价（免得表单把输出价填成 0）；按次计价（每张图 / 每次查询）给 `per_call_usd`；按秒等 Okapi 现有计价方式表达不了的，`billing = unsupported`，只给备注。

## 接口

| 路径 | 权限 | 说明 |
| --- | --- | --- |
| `GET /admin/pricing/catalog/lookup?model=<名字>` | `pricing.read` | `{ "suggestion": {…} \| null, "snapshot": {…} }`；名字为空 / 超过 128 字符 400 |
| `GET /admin/pricing/catalog/search?q=<关键词>&limit=<n>` | `pricing.read` | `{ "data": [{ "id", "provider", "mode", "kind", "input_per_1m", "output_per_1m", "listings" }] }`，每个 id 一条（取第一方那条），`limit` 夹在 1–50，空关键词不联想 |

`suggestion` 的字段：`key`、`provider`、`matched`（`exact` / `prefixed` / `bare`）、`mode`、`kind`（Okapi 模型类型）、`billing`（`ratio` / `per_call` / `unsupported`）、
价格与倍率、`context_window`（目录的最大输入）、`max_output`、输入输出模态、`capabilities`（只含目录明确声明的，没声明的不当成 false）、
`notes`（`long_context` / `service_tiers` / `reasoning_price` / `audio_price` / `per_second` / `free` / `no_price`，带参数）、`deprecated`、`source_url`、`alternatives`。

## 界面（模型定价抽屉）

- 名称停顿后按完整名字查目录：命中就出**建议卡**——价格、缓存倍率、输入输出上限、能力、备注、弃用提醒、同名备选，以及数据来源链接与快照日期。
- 「填入建议价」只改价格（按倍率计价时是输入 / 输出价，按次计价时是按次价）和目录给了的缓存读写 / 1 小时倍率；嵌入没有输出价时保留管理员原来的输出价；阶梯计价下不能硬套。
- 「带入规格」只补**空项**（类型、模态、能力、上下文 / 输出上限）：管理员已经填的不动，未知模态和超出整数范围的限制丢弃。
- **校验基准**：填的输入 / 输出价与参考价相差一倍以上或不到一半时提醒（不拦保存）。参考价取预设的人工核对价，没有预设才取目录价。加价、折扣属正常，要拦的是小数点和单位错位。
- 选了预设就不再问目录（预设是人工核对过的，优先）；目录没有这个名字、查询失败时有说明，不影响手动配置。
- 名称联想：预设（人工核对过）排在前面，目录命中排在后面，带厂商与价格；同一个 ID 两边都有时只留预设那一条。

新建渠道时「拉取上游模型」拉回来的名字，会提示其中哪些还没在价簿里建模型（请求会因未定价被拒），并链到模型定价页补上——补价时就用上面的建议卡。
