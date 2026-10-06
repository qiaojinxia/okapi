# 门户范围、首字延迟与缓存采集

## 登录与统计范围

- 邮箱注册/登录、OAuth 登录使用账户视角，门户总览、日志、个人页均查询 `scope=user`。
- API Key 登录默认查询 `scope=key`，显示当前密钥名称与展示前缀，可切换到全账户。选择在这些页面间沿用；显式 URL 参数优先。
- `okapi.login-mode`、`okapi.usage-scope` 仅是展示偏好，不参与鉴权。切换登录凭证或退出时清理范围偏好。
- 旧浏览器未保存登录方式时，`/api/me` 的 `has_web_session` 只在有效网页会话与 Bearer 所属用户一致时为 true。返回的 `key_name`、`key_prefix` 不包含密钥明文。
- 本次未变更网页会话兑换 API Key 的认证架构。

## 首字延迟（TTFT）

平均首字延迟 = 已记录首字耗时之和 / 首字样本数，单位 ms。门户汇总增加 `ttft_samples`。没有样本返回 null；历史缺少性能记录不抹去已有的首字样本。请求总时延仍单独统计，不把非流式或未采集请求当作 0 ms 首字样本。

## 缓存

`TokenUsage.cache_read_reported` 与 `cache_write_reported` 表示上游是否明确上报对应字段；零值可以是已上报，缺失和 null 则不是。OpenAI 兼容、Responses、Anthropic（包括两种流式路径）、Gemini 的解析和协议转换保留这个区别，估算用量不会伪造缓存采集状态。

OpenAI 兼容响应统一接受写入字段 `cache_write_tokens`、`cache_creation_input_tokens`、`created_cache_tokens`、`cached_creation_tokens`、`cache_creation_tokens`、`cache_write_input_tokens`，以及读取字段 `cached_tokens`、`cache_read_input_tokens`、`cache_read_tokens`、`prompt_cache_hit_tokens`。字段可以位于 usage 顶层、Chat 的 `prompt_tokens_details` 或 Responses 的 `input_tokens_details`。它们都是输入总量的子项，不重复加到输入总量。重复字段必须数值一致，冲突或非法值拒绝结算估算。千问返回的 `cache_creation.ephemeral_5m_input_tokens`（及兼容的 1 小时明细）和旧桥接的 `claude_cache_creation_5_m_tokens` / `claude_cache_creation_1_h_tokens` 保留为缓存写入时长细分，细分必须与写入总量一致；未返回时长明细则保留未知。旧桥接在没有写入总量时默认输出的两个零值，不视为已采集写入。流式按累计快照更新，新写入总量不继承过期的时长细分。

字段依据：[千问 Chat API](https://help.aliyun.com/en/model-studio/qwen-api-via-openai-chat-completions)、[vLLM 用量协议](https://docs.vllm.ai/en/stable/api/vllm/entrypoints/serve/engine/protocol/)。这些映射识别上游实际返回的写入数据，不根据缓存未命中推测写入，也不补造历史记录。

### 读取与桥接字段兼容

| 上游返回格式 | 读取 | 写入 | 输入总量口径 |
| --- | --- | --- | --- |
| OpenAI 兼容 Chat / Responses | 上述读取别名；Chat 额外接受 Kimi `choices[].usage.cached_tokens`、llama.cpp `timings.cache_n` | 上述写入别名与时长明细 | 标准输入总量已含缓存，不再相加 |
| 豆包音频明细 | `cached_tokens` 是全部缓存；`audio_cached_tokens` 仅为其中的音频子项 | 使用明确上报的写入字段 | 音频缓存从原始音频输入中分离，不额外加到缓存或输入总量 |
| Anthropic Messages | 原生字段与上述读取别名 | 原生字段、上述写入别名与时长细分 | 原生 `input_tokens` 不含缓存；带 `prompt_tokens` 的混合桥接先校验含缓存总量，再拆分，避免重复加缓存 |
| Gemini GenerateContent | `cachedContentTokenCount` 与 `cacheTokensDetails` | 协议未提供请求级写入量时保持未知 | `promptTokenCount` 已含读取缓存 |
| 桥接保留的 Bedrock Converse 用量 | `cacheReadInputTokens` | `cacheWriteInputTokens`；`cacheDetails` 的 `5m` / `1h` | 原始 `inputTokens` 不含读写缓存，归一化时加一次 |
| 桥接保留的 Gemini Interactions 用量 | `total_cached_tokens` 与 `cached_tokens_by_modality` | 没有明确写入字段时保持未知 | `total_input_tokens` 已含缓存；输出按 `total_output_tokens + total_thought_tokens` 归一化 |

DeepSeek 的命中与未命中之和必须等于输入总量；两者完整上报时，可在缺少 `prompt_tokens` 的流式快照中保留这项已观察总量。只有未命中字段时，不推算读取或写入。豆包的音频缓存子项须与标准音频缓存明细一致，并且不能超过音频输入或缓存总量。

Kimi 的多条 choice 缓存计数按同一请求的镜像校验，不相加；它们也必须与顶层及标准明细一致。仅带缓存计数的流式 chunk 保留此前已上报的输入/输出轴，明确零值可以替换此前的缓存计数。Messages 混合桥接的 `input_tokens` 必须与普通输入或含缓存总量之一一致；其余值视为冲突。

兼容缺口对照：[New API 缓存提取](https://github.com/QuantumNous/new-api/blob/1a4166d8e8ba9802d2ca56fe8ecf0ed5404e80d5/relay/channel/openai/usage.go)、[New API 用量 DTO](https://github.com/QuantumNous/new-api/blob/1a4166d8e8ba9802d2ca56fe8ecf0ed5404e80d5/relaykit/dto/openai_response.go)、[Sub2API OpenAI 用量提取](https://github.com/Wei-Shaw/sub2api/blob/3040209f205472038c1ba745a1bedd2edd9053b1/backend/internal/service/openai_gateway_response_handling.go)、[Sub2API Messages 归一化](https://github.com/Wei-Shaw/sub2api/blob/3040209f205472038c1ba745a1bedd2edd9053b1/backend/internal/service/gateway_anthropic_passthrough.go)。这些字段映射不采用按成本反推缓存写入或强制改写缓存账单的策略。

桥接兼容覆盖现有 Chat、Responses、Messages 和 GenerateContent 调用路径的 JSON 与流式响应；不增加原生 Converse 或 Interactions 的请求转换与传输入口。桥接可以返回完整原始用量对象，也可以返回标准归一化总量并附带原始缓存字段；后一种形式不重复加缓存。空值不遮盖其他有效字段，重复计数必须一致；非法类型、溢出、时长或模态细分冲突会拒绝按该用量结算，不降级为估算扣费。

字段依据：[DeepSeek Chat API](https://api-docs.deepseek.com/api/create-chat-completion)、[豆包 Chat API](https://docs.volcengine.com/docs/ark/chat-api?lang=zh&redirect=1)、[Bedrock 缓存计数口径](https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html)、[Bedrock CacheDetail](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_CacheDetail.html)、[Gemini Interactions API](https://ai.google.dev/api/interactions-api)。兼容测试使用这些协议的模拟响应，验证解析、流式、日志、统计、余额和结算 outbox；不代表所有模型、账户或桥接服务已做真实调用验证。上游未返回字段时，仍无法补造采集数据。

状态通过结算 outbox 传到 ClickHouse。新增 nullable 状态列、`mv_cache_reporting_day` 和 `mv_cache_reporting_hour`，与既有金额和用量视图并存。启动时增量创建，不删除、不重算、不回填历史。

- 命中率 = 缓存读取 Token 总数 / 输入 Token 总数，按 Token 加权，不是请求命中比例。
- 有输入且全部请求明确上报读取用量时返回 `cache_hit_bp`，否则为 null；`cache_read_known_requests` 展示覆盖数。
- 缓存写入同样通过 `cache_write_known_requests` 区分缺失和已知零值。覆盖不完整时 `cache_write_tokens` 为 null。
- 门户总览、模型构成、个人页，以及管理分析、总览、日志统计保留未知值，不转成 0%。构成图的“其他输入”可能包含未细分的缓存，不等于确定的非缓存输入。
- 旧版本生成的记录即使已有数值 0，也无法可靠判断是零命中还是字段缺失，因此不自动补报采集状态。

采集兼容不修改已配置的模型倍率；补齐上报字段后，新请求按原配置正确分项结算，历史账单不重算。Token 计费的缓存读取、写入分别使用该模型的配置倍率；1× 与普通输入同价。完整上报 5 分钟 / 1 小时明细且配置对应倍率时，使用 `cache_write_5m` / `cache_write_1h` 分项价格；缺少明细或对应价格时，使用通用缓存写入倍率。不包含独立缓存存储服务费用。按次计费不随缓存用量变化。“计费优惠”展示规则折扣，不是缓存节省估算。

## 用户用量日志

- `/api/me/logs` 和 `/api/me/logs/stat` 共用用户、范围、模型、密钥、请求 ID、失败状态及日期条件。统计接口忽略 `before` / `limit`，不从前端已加载行求和。日期沿用自然日和 IANA 时区语义。
- 全部查询强制限定认证用户；密钥范围始终额外限定当前密钥，指定其他密钥不能绕过限制。只返回公开用量与计价字段，不返回渠道凭据、成本或内部节点。
- 顶部“记录数”是匹配的账本记录数，不是全部入口请求数；未落账的拒绝不在其中，因此不展示未经验证的全站请求成功率。
- 状态 `10/20/30/40` 分别为待结算、已结算、已退款、失败。`errors_only` 只匹配 `40`。已退款记录保留原金额与 Token 事实，`net_amount_micro=0`；实际消费汇总仅累计 `20` 状态金额。
- 首字均值仅纳入有有效 TTFT 的流式已结算/已退款记录；总耗时均值独立使用有效总耗时样本。无样本返回 null，不显示 0 ms。
- 迁移 `0015_billing_usage_details` 增加可空 `billing_records.usage_details`。新结算在同一 INSERT/事务保存规范化 Token 用量、请求模型和调用接口；不保存提示词、回答正文或凭据。旧记录保持 NULL，不回填推测值。
- 单条记录明确区分缓存上报为零、未上报和历史未记录；历史已有正数读取量仍可展示。汇总缓存读取同时显示覆盖条数，不把部分覆盖误称为完整命中率。
- 详情使用请求当时的 `pricing_snapshot`，不查询当前目录价格。按 Token 分项金额仅为基于已保存单价的展示参考，含原分组/优惠，不重复应用倍率；单价舍入可能与整笔记账不同。缺少完整快照不重建历史分项。按次记录展示原基础价、媒体数量和规则，不伪造 Token 单价。
- CSV 明确标为“导出已加载 CSV”，仅导出已加载记录，包含账务状态、净消费、原扣费、退款、缓存采集标记和错误码；未知缓存留空，非流式 TTFT 留空。不是全量报表导出。
