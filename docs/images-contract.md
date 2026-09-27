# 图片接口与持久异步任务契约

## 当前可用接口

`POST /v1/images/generations` 接收 JSON；`POST /v1/images/edits` 接收 JSON 或 multipart。三个入口形态共用鉴权、参数校验、模型映射、预扣、选路与结算。

- `model`、`prompt` 必须是非空文本。`n` 为 1–10 的整数；缺省为 1。JSON 的 `n: null` 按缺省处理，数字字符串不接受。multipart 的十进制文本允许前后空白，转发前统一为规范数字。
- 不再把非法 `n` 截成合法值后转发原始请求。JSON 重复的身份、张数等已识别字段拒绝；multipart 除 `image` / `image[]` 外的重复字段拒绝。验证失败发生在预扣与上游请求之前。
- multipart 支持 `image` 与重复的 `image[]`；文件字节、文件名、MIME、mask 和其他选项保留。
- JSON 编辑要求非空 `images` 数组，元素使用 `image_url`；支持 HTTP(S) 地址或图片 data URL，mask 使用同样的引用形态。网关不自行下载这些地址。上游文件尚无租户归属登记，因此 `file_id` 明确拒绝。
- JSON 编辑的协议依据是 OpenAI 2026-02-09 的更新：[官方更新记录](https://developers.openai.com/api/docs/changelog)。这不等于全部供应商均支持 JSON 编辑；具体模型和供应商限制仍由上游执行。
- 当前只支持 `per_call` 按张计费。图片流式输出和按输入/输出图片 Token 定价尚未实现；`stream: true` 明确返回 400，避免将 SSE 缓冲后伪装成 JSON 成功响应。

## 金额与传输边界

按请求张数预扣，按成功返回的 `data` 元素数量结算，多预扣的金额退回。返回数量必须为 1 到请求张数，每个元素需有非空 `url` 或 `b64_json`；这里校验响应结构，不代表已下载、解码并验证图片内容。结构损坏、空数组或多于请求张数均返回 502 并退款，不自动重复生成。

报价使用检查乘法，金额溢出在预扣前拒绝。结算沿用预扣时的单张报价和价格版本，`pricing_snapshot.media_units` 记录实际张数；上游请求 ID、HTTP 状态和实际换渠道次数一起落账。

只有明确的上游 401/402/403/429 拒绝允许换渠道。连接错误、超时、408、5xx、响应损坏均不自动重发，因为此时供应商可能已经执行生成。OpenAI 与 Azure 的图片 POST 不跟随重定向。该行为不能保证供应商端恰好执行一次，也不能撤销供应商已发生的费用。

请求体沿用网关的 32 MiB 上限，超限返回带错误码和请求 ID 的 413。图片成功响应最多缓冲 64 MiB，错误响应最多读取 64 KiB；同时检查 Content-Length 和逐块累计大小，没有 Content-Length 也受限。超限不向调用方收取本次费用，但上游可能已经收费。其他端点不使用这一图片响应上限。

验证入口为 `gateway_images_contract`，采用真实隔离 PG/Redis 和受控 HTTP 上游；包含 JSON/multipart、OpenAI/Azure、参数拒绝、文件保留、少返回图片退款、改价期间结算、重定向、分块超大响应与余额/账单/API key 用量对账。实际运行结果见 [核心验证记录](core-api-verification.md)。

## 持久异步任务

需要迁移 `0006_image_tasks.sql`，并运行 `okapi worker`（或包含 worker 的 `okapi all`）。通过管理设置将 `image_tasks_enabled` 设为布尔值 `true` 开启创建，默认关闭。关闭后停止接受新建请求，已有任务继续执行、查询、取消和下载。仅运行 gateway 不会执行队列。

| 接口 | 行为 |
| --- | --- |
| `POST /v1/images/generations/async` | JSON 生成请求，成功返回 202 |
| `POST /v1/images/edits/async` | JSON 或 multipart 编辑请求，成功返回 202 |
| `GET /v1/images/tasks/{task_id}` | 查询状态、错误或结果 |
| `POST /v1/images/tasks/{task_id}/cancel` | 排队任务立即取消；执行中只提出取消请求 |
| `GET /v1/images/tasks/{task_id}/content/{index}` | 用创建任务的 API key 下载已保存的图片 |

创建和查询另有不带 `/v1` 的别名；响应中的 `poll_url`、图片下载地址始终使用 `/v1`。创建返回 `Location`、`Retry-After: 3` 和 `Cache-Control: no-store`。任务 ID 形如 `imgtask_<uuid>`；执行后返回的 `request_id` 是本次账单/预扣 ID，与任务 ID 不同。

- `Idempotency-Key` 可选，须为 1–128 个可见 ASCII 字符。同一用户、同一 API key、同一幂等键及同一规范化请求复用任务，包括已结束的任务；不同内容返回 409。已过期、对账完成且没有待清理对象的记录释放该键；仍待对账或对象清理的记录返回冲突。
- PostgreSQL 保存待执行请求和租约；请求体不保存调用方 Bearer。排队不预扣，worker 领取后重新读取 key/用户状态、有效期、模型与 IP 权限及价格。价格在执行预扣时确定，之后沿用该报价完成结算；排队期间不锁价。
- 每个 worker 最多同时执行 4 个任务；单次执行时限 420 秒，租约 9 分钟，预扣有效期 10 分钟。领取采用行锁和 `SKIP LOCKED`。每次领取生成独立预扣 ID，旧 worker 不能修改新租约的状态或账单。若旧执行在失去租约后才完成 Redis 预扣，其独立预扣由过期清扫兜底释放，可能暂时占用余额；不承诺所有故障退款立即完成。
- 已领取但未标记发送的任务可退款后重新排队，最多领取 3 次。在向上游 POST 前持久标记 `processing`；此后崩溃或失联按 `failed / image_task_result_unknown` 处理并释放用户预扣，不自动重复生成。上游可能已经产生费用。
- 排队取消释放请求体和结果预算，不收费。已发送任务的取消不能撤销上游执行；若收到了成功结果，仍保存结果并按实际返回张数收费。该行为不代表供应商原生取消支持。
- 成功结果、图片字节、任务终态、PG 账单、余额快照和 outbox 在同一事务中提交。Redis 结算通过 `billing_pending` 标记恢复并幂等重放；过期预扣清扫与结果写入锁定同一任务行。失败或保存失败释放用户预扣。实时 KPI/软限额计数不属于这个事务，不能据此宣称所有统计均恰好更新一次。
- 轮询和下载不检查可消费余额，仍需有效身份与任务归属。任务同时限定用户和 API key；同一用户的另一把 key 也返回 404。

### 保存与容量

输入沿用 32 MiB HTTP 上限，持久编码上限 48 MiB。每个排队/执行任务预留 64 MiB 结果预算，加实际编码输入和 4 KiB 元数据预算。每用户预算 256 MiB、全站 1 GiB，同时受每用户最多 8 个待执行任务、1,024 条保留任务和全站 16,384 条任务限制；任一容量不足返回 429。由于结果预留，默认通常最多容纳同一用户 3 个小请求；这不是保证可排入 8 个任务。终态按实际结果大小加元数据预算计数，取消和失败记录也计数。

`b64_json` 解码后保存在 PostgreSQL 私有图片表，结果内替换为需要原 key 鉴权的相对下载 URL；结果元数据最多 1 MiB，总图片及元数据最多 64 MiB。空字节结果明确拒绝并退款，即使同一元素另有非空 URL；这里只做 base64 解码和 MIME 特征识别，不进行完整图片解码验证。每个 gateway 最多 4 个在途下载，许可持续到响应体结束或丢弃；不把图片写入 Redis。

任务和结果保存 24 小时（领取及成功完成时续期），worker 每分钟批量清理；尚待账本恢复的终态记录保留到恢复完成。终态清除输入体和来源 IP，过期删除连带清除图片和领取记录。数据库备份与物理空间回收另受数据库运维策略控制，这些逻辑预算不等于 PG 文件的物理硬配额。

默认未配置存储时，上游只返回外部 `url` 会保存原 URL，可用期由上游决定；开启下述 URL 转存后改为私有下载地址。供应商原生 Batch、图片流式输出、图片 Token 定价及 `file_id` 归属生命周期尚未完成。

### URL 转存与私有 S3

迁移 `0007_image_objects.sql` 后，可为 **gateway 和 worker 的全部实例** 设置相同的 `OKAPI_IMAGE_STORAGE` JSON，重启生效。配置错误会使启动失败；未设置或空值沿用 PG base64 存储和原始 URL 行为。该配置目前由部署环境管理，没有新增控制台配置接口。仅将外部 URL 保存到 PG 可使用 `{"copy_urls":true}`。启用 S3 的配置形态如下（凭证由部署系统注入）：

```json
{
  "active": "assets-v1",
  "stores": [{
    "id": "assets-v1",
    "endpoint": "https://s3.example.com",
    "region": "us-east-1",
    "bucket": "okapi-images",
    "access_key_id": "<injected-access-key>",
    "secret_access_key": "<injected-secret>",
    "path_style": true
  }],
  "fetch": {"trusted_origins": []}
}
```

- 配置仅作用于异步结果。`active` 选择新转存的存储，自动开启 URL 复制；worker 也会转存现有未过期任务的 PG 图片。`stores` 最多 16 个，ID 唯一；可配置 `session_token`。默认要求 HTTPS、路径风格寻址；虚拟主机风格需域名端点及匹配证书/DNS。自建 HTTP 存储须显式设置 `allow_http: true`，不影响远端图片 URL 的抓取权限。
- 复制远端 URL 时只访问公网 HTTPS，校验所有 DNS 地址并固定解析结果；最多跟随 3 次重定向，每跳重新检查。不会携带调用方或上游的 Authorization/Cookie。私网或 HTTP 图片源必须在 `fetch.trusted_origins` 指定完整 origin（协议、主机、端口），不继承渠道的 SSRF 放行设置。可信 origin 是运维授予的例外，不接受用户动态添加。
- 同时支持图片 data URL；识别 PNG/JPEG/WebP/GIF 特征，拒绝空内容、HTML/未知格式和超出剩余结果预算的内容，按长度声明及逐块读取双重限制。此处不做完整图片解码。下载失败使该异步任务失败并释放用户预扣，不重发生成；供应商已发生的费用无法撤销。
- 图片先随任务结果和账单持久化到 PG，再后台转存 S3。转存意图先于 PUT 落库，采用不可变随机对象键、条件 PUT、字节数与 SHA-256 核对；上传确认丢失可重复确认同一内容。上传失败期间 PG 图片仍可下载，不重复收取生成费用。确认成功后才在事务中释放 PG 图片字节。历史版本可能保存的空图片不参与转存，随原任务到期清理，避免反复违反对象字节数约束而阻塞后续任务；不追溯修改既有账单。
- 下载始终经过原用户和原 API key 的归属检查，随后由服务端签名读取私有对象；返回地址不含桶、对象键、凭证或预签名 URL。下载检查内容长度和 SHA-256，配置缺失或内容不符返回 502。成功结果保留供应商其他元数据，转存的 `url`/`b64_json` 被私有下载地址替换。
- 对象工作使用 3 分钟领取租约，失败 30 秒后重试，PUT/GET/HEAD/DELETE 单请求超时 60 秒。临近过期 3 分钟不开始新上传。清理记录在远端删除成功前一直保留；版本化桶使用已确认版本，上传确认不明时先 HEAD 查询版本，避免只写删除标记。实现依据 [AWS 条件 PUT](https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html) 和 [版本化删除](https://docs.aws.amazon.com/AmazonS3/latest/API/API_DeleteObject.html)。
- 关闭新转存可移除 `active`，但须保留旧 `stores` 条目直到原对象全部清理。位置摘要包含端点、区域、桶和寻址风格，同一 ID 不能静默改指向另一个桶；迁移到新位置应使用新 ID。正常凭证轮换不改变位置摘要。误删旧配置会使下载失败、清理重试并继续占用容量，恢复该配置后可继续处理。
- S3 凭证需允许对象 PUT/GET/DELETE；版本化桶还需读取/删除指定版本的权限。HEAD 遇到不存在对象时需能够返回 404（AWS 的权限策略可能使其返回 403），否则清理保留记录并重试。部署应为专用图片前缀配置与 24 小时保留期相容的生命周期兜底，包括旧版本和删除标记；逻辑过期不保证远端物理删除即时完成，也不保证清理完成后无限延迟的外部 PUT 不会产生孤立对象。

S3 转存不降低逻辑容量记账，删除失败的任务继续占用预算。当前没有跨存储自动迁移、公开分享/预签名链接或真实云 S3 的联调结论。故障行为见 `support/image_storage_cases.rs`，实际执行证据见 [核心验证记录](core-api-verification.md)。

独立 S3 协议检查程序为 `crates/okapi-providers/examples/verify_image_store.rs`。在专用测试桶中分别验证开启/关闭版本管理：设置 `OKAPI_S3_VERIFY_CONFIG`（上述 `stores` 中单个条目的 JSON）、`OKAPI_S3_VERIFY_VERSIONED=true` 或 `false`，运行 `cargo run -p okapi-providers --example verify_image_store`。缺少配置直接失败，不会软跳过；程序创建唯一的 `okapi-verification/` 对象，验证签名、特殊字符路径、重复上传、冲突、读取限制和版本删除，并尝试清理。它不属于默认单元测试通过计数；服务不可用时不得标为实服验证通过。

## 竞品基准与剩余批量任务缺口

固定基准 Sub2API 注册了 `POST /v1/images/generations/async`、`POST /v1/images/edits/async`、`GET /v1/images/tasks/:task_id`，并另有图片批量任务接口。[路由源码](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/server/routes/gateway.go)。

其单图异步实现把任务记录写入 Redis，在后台执行同步图片处理，成功结果通过对象存储转存；未配置对象存储时不开启异步入口。它的任务状态保存本身不能证明执行队列可在进程重启后恢复。[异步说明](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/docs/ASYNC_IMAGE_TASKS.md)、[处理器](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/handler/image_task_handler.go)、[存储实现](https://github.com/Wei-Shaw/sub2api/blob/a3eb7ef302961cba716dc78b39b93b60c467db0e/backend/internal/repository/image_task_store.go)。以上为固定版本源码核对，未运行竞品做可靠性比较。

Okapi 当前实现的异步队列使用上述 PG 存储及恢复策略。对应断言在 `gateway_images_contract` 的 `support/image_tasks_cases.rs`、`support/image_tasks_edges.rs`，实际通过情况见 [核心验证记录](core-api-verification.md)。批量任务新增 Gemini/Vertex/GCS 协议层及受控测试，见 [原生批处理边界](native-image-batches.md)；公开 API、持久编排、逐项结果和长期费用恢复仍待接入。没有真实供应商或竞品性能联调，不能宣称全面超过竞品。
