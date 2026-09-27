# 原生图片批任务：持久状态与结算门槛

`okapi_store::image_batches`、公开 Batch 路由和后台执行器已接通；创建入口由 `settings.image_batches_enabled` 控制，默认关闭。受控 Gemini、Vertex/GCS HTTP 上游配合真实 PG/Redis 已验证提交、部分成功结算、取消、原 API key 归属、分页与私有下载。未知提交支持固定账号查找与持久分页恢复；删除和到期回收已接入，验证记录见文末。已接入整批 ZIP 与列表筛选；**完整准入和真实云端联调仍未完成**，这不是完整生产验收或全部竞品能力对齐。

## HTTP 接入

| 接口 | 当前行为 |
| --- | --- |
| POST /v1/images/batches | 鉴权后读取有界 JSON，持久化任务及 pending 资金意图，返回 202、Location、Retry-After；后台成功冻结前不发送上游 |
| GET /v1/images/batches | 同用户且同 API key；默认 20 条，最大 100 条，筛选后再做后端游标分页 |
| GET /v1/images/batches/models | 返回调用方具有权限、按次计费且具备原生供应商候选的模型；尚未优化大目录查询或保证 Vertex GCS 配置完备 |
| GET /v1/images/batches/{id} | 返回白名单元数据、cleanup_done 和可用时的 download_url，不返回凭证、远端资源名、提交意图或正文 |
| POST /v1/images/batches/{id}/cancel | 保存取消意图；已经发送时等远端终态，再按实际成功图片结算 |
| GET /v1/images/batches/{id}/items | 条目及槽位状态分页，默认 20 条、最大 100 条；结算完成后提供私有下载路径 |
| GET /v1/images/batches/{id}/content/{slot} | 已结算、未删除、未过期且原用户/key 才能读取；private/no-store、下载并发限制 |
| GET /v1/images/batches/{id}/download | 终态且有成功图片时提供 ZIP，包含图片和 manifest.json；原用户/key、私有下载、限时租约 |
| DELETE /v1/images/batches/{id} | 仅终态可删除；先隐藏并返回 cleanup_pending，后台完成远端与本地回收后重复 DELETE 返回 cleanup_pending=false |

请求支持 model、task_name、parent_batch_id、provider、items（custom_id/prompt/output_count/reference_images）、aspect_ratio、image_size、metadata。参考图支持内联 Base64，不支持 file_uri。格式转换未实现；response_mime_type 只允许省略或 image/png。配置通过 GenerateContent generationConfig/imageConfig 发送；响应图片实际 MIME 以供应商输出为准。输出展开逐槽序列化，达到 128 MiB 即拒绝，避免先复制全部参考图。

## 数据与容量

迁移 `0010_image_batches.sql` 新增四张表：

| 表 | 内容 | 边界 |
| --- | --- | --- |
| image_batches | 原始用户/key、请求与幂等摘要、模型/分组、固定渠道账号、价格、状态和租约 | 元数据查询不读取请求正文、凭证或图片 |
| image_batch_payloads | 输入、固定账号连接快照、上传会话 | 私有类型不实现 Debug/Serialize；绑定及上传会话由调用方按主密钥政策封装后保存 |
| image_batch_items | custom_id、有限长度预览、请求图片数 | 同一任务内 custom_id 唯一 |
| image_batch_outputs | 每张图片一个预分配槽位、结果摘要、私有字节、用量或固定错误码 | 只能填充已有槽位；冲突重放不能覆盖 |

`0011_image_batch_recovery.sql` 另增私有 `image_batch_recovery`：保存下一页游标、候选远端任务名、页数、已见游标 SHA-256、扫描完成和冲突标记。每页最多 100 项，一轮最多 1,024 页；不通过公开元数据接口暴露。该记录本身不能授权提交或结算。

创建在一个 PG 事务内写入任务、条目、槽位及 pending balance_hold。相同用户/key/幂等摘要只对应一个请求摘要；并发重放复用原任务、渠道和价格，不同请求拒绝。新任务检查用户/key 状态、有效期及渠道账号归属；共享账本用户锁限制未关闭资金意图不超过 128 个，独立 key 预算检查已用额加所有未关闭冻结最大额。网关校验模型、IP、池权限和成员限制，提交前再次从数据库校验 key、模型状态及原账号权限。创建时密钥 RPM/RPD、用户×模型 RPM、分组内每用户 RPM/RPH 共用普通请求的窗口，并按展开的上游子请求数计数；详见文末。冻结时还与普通请求共用密钥并发上限，名额不足时保留未冻结意图等待。成员月消费和渠道 key 日消费已有统计补记、创建/提交前的软限额检查。TPM、渠道并发与包含全部在途冻结的跨接口硬预算仍需补齐，不能声称所有限制已统一。

输入最多 128 MiB、200 条目/200 张图片、每条目 1–4 张；图片槽位最多 16 MiB。存储预算按全部输出最坏大小预留，另计输入、每槽 16 KiB 和每任务 512 KiB 元数据。默认每用户/每 key 最多 8 个未终结任务，每用户 1,024/全站 16,384 个尚未回收产物的任务，每用户 8 GiB/全站 32 GiB 预算。失败、取消和未完成记录均计容量；回收完成后释放产物名额和字节预留，保留每条 512 KiB 元数据预算，因此历史记录仍受总预算约束。

列表使用 `(created_at,id)` 降序游标、查询 `limit+1` 判断后页，后端拒绝 0 或大于 100 的大小。存储测试验证 23 条分成 20+3、无重叠、跨 key 游标和 parent 拒绝；HTTP 测试另验证默认 20+1 分页。列表支持以下可组合筛选，均在 SQL LIMIT 之前应用；翻页时保留相同筛选参数。游标对应的任务被主动删除后，同一用户/key 仍能继续翻页；跨 key 游标拒绝。

| 参数 | 语义 |
| --- | --- |
| status | 精确状态；未知值返回 400 |
| q | 任务名称忽略大小写的字面子串，支持中文；`%`、`_` 不作为通配符。去除首尾空格，空串不筛选；最多 256 字符/1,024 字节，不允许控制字符 |
| created_from / created_before | 非负 Unix 秒，创建时间范围为左闭右开；相等、倒置或越界返回 400 |
| downloaded | `true` / `false`，根据首次下载尝试时间是否存在筛选，不代表已完整接收 |

未知参数、非法布尔、非法分页大小及不属于当前用户/key 的有效格式游标返回 400；游标 ID 格式非法返回 404。接口不返回所有记录再由前端切页。

调用示例（BASE_URL 为网关地址，API_KEY 为原任务密钥；BATCH_ID 取返回的 imgbatch_ ID）：

```sh
curl --fail --get "$BASE_URL/v1/images/batches" \
  -H "Authorization: Bearer $API_KEY" \
  --data-urlencode 'status=completed' \
  --data-urlencode 'q=示例任务' \
  --data-urlencode 'downloaded=false' \
  --data-urlencode 'limit=20'

curl --fail "$BASE_URL/v1/images/batches/$BATCH_ID/download" \
  -H "Authorization: Bearer $API_KEY" \
  --output batch.zip.part &&
python3 -c 'import sys, zipfile; sys.exit(zipfile.ZipFile("batch.zip.part").testzip() is not None)' &&
mv batch.zip.part batch.zip
```

下载示例使用临时后缀，curl 与 ZIP 完整性检查均成功后才改名。命令失败时保留 `.part` 便于检查，不能把它视为完整结果；再次下载使用相同任务，不重新创建批任务。


## 状态和恢复

正常路径为 `funding → preparing → submitting → running → collecting → settling → completed/partial/failed/cancelled`。

- `prepare` 和 `mark_submitting` 核对同 UUID 的 held 记录，必须匹配用户/key/模型/请求摘要/最大金额及完整价格快照。
- 提交前持久化唯一 `submit_intent`。submitting 的租约丢失或主动释放进入 uncertain，不能再次走 mark_submitting，也不能以本地取消当作确定未执行而退款。
- 领取通过 `FOR UPDATE SKIP LOCKED`，每次使用新 token，期限 120 秒。变更和私有 payload 读取先锁行，再以数据库时钟核对期限；不能只用等待锁之前的 WHERE 判断。已通过真实行锁等待跨越截止时间的回归。
- 对同一提交意图，迟到的远端确认可保存任务身份，不需要已失效的执行租约。不同远端身份、矛盾终态拒绝；迟到的 pending/running 观察不能将已知终态倒退。
- `cancel` 仅保存取消意图，不退钱。提交前明确中止才可把输出转失败并进入 settling；已发送/不确定任务必须等待远端事实。远端已成功时，即使本地曾请求取消，仍按成功输出结算。

worker 每进程最多执行两个步骤，轮流领取执行、清理和统计补记。执行/清理每 30 秒续租，每步最多 600 秒；失去租约停止当前步骤。Gemini 固定文件名与封装后的上传会话、Vertex 固定 GCS 前缀和账号快照用于重启恢复。Vertex 在每个步骤开始前刷新 token。明确拒绝可进入零收费结算；超时/5xx/无效成功确认进入 uncertain，保留冻结且不重发创建。

uncertain 恢复逐页查询原账号：Gemini 列举 batches/operations，Vertex 按精确 displayName 过滤；标签包含任务 UUID 和原提交意图。每领取一次最多处理一页，进程重启继续保存的游标。完整扫描后必须只有一个候选，再通过独立 GET 核对原标签、模型和固定输入文件/GCS URI；Vertex 同时限制项目与区域。接管事务再次检查当前租约、uncertain 状态、完成的扫描和无冲突候选，不能将允许迟到的原始 POST 确认规则套用到过期恢复进程。

没有匹配时每 60 秒重新扫描，详情 404 时保留候选并等待；这两种情况都不能证明未执行。带游标的列表 400 允许重新扫描，但保留已有候选；新的不同候选、列表或详情身份矛盾形成持久冲突，不靠重试清除。多页循环、格式错误、unreachable 和页数上限阻止接管。超过 1,024 页的大账号、身份字段缺失、凭证失效或持久冲突仍可能长期保持冻结，需要更强的远端事实才能解决；当前没有人工强制认领/退款入口。真实 Gemini 是否完整回显输入身份仍需实服验证。

## 结果和金额

每个预分配槽位只接受一次结果；相同状态、内容 SHA-256、类型、用量/错误码可重放，不同结果或未知槽位拒绝。接入解析器核对任务 UUID/槽位 key、重复 key、结果冲突、Base64、图片大小及 MIME 魔数、Token 数值与总量关系；这仍不构成完整图片解码验证，多模态 Token 明细尚未单独归类。

只有全部槽位终结才能 seal，seal 后不再允许写输出。执行器必须先验证所有远端文件、逐项身份和完整 EOF，再调用 seal；计数相等本身不证明结果流完整。

Succeeded/PartiallySucceeded 必须收集到全部请求槽位的结果行（成功或明确失败），缺失行继续保持 collecting，不提前结算。Cancelled/Failed/Expired 仍可包含已完成图片，缺余槽位按失败处理。Vertex 的非空 status 配空 response:{} 是合法失败行；配真实非空响应则是冲突，不能按零费用忽略。

Vertex/GCS 按远端返回的 outputInfo.gcsOutputDirectory 收集；未提供时沿用已核验的固定任务输出前缀。请求的 prefix 必须是该具体目录，返回对象也逐个核对，不能读相邻目录；对象 generation 固定。每次收集的累计文件预算为 storage_budget 两倍（含原始输入和图片预留），覆盖 Base64 和输入回显，读完文件后按实际收到的字节扣减余量，空白也计数。总预算硬上限 8 GiB、单行 64 MiB、最多 200 个对象/32 页；错误或后续页中断时已有 staged 图片继续私有，重试使用原快照。

`UnitQuote` 使用整数 micro-USD，检查 `original-discount=amount` 及缩放边界。discount 可为负，保留既有加价规则。创建时保存原按次报价及 image_batch_ratio_milli（默认 500，允许 0–10000）；实际账单为保存的单张批价格乘成功槽位数，失败不收费。当前上游成本估计按渠道成本再乘 500‰，并非真实供应商发票验收。finish 必须读到同身份/价格的 closed hold，核对实际金额、原额、优惠、标价、成本及 media_units，才能标记公开终态。错误收费或未关闭冻结都不能发布图片。结算收据仅依赖已封存结果与原始快照，重试时不使用当前价格、临时错误或主机名；新任务的 latency_ms 为创建至结果封存的实际毫秒数，包含排队、上游执行与收集，不是同步请求 RTT；超过 i32 范围饱和到 i32::MAX。封存时间持久化，结算失败重试不会改变收据耗时。升级前已在 settling 的任务没有封存时间，继续保留原先的 0，避免改变已有结算收据。

图片查询同时检查用户、原 API key、终态、删除标记与七天期限；staged、未结算、跨用户/key 和已过期图片均不可读。downloaded_at 记录首次开始提供下载，不表示客户端已接收全部字节。单图和 ZIP 的 HEAD 只检查元数据，不读取图片正文、不领取下载槽、不设置 downloaded_at。

## 统计补记与软限额

迁移 `0014_image_batch_statistics.sql` 保存创建时的 member_user_id 和结果封存时间，并增加独立统计投递表。首次公开终态与投递记录在同一个 PG 事务中提交；统计金额/Token 来源于已关闭的结算凭据，时间使用首次 billing_records.created_at。投递写入失败时账单仍只结算一次，任务停留 settling，下一次可以按原收据继续发布。任务创建后修改密钥成员归属，不会把已经发出的任务消费转记给新成员；发送上游前发现归属变化则中止并退回冻结。

投递更新成员月消费、用户月 Token/消费、渠道 key 日消费，以及实时 KPI 的完成请求数、Token、金额、错误数。每批任务是一次完成请求，部分成功按实际成功图片收费；全失败记错误。批任务月累计始终保存，报价仅在配置了对应 volume 规则时读取。账单、outbox/ClickHouse 保留原有持久链路，不依赖这次 Redis 补记；未向同步渠道的时延 EWMA 写入批任务排队耗时。

统计具有独立 120 秒租约，单次 Redis 投递上限 60 秒，失败 30 秒后重试；不长期持有 PG 连接。每个指标通过 Lua 将整数增量和同 Redis Cluster 槽的去重凭据一起写入；部分指标已成功、响应丢失、进程重启或 PG 确认失败时，重放不会重复累加。金额增量以十进制整数文本传给 INCRBY，避免 Lua double 的精度损失。任务图片清理不删除统计投递记录。

月份、日期和 KPI 秒桶都使用原结算时间，不使用恢复进程时间。月指标/凭据有效到原结算后 40 天、渠道消费 2 天、KPI 360 秒；已经过期的桶跳过，不伪造当前流量。投递完成后不会周期重建 Redis；Redis 整库丢失、独立键淘汰或人工修改导致的历史统计恢复仍未实现。去重能力不代表全部 Redis 数据损坏场景都能自动恢复。

成员限额、渠道日消费保持既有“结算后累计”的软限额语义，创建和发送前会检查已累计金额；渠道达到上限时，创建尝试其他允许的候选，已绑定任务提交前触顶则零费用终结。Redis 短暂故障/补记延迟和并发在途任务仍可能超额，尚未改为包含所有冻结的跨接口原子硬预算。批任务完成后无需重新生成就能补记，不因统计失败再次扣款。

升级前的历史终态不会自动补记；旧在途任务缺少可靠的创建时成员身份，保持 NULL，不按当前密钥归属猜测历史成员。新字段及投递队列属于内部状态，不通过任务元数据暴露成员 ID。

## 整批 ZIP 下载

下载路径为 `/v1/images/batches/{id}/download`，任务终态、至少一张成功图片、未删除/清理/过期时提供 download_url；其他情况为 null，直接下载返回 404。失败任务中的已结算成功图片也可下载。鉴权使用原用户和原 API key，下载已付费图片不要求剩余余额，不触发新生成或再次扣费。

ZIP 附件名为系统任务 ID，内部图片名为 `images/0000.png` 等槽位名（PNG/JPEG/WebP 根据实际 MIME），用户 task_name/custom_id 不参与文件路径。manifest.json 包含公开批任务元数据、逐槽 custom_id/状态/文件名/MIME/错误码；失败槽位的文件为 null。不包含输入 prompt、上游资源身份或凭证。

采用 async_zip 0.0.18 的 Stored 模式，逐张读本地图片、检查长度和 SHA-256，再异步写入 64 KiB 管道。应用不会缓存整批 ZIP；每个传输同时持有一张最多 16 MiB 的图片及有限管道/响应缓冲，数据库驱动和网络另有开销。最多 200 张图片，小于 ZIP32 的 4 GiB 上限；没有临时磁盘副本。损坏或超时中断响应体，不写成功的 ZIP 结束目录。此上限来自实现约束，尚未完成 200×16 MiB 实测。

单进程与其他图片下载共享 4 个并发槽，单任务通过数据库限制最多 16 个跨进程有效下载租约；超限返回 429。迁移 `0013_image_batch_downloads.sql` 增加 600 秒租约，生产 ZIP 的后台任务最多运行 590 秒，不保证慢链路一定传输完成；当前无 Range/断点续传。连接中断或响应体读完后释放租约和并发槽；进程异常、数据库释放失败则等待租约到期。下载流不长期持有数据库连接。

授权后的传输允许在任务被删除/到期后继续读取，新的请求立即拒绝。清理领取与下载领取锁定同一批任务，并在拿锁后复核有效下载租约；租约有效时不回收图片。租约过期后仍留在进程缓冲中的字节可以发送，但不能再从数据库领取下一张图片。最终清理同时删除下载租约记录。downloaded_at 即使传输失败也保留首次尝试时间。

接口对照固定版本 [Sub2API Batch MVP](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/docs/BATCH_IMAGE_MVP.md) 的 download 路由；打包实现依据 [async_zip](https://github.com/Majored/rs-async-zip) 和已锁定版本源码。此对照不表示全部字段/筛选名称与竞品完全兼容。

## 删除和到期回收

`0012_image_batch_cleanup.sql` 增加私有清理检查点，记录远端任务是否已删除、Vertex 删除 operation 和独立重试错误。仅终态、同身份/价格/金额的 closed hold、且用户已请求删除或七天到期的任务可以领取清理租约。租约类型与执行器隔离，锁行后复核数据库时间，不能用于生成或结算；worker 的两个并发槽交替处理执行与清理。

清理始终使用创建时冻结的账号连接快照。Gemini 先核实远端终态，删除 operation 并通过 GET 404 确认，再删除固定输入文件和已持久化的结果文件。Vertex 保存异步删除 operation，重启后继续查询；operation 完成、失败或 404 都不单独证明任务消失，必须重新查询原 job 得到 404 后才能删除对象。失败或响应丢失只重试原资源，不重新生成、不修改账单。

GCS 清理列举当前任务前缀的所有 generation，包括 input.jsonl 和 output/ 下的旧版本与其他输出文件。每次最多删除 32 个版本，下一步从目录起点重扫，避免边删边翻页造成遗漏；空页可继续翻页，但限制 32 页并检测游标循环。不同任务、桶、输入文件名的相邻路径均拒绝。空列表确认完成后，PG 事务删除输入、连接快照、上传会话、条目/图片和恢复检查点，并标记 cleanup_done。供应商权限或保留策略拒绝删除时，清理保持待重试，私有恢复依据和原预算仍保留。

账单、资金关闭凭证和任务幂等身份始终保留。到期任务保留可查询历史及相同请求的幂等重放；主动删除任务继续隐藏，原幂等键重放返回冲突。清理完成后的条目列表为空，历史计数与金额仍表示原任务结果。对象 DELETE 不绕过云端软删除、桶保留策略或备份；这里的回收不等于云服务底层介质立即擦除。真实云端 IAM/保留策略行为仍需联调。

执行错误采用独立的类型化 `image_batches::Error`；公开路由显式映射到已有本地化错误壳，供应商错误正文不公开。未修改错误码守卫或前端语言文件。

## 验证记录

- 加价结算：`/tmp/okapi-native-batch-surcharge-red.log` 实际退出 101，0 passed/1 failed，复现账本误拒负折扣；移除这项误拒，保留金额等式和冻结上限。`/tmp/okapi-native-batch-surcharge-money.log` 实际退出 0，domain/pricing/ledger **89 passed**。
- 首批存储/账本集成：`/tmp/okapi-native-batch-jobs-first.log` 实际退出 0，**11 passed**。真实隔离 PG/Redis，包含 16 路并发创建、24 路并发领取、部分成功的实际账单/余额/outbox、取消后成功收费、越权读取拒绝等。
- 行锁等待竞争：`/tmp/okapi-native-batch-lease-red.log` 实际退出 101，0 passed/1 failed；复现过期执行器仍可提交，修复为锁后核验。`/tmp/okapi-native-batch-jobs-money.log` 实际退出 0，domain/pricing/ledger **101 passed**，包括批任务 12 项、冻结原有 25 项及加价新增 1 项。此后执行错误改为独立类型，最终全工作区结果另记，不合并不同运行计数。

以上不是公开 API、真实云端提交、真实供应商计费或性能基准验收。原生 Batch 仍须完成 [业务接入](native-image-batches.md#仍须完成的业务接入)。

存储阶段 `/tmp/okapi-native-batch-jobs-full.log` 实际退出 **0**，**795 passed、0 failed、0 ignored、0 软跳过**，128 个完整套件，包含最终类型化错误版本；不是定向运行相加。1,158 文件运行前后指纹一致。该阶段严格 Clippy 和静态守卫通过。

公开接口与执行器接入后，`/tmp/okapi-native-batch-endpoints-full.log` 实际退出 **0**，全工作区 **802 passed、0 failed、0 ignored、0 软跳过**。包含 HTTP 业务测试 6 项、输入展开容量测试 1 项，以及最终模型停用分支和上传会话恢复验证。`...-clippy-pass.log` 全工作区/all-targets/`-D warnings` 通过；1,169 文件运行前后指纹一致。425 条权限/错误探针和 14 项 Python 工具测试单列。完整证据见 [核心验证记录](core-api-verification.md)。

自动找回定向验证（2026-09-27）：`/tmp/okapi-batch-recovery-targeted-r2.log` 实际退出 0，81 项通过，其中 HTTP 业务 13 项、账本/存储 42 项、协议 26 项。新增场景包含回包丢失后的分页与进程重启、账号地址/价格修改仍沿用原快照、唯一账单、未找到/详情 404 后恢复、多候选和详情矛盾持久冲突、无效页面/过期游标、key 撤销后的取消、取消且部分输出的收费。存储层另验证过期执行器不能接管、并发检查点写入、跨页循环、重扫保留证据及原始迟到确认。供应商采用受控 HTTP 服务，PG/Redis 为真实隔离服务；不是云端验收。

最终 `/tmp/okapi-batch-recovery-full.log` 实际退出 **0**，全工作区 **819 passed、0 failed、0 ignored、0 软跳过**；全目标严格 Clippy 通过，1,178 个验证文件运行前后一致。探针与工具测试单独统计，完整证据见 [核心验证记录](core-api-verification.md#原生-batch-未知提交找回2026-09-27)。

此后 Vertex/GCS 收集修复的最终定向运行 `/tmp/okapi-vertex-batch-verified.log` 实际退出 0，50 项通过（网关业务 22、协议 28），含 9 项新增 Vertex 业务场景；异常后持久冻结/实际余额和结算后私有下载均有断言。严格 Clippy 通过，1,182 文件验证前后一致。它不替代上述历史全量结果，也不是实际云服务验收；本轮未改账本或迁移。

删除与到期回收的实现和实际测试记录见 [核心验证记录](core-api-verification.md#原生-batch-删除和到期回收2026-09-27)。覆盖原账号和删除 operation 的重启恢复、全版本对象回收、回包丢失、权限失败、异常分页、自动到期 worker、租约隔离、容量释放与账单/幂等身份保留；记录区分中间定向运行与最终源码全量运行。

ZIP、筛选及 HEAD 语义的新增验证见 [核心验证记录](core-api-verification.md#原生-batch-zip-和列表筛选2026-09-27)，其中将独立 ZIP 解码、定向与完整工作区结果分开记录。

本轮最终全工作区实际退出 0：**849 passed、0 failed、0 ignored、0 软跳过**，1,197 文件指纹一致；严格 Clippy、许可证/来源和静态守卫通过。ZIP 响应另经 Python zipfile 独立验证。记录见上述核心验证链接，未将定向结果与历史全量相加。


## 创建时的请求次数限流（2026-09-27）

密钥的每日窗口使用 UTC `YYYYMMDD`，与普通请求账本和速率查询一致，修复旧批任务使用 Unix 天数导致两种接口分开计数的问题。模型按规范模型 ID、用户共享，分组按生效分组、用户共享。每张输出在供应商 JSONL 中对应一个独立请求，因此增量为所有条目的 output_count 之和；例如 3 个条目各 2 张，五个配置启用的请求次数维度各增加 6。密钥 RPM/RPD 即使不设上限也保留计数；未配置的模型/分组维度不写入。该计数是准入尝试量，账单/KPI 的批任务请求数与实际用量不改写。

数据库先检查幂等重放、父任务归属、预算及容量，再在同一创建事务持锁期间调用 Redis。并发或跨网关的同一幂等请求只为新任务执行一次限流；限流成功后才插入任务和 pending 冻结意图。全部 Redis 键带用户 hash tag，单 Lua 先验证所有计数再写入，任一维度不足返回 429 rate_limited 和对应 rpm/rpd/model_rpm/group_rpm/group_rph 参数。错误类型、非法整数、越界计数或连接异常按 503 overloaded 拒绝，调用最多等待 3 秒；期间不另取 PG 连接、不调用供应商。

Redis 与 PG 不是分布式事务：Redis 已执行但应答丢失、PG 随后回滚或提交状态未知，可能保守消耗一次尝试而没有可见新任务；不会猜测回滚共享计数，以免放大额度。没有为旧版本独立 RPD 桶做历史回填，旧错误桶会按原 TTL 自然到期。此次数准入不处理排队后的时间窗重新准入、TPM、多模态 Token 预算或在途硬预算；密钥并发的后续接入见下节。

## 密钥并发与排队

资金冻结前读取当前密钥并发上限，与普通 API 请求原子竞争同一份额度。每个已冻结批任务占一个名额；未冻结任务仅保留 pending 意图，名额不足时约三秒后重试，不扣款、不提交供应商、不返回虚假的上游失败。排队期间仍可取消，取消等待任务不会释放正在执行的普通请求名额。

名额随长期冻结保留，直到确认远端终态、账单提交并真正关闭冻结。取消请求、未知提交、进程重启和重复轮询都不提前释放，重复关闭也不会多释放。缺少旧版派生索引时从当前凭证推导，PG 权威恢复时重建索引。Redis 整体丢失后的边界、普通同步占用的既有 TTL，以及管理员降低上限的语义见 [长期冻结契约](durable-balance-holds.md)。这不是渠道级并发，也不限制供应商内部批任务子请求的并行数。


共享并发验证（2026-09-27）：完整 Batch HTTP 55 项、长期冻结/存储 52 项、worker 修复 4 项和重复预扣 5 项共 116 项关联通过；随后单次完整工作区 **895 项通过**，实际退出 0，0 failed/ignored/软跳过。新增断言覆盖普通 HTTP 在途时批任务排队、取消排队任务不释放他人占用、已冻结取消确认、未知提交与执行器重启、重复释放、不同 key 隔离及索引修复。供应商为受控 HTTP，数据库和 Redis 为真实隔离服务。严格 Clippy 和静态守卫通过，源码与完整日志范围见 [核心验证记录](core-api-verification.md)。
