# 模型广场设计与调研

调研日期：2026-09-05。研究展示结构和交互方式，不复制竞品业务代码，也不将竞品报价或模型能力写入本站数据。

| 对照对象 | 已核实的设计 | 本次采用 |
| --- | --- | --- |
| [OpenRouter 模型目录](https://openrouter.ai/models/)及[模型 API](https://openrouter.ai/docs/api/api-reference/models/get-models) | 模型搜索、模态筛选、上下文与输入/输出报价；模型作者与托管提供方是不同维度 | 厂商导航面向模型作者；能力只使用目录中明确声明的信息；单价旁保留单位 |
| [new-api Pricing](https://github.com/QuantumNous/new-api/blob/main/web/src/features/pricing/index.tsx) | 厂商/分组筛选、卡片/表格切换、模型详情抽屉 | 默认卡片快速浏览，表格比较价格；完整计价与模拟器进入详情 |
| [Sub2API PlazaFilterBar](https://github.com/Wei-Shaw/sub2api/blob/b1748c4ea99ce2120401a269142aa071e18a84da/frontend/src/components/modelPlaza/PlazaFilterBar.vue)及[分组区域](https://github.com/Wei-Shaw/sub2api/blob/b1748c4ea99ce2120401a269142aa071e18a84da/frontend/src/components/modelPlaza/PlazaGroupSection.vue) | 平台、分组、倍率与搜索联动，分组价格附带条件说明 | 将厂商与本站接入分组分开；统一分组视角，使卡片、详情和费用估算同步换算 |

## 页面结构

- `/pricing` 保留匿名访问，入口改名为“模型广场”。
- 厂商目录展示本地品牌 SVG、名称和模型数。已知厂商别名归一；自定义厂商保留原名称，空厂商进入“未归类”，不把 OpenAI 兼容协议当成厂商信息。
- 卡片展示名称、可复制模型 ID、已知能力/上下文、输入输出单价和接入分组。支持厂商、关键词、能力、计费方式与接入情况交叉筛选。
- 表格集中比较核心价格。按 Token 输入/输出价格排序时，按次和阶梯计价不参与混排，未知值置底。
- 详情提供缓存读取/写入、已配置的音频/图片计价、精确规格和分组费用估算；阶梯计价明确为条件价格。
- URL 保存筛选、单位、视图、排序、当前页、每页数量及模型详情页签。默认每页 24 个模型，可切换 12/24/48；支持上一页、下一页和页码跳转，改变筛选或页容量返回第一页。
- 手机端厂商导航横向滚动并定位当前选项，筛选与计价可展开，优先留空间给模型卡片。支持深色主题、中英文和键盘操作。

## 数据口径

`GET /api/pricing` 增加 `context_window`、`max_output` 及布尔能力白名单，来自已有模型配置。未配置字段保持未知，不根据名称或倍率猜测能力；扩展对象不直接公开。不增加数据库字段或更改路由/计费引擎。

接入情况来自已启用渠道与池/分组配置，是静态配置可达性，不等同于实时健康或个人密钥权限。

固定单价遵守原倍率口径：输入为模型倍率 × 分组倍率 × $2/1M；输出及缓存等再乘对应倍率。按次价格也应用分组倍率；合法零倍率保留为零。缺失价格显示“—”，很小的非零单价切换成 1K 后仍保留精度。估算不含个人系数及动态规则，最终以账单为准。

32 个厂商图标来自 [Lobe Icons](https://github.com/lobehub/lobe-icons/tree/4aaf4ee1fb2678a7f989ea570f0f6ce14a9abf75/packages/static-svg/icons)，MIT 许可及来源随 `frontend/public/vendor-icons` 保留。资源本地托管，不依赖第三方图片请求。

## 验证

`frontend/e2e/catalog.spec.ts` 覆盖厂商别名、自定义厂商、图标加载、交叉筛选、分页、深链接与返回、复制与焦点恢复、零价和缺失价格、小额精度、阶梯价、模拟器校验、手机布局及错误/空态。截图使用隔离测试数据，不代表本站实际报价或模型能力。

`console_portal_pages` 覆盖匿名访问、公开规格/能力白名单，以及主池/降级池对应的分组可见性。

本次验证结果：前端构建、oxlint、i18n/权限守卫通过；38 项交互回归、2 项真实控制台只读冒烟、2 项公开目录接口测试通过。最终中文别名及手机展开交互又通过目录专项回归。

全量 Clippy 尚未通过：工作区已有的 `channels.rs` 类型转换，以及 `analytics.rs`、`stats.rs`、`usage_details.rs` 中的格式化/行数等告警阻塞检查；本次目录修改的告警已修复。这些其他功能的代码未在本次任务中修改。

## 调用示例与分页补充

模型卡片和表格都有“调用示例”入口；详情内分为“价格与详情 / 调用示例”，切换页签保留正在编辑的示例。匿名用户可以复制 Base URL、完整 POST URL、cURL、Python 3 标准库、Node.js 18+ fetch 及 JSON 请求体，并下载 `.sh` 脚本。页码、页容量、模型和页签均可通过 URL 恢复。

接口模板直接对照本仓库 `gateway::router` 和各入口探针：Chat Completions、Responses、Messages、Embeddings、Images、Rerank、Speech、Videos。用户按模型实际能力选择模板；明确声明 `embedding=true` 时默认向量模板。不根据按次计价或厂商猜测图像/视频能力。聊天模板可启用流式响应；Speech 样例将二进制结果保存到 `speech.mp3`。

地址默认遵守当前部署：console `:8081` 及本地 Vite 预览对应 gateway `:8080`，同域反代使用当前 origin 的 `/v1`。前端构建时可在 `frontend/.env.local` 设置 `VITE_GATEWAY_BASE_URL=https://api.example.com/v1` 指定独立网关域名；此项是公开地址，不应含密钥。用户也能在示例中临时修改地址，子路径保留，不重复追加 `/v1`。不会自动请求用户输入的地址。

示例从环境变量 `OKAPI_API_KEY` 读取凭证，不读取或拼入当前登录密钥，不把凭证存到分享链接。请求体先序列化、再按目标语言转义；实际调用分组来自密钥，与目录价格筛选分开。下载脚本先检查环境变量，页面本身不发送模型调用。

专项验证在 `catalog.spec.ts` 与 `request-examples.spec.ts`：分页跳转/刷新/返回/筛选复位；页签与输入保留；URL 编辑和复制；下载；特殊字符通过 sh、bash、zsh、Python、JavaScript 的本地替身执行验证；不产生真实模型请求。

分页与调用示例补充验证：构建、oxlint 和 i18n/权限守卫通过；44 项交互/生成器回归及 2 项真实控制台只读冒烟全部通过。
