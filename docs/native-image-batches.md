# 原生图片批处理：协议层与接入边界

`okapi-providers::batch` 提供 Gemini/Vertex/GCS 协议层，并已接入默认关闭的 `/v1/images/batches`、后台执行器、[长期冻结账本](durable-balance-holds.md) 和 [批任务持久状态](native-image-batch-jobs.md)。受控 Gemini、Vertex/GCS HTTP 上游加真实 PG/Redis 已验证提交、结果收集、未知创建找回和结算链路；删除/到期回收已接入，真实供应商联调仍有缺口。现有异步图片 API 继续使用 [图片契约](images-contract.md)，不会自动转为供应商 Batch。

## 已实现的协议

| 范围 | 当前实现 | 重要语义 |
| --- | --- | --- |
| Gemini 内联提交 | `models/{model}:batchGenerateContent`，保留原生 GenerateContent JSON 与 `metadata.key` | 每条请求必须有唯一 key；发送前检查序列化容量；不自动重发创建 |
| Gemini 文件提交 | 可预先指定名称的 Files resumable upload，查询文件到 ACTIVE，再以 `inputConfig.fileName` 提交 | 上传与创建是分开的步骤；调用方需要分别保存意图和清理依据 |
| Gemini 状态/输出 | REST Operation 的 `metadata.state`、`metadata.output`、`done`、`response/error`，以及直接 `state/dest` 形状 | 拒绝未知状态、身份不符和互相矛盾的终态；成功状态必须携带结果位置；取消后的 metadata 部分结果仍交给业务层逐项核对 |
| 原提交查找 | Gemini `batches` 列表的 `operations`；Vertex `batchPredictionJobs` 精确 displayName 过滤 | 每页最多 100 条；固定账号、模型和输入；列表只是线索，完整分页和新鲜详情一致才可接管 |
| Gemini 取消/删除 | cancel、删除 operation、删除文件、下载结果 JSONL | cancel 只确认收到请求；后续可能成功，仍需轮询。删除 operation 不代表取消或文件清理 |
| Vertex | `batchPredictionJobs` 创建、查询、取消、异步删除及 operation 查询；原生模型名、GCS 输入/输出和 `instanceConfig.keyField` | 项目/location 固定；结果目录必须在当前任务的 GCS 前缀内；保留部分成功状态；删除后独立确认 job 404 |
| GCS | 条件创建输入、读取 metadata、按返回目录分页列举结果、按 generation 下载和删除 | `ifGenerationMatch=0` 防覆盖；412 后核验现存版本内容；收集不能跨到返回目录的相邻目录 |
| JSONL | 增量读取、单行/总字节/条数限制、最终非换行行、CRLF | 截断或错误后不能当作完整 EOF；读到的行不等于已经完成业务校验或结算 |

协议依据：[Gemini Batch](https://ai.google.dev/api/batch-api)、[Gemini Files](https://ai.google.dev/api/files)、[Vertex BatchPredictionJob](https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/rest/v1/projects.locations.batchPredictionJobs)、[GCS 条件上传](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert)。竞品对照固定 Sub2API `a3eb7ef302961cba716dc78b39b93b60c467db0e` 的 [Gemini provider](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/batch_image_provider_gemini.go) 与 [Vertex provider](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/service/batch_image_provider_vertex.go)。没有运行竞品或据此推断性能优势。

恢复协议补充核对 [Google 官方 Python SDK batches](https://raw.githubusercontent.com/googleapis/python-genai/main/google/genai/batches.py)：Developer API 列表使用 operations，SDK 不支持该 API 的 filter，返回值映射包含 metadata.output。Vertex 列表按[官方 list 文档](https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/rest/v1/projects.locations.batchPredictionJobs/list)使用 displayName 精确过滤和 nextPageToken。身份验证还要求详情回显固定输入；这项云端实际行为不能仅凭 SDK 或模拟上游认定已验收。

Vertex [官方 Cloud Storage 批推理输出示例](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/capabilities/batch-inference/new-job-from-cloud-storage#retrieve-batch-output)中，成功条目使用 response，失败条目可同时有非空 status 和空 response 对象。解析器保留这项合法组合；错误与实际非空结果并存仍拒绝。key 关联采用 BatchPredictionJob 的 instanceConfig.keyField 契约；测试使用受控上游，未将其视作真实云端行为验收。

回收协议核对 [Vertex batchPredictionJobs.delete](https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/rest/v1/projects.locations.batchPredictionJobs/delete) 和 [Google 官方服务定义](https://raw.githubusercontent.com/googleapis/googleapis/master/google/cloud/aiplatform/v1/job_service.proto)：DELETE 返回 Operation，不能把成功响应直接当作删除完成。执行器持久化 operation 并轮询，最终还要求原 job GET 返回 404；详见 [删除和到期回收](native-image-batch-jobs.md#删除和到期回收)。

## 本地容量与出站约束

- 输入最多 10,000 条；key 非空、不含控制字符且不超过 256 字节。请求必须是含非空 `contents` 数组的对象；模型的其他业务约束仍由接入层检查。
- 内联完整提交 JSON 小于 20 MiB，JSONL 输入不超过 128 MiB；序列化在预算耗尽时停止，不先构造完整超大请求。这里是 Okapi 的限制，不代表供应商最大容量。
- 控制响应上限 64 MiB；空确认响应上限 64 KiB。JSONL 默认单行 64 MiB、总量 1 GiB、10,000 行；可显式指定总预算，硬上限 8 GiB，逐行流式读取而非整文件分配。声明长度和实际接收字节都检查，空白也计入字节数。
- 业务层每次收集的文件总预算是原任务 storage_budget 的两倍，为 Base64 和输入回显保留空间。Gemini 单文件和 Vertex 全部文件使用此预算；Vertex 不会逐文件重置额度。单行仍限 64 MiB，每文件最多 200 行、整个任务最多 200 个结果槽位、200 个对象和 32 个结果列表页面；非常大的单行输入回显仍可能超过此界限，不能宣称任意供应商结果都可接收。
- 连接超时 10 秒、单次请求整体超时 60 秒，覆盖响应体读取；不是批任务完成时限。超大文件和慢链路可能超时，接入层须记录不确定结果并恢复，不能无限重试生成。
- 不跟随重定向，不使用环境代理，不自动重试。允许显式配置出站代理；凭证、上传控制头及 Accept-Encoding 不接受额外头覆盖。
- 上传会话 URL 是敏感 bearer URL，只接受同源、同上传路径；不实现 Debug，也不记录上游错误正文。会话提供 encode/restore 接口；调用方按主密钥政策封装后持久化，恢复时核对文件名和字节长度，不能跨账号复用。
- 基础地址来自受信的渠道配置，允许反向代理前缀。此模块不替代网关 SSRF/DNS 策略；接入时仍须校验渠道地址和归属。不要接受最终用户传入任意基础地址。
- GCS 前缀固定为 `okapi-batches/{持久化32位hex任务ID}/`。输入名固定，输出仅允许该任务的 `output/`；下载/删除固定 generation，404 清理视为已完成。分页只拒绝立即重复的 cursor；编排层还须限制总页数并识别跨页循环/重复对象。
- `BatchError.may_have_executed` 表示本次变更可能已经生效。传输失败、无效成功回包、超限回包、408/5xx 等不得当作“未提交”后重新创建。错误仅保留固定代码、HTTP 状态和数值 Retry-After。

## 仍须完成的业务接入

1. **下载容量与恢复**：创建/查询/取消/单图和整批 ZIP 下载、状态/时间/名称/下载筛选、默认 20 条后端分页、删除与七天回收已接入。ZIP 使用有时限的下载租约协调清理；完整 200 张上限与慢链路性能、断点续传仍未验收或实现。删除响应 cleanup_pending=true 表示仍在回收，不能当作远端已清理。
2. **完整准入**：原任务价格和独立 key 预算、待冻结意图容量、用户/key/模型/池权限已接入。创建时 key RPM/RPD、用户×模型 RPM、分组 RPM/RPH 与普通请求共用窗口，按展开子请求数原子准入；并发幂等重放不重复占次数。冻结时与普通请求共用密钥并发上限，每个已冻结任务占一个名额，满额排队且不冻结资金；直到实际结算关闭才释放。成员消费、月 Token/金额、渠道 key 日消费及实时 KPI 已增加可重试补记；成员/渠道软限额在创建和提交前复查。TPM、多模态估算、排队后发送窗口准入、渠道并发及含全部在途冻结的跨接口硬预算仍待补齐。资金、任务创建和重放验证不能代表所有预算行为已覆盖。
3. **未知创建与上传恢复**：已加入唯一 display/提交意图、固定账号/模型/输入的列表查找，持久化分页、检查跨页重复游标和多候选冲突；完整扫描后再次 GET 核对，接管受当前租约约束。未找到/404 继续等待，过期游标重新扫描但保留已有身份线索；绝不重新 POST 或凭 404 退款。持久冲突、缺失身份、超过扫描上限和过期上传会话仍需进一步解决，真实供应商恢复行为尚未验收。
4. **结果和统计完善**：Gemini 与 Vertex/GCS 受控业务验证逐项 UUID/槽位关联、未知/重复 key 拒绝、部分图片收费、未完成或无效结果不发布；Vertex 收集使用返回的结果目录、固定对象 generation，并对多文件总字节计数。Succeeded/PartiallySucceeded 但缺结果行会继续等待，不能把缺失行直接当作退款依据；Cancelled/Failed/Expired 可按完成的实际图片结算。真实 GCS 输出布局、多模态 usage 明细、完整图片解码及真实云端耗时仍待验证。新任务记录创建至结果封存的耗时，账单使用 PG/outbox，实时 KPI 经持久队列补记；这不是同步 RTT 或真实云端吞吐验收。
5. **远端生命周期与实服验证**：已实现固定账号下的输入/输出清理、GCS 全版本删除及 Vertex 异步删除检查点；按任务回收本地产物后仍保留账单、幂等身份和元数据预算。真实 Gemini/Vertex/GCS 提交、IAM、软删除/保留策略、桶区域、延迟和实际计费尚未联调。默认 image_batch_ratio_milli 为 500，上游成本的 500‰ 折算仅为本地估计，不是供应商账单验证。

这些是剩余实现项，不是要求用户批准或手工完成的清单。原生批处理核心目标仍在进行中。

## 实际验证记录

- `/tmp/okapi-native-batch-tests.log`：首批 12 项通过；补充容量、状态和分页边界后，`/tmp/okapi-native-batch-tests-r2.log` 实际退出 0，20 项通过。
- 首轮严格 Clippy 指出局部类型声明位置、简化条件及 clone 赋值问题；第二轮指出测试 HTTP 服务的按值参数和字符串拼接问题，均已修正，没有降低 lint 级别。最终 `/tmp/okapi-native-batch-clippy-r3.log` 的全工作区/all-targets/`-D warnings` 实际退出 0。
- 最终 `cargo test -p okapi-providers --locked --offline -- --test-threads=1 --nocapture` 实际退出 0。`/tmp/okapi-native-batch-provider-report.json`：**135 passed、0 failed、0 ignored、0 软跳过**，包含原生批处理 20 项；这是适配层回归，不是全工作区或公开 Batch API 验收。
- 受控回环 HTTP 覆盖原生请求结构、上传/查询/取消/下载、302/307/308 不跳转、状态码与断连、敏感错误脱敏、真实输入/响应容量边界、结果流截断、GCS 条件上传冲突、版本操作与分页、跨项目/目录/文件身份拒绝。
- `/tmp/okapi-native-batch-source-manifest.json` 与验证文件核对 1,135 个源码/夹具/配置等文件，运行前后一致。该次适配层运行相对上一图片存储最终清单，只修改 providers 的模块导出并新增 10 个批处理源码/测试文件；未改前端、账本、迁移、锁文件或公开路由。文档单独同步，不将新增 20 项叠加到之前 729 项声称新的全量通过。之后发现并修复的重复预扣问题及其回归在 [核心验证记录](core-api-verification.md) 中单独记录。

2026-09-27 恢复接入时，新增测试先复现漏读 metadata.output（`/tmp/okapi-batch-recovery-metadata-red.log` 实际退出 101，0 passed/1 failed），修复后协议初轮 21 项通过。补充查询/分页/身份及真实 PG/Redis 业务测试后，`/tmp/okapi-batch-recovery-targeted-r2.log` 实际退出 0，**81 passed、0 failed、0 ignored、0 软跳过**：协议 26、HTTP 业务 13、长期冻结/存储 42。最终全工作区结果另见核心记录，不将定向测试相加冒充全量。

Vertex/GCS 业务接入复核先以 `/tmp/okapi-vertex-batch-red.log` 复现四项收集问题（实际退出 101，0 passed/4 failed），修复后的最终 `/tmp/okapi-vertex-batch-verified.log` 实际退出 0，**50 passed、0 failed、0 ignored、0 软跳过**，包含全部 22 项批任务 HTTP 业务和 28 项协议测试。严格 Clippy、静态守卫通过；1,182 个文件在验证前后保持一致。该次是关联定向回归，云端 IAM/费用、图片完整解码和超大文件吞吐仍未验收，详细证据见 [核心验证记录](core-api-verification.md#vertexgcs-批任务结果收集2026-09-27)。

随后删除/到期回收新增 Vertex 异步删除协议和 GCS 全版本清理。回收失败保留恢复依据与存储预算，远端和本地产物完成回收后仍保留账单与幂等身份；完整运行证据见 [回收验证记录](core-api-verification.md#原生-batch-删除和到期回收2026-09-27)。

整批 ZIP 与筛选契约见 [下载说明](native-image-batch-jobs.md#整批-zip-下载)，实际验证见 [核心记录](core-api-verification.md#原生-batch-zip-和列表筛选2026-09-27)。

ZIP/筛选接入后的全量回归 `/tmp/okapi-batch-archive-full.log` 实际退出 0，849 项通过、0 失败/跳过，含原生协议 31 项、Batch HTTP 33 项和长期冻结/存储 47 项；严格 Clippy 及 1,197 文件一致性校验通过，完整证据见上述核心记录。
