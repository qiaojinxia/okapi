// Model editor copy is kept together, separate from the shared statistics language pack.
export const modelPresetZh = {
  vendorFilter: '预设供应商', allVendors: '全部供应商',
  vendors: { OpenAI: 'OpenAI', Anthropic: 'Anthropic · Claude', Google: 'Google · Gemini', DeepSeek: 'DeepSeek', Alibaba: '阿里云 · Qwen', Moonshot: '月之暗面 · Kimi' },
  placeholder: '搜索主流模型，或输入自定义模型 ID',
  customHint: '没有匹配预设，可保留此 ID 并手动配置。',
  customResetHint: '改为自定义 ID 会清除预设规格和缓存倍率，但保留输入／输出价格。',
  selectHint: '当前范围 {{n}} 个预设；选择后带入规格与已核实的缓存倍率，不改输入／输出单价。仅输入名称不会自动套用。',
  applied: '已带入预设规格 · 展开可调整', referenceOnly: '官方参考 · 不自动覆盖现有配置',
  source: '官方规格 · 核对 {{date}}', pricingSource: '价格来源', ttls: '官方缓存时长：{{values}}',
  priceReference: '标准参考价：输入 ${{input}} / 输出 ${{output}} · USD / 1M',
  snapshotId: '{{vendor}} · 快照 ID',
  applyPrice: '填入参考价', priceExpired: '参考价已过期', conditions: '适用条件与计价说明',
  scope: '预设是人工核对的离线参考，不是实时同步。实际接口和能力取决于渠道；未核实项保持未声明。价格仍需发布，渠道需单独配置。',
  cachePrecision: '缓存折扣相对当前输入价，倍率保留 6 位小数。未核实的倍率按普通输入价格起步；参考价不等于完整上游账单。',
  replaceSpecs: '用预设替换规格',
  replaceHint: '仅替换类型、模态、能力和限制；保留展示名称、说明、所有价格、缓存倍率、服务档位及路由。',
}

export const modelPresetNoticesZh = {
  gpt6: '参考价适用于 Standard 短上下文；超过 272K 输入时官方对整次请求加价，另有工具及服务层级费用。此按钮不会配置这些规则。',
  gpt61Tools: 'GPT-6.1 Sol 工具调用需要 Responses；Chat Completions 不支持工具调用。',
  gpt6Tools: 'GPT-6 Sol / Luna 工具调用请使用 Responses；Chat Completions 函数调用需要 reasoning_effort=none。',
  gemini: '参考价仅覆盖文本输入／输出；音频输入、显式缓存按小时存储及搜索可能单独收费，请核对渠道支持与高级计价。',
  geminiLegacy: 'Gemini 2.5 当前要求上游账号已有使用记录，预设不保证新账号可调用。',
  geminiPro: 'Gemini 2.5 Pro 参考价适用于不超过 200K 输入；长上下文还有不同的输出加价，需要单独配置。',
  geminiPromotion: '该参考价有效至 2026-12-31；过期促销价不可再填入。',
  imagePricing: '图像生成可能按图片输出 Token、分辨率或张数收费。请配置实际计费模式和图像输出价格，文本输出价不等于图片价格。',
  deepseek: '参考价使用官方高峰价格，低峰为一半；预设不会按时段自动切价。',
  regional: 'Qwen 价格受部署区域和输入长度影响；请按实际上游区域配置价格与缓存倍率。',
  kimiCache: 'Kimi K3 支持 5m / 1h 缓存；仅核实了时长支持，未取得明确倍率，不套用 Claude 缓存折扣。',
  partial: '只填入已核实规格；未声明的输出限制和缓存倍率请与实际上游核对。',
}

export const modelEditorZh = {
  capabilitiesEmpty: '未声明常用能力。选择模型预设可自动带入，也可按需调整个别标签。',
  editCapabilities: '调整常用标签', retainedCapabilities: '其他预设或历史能力声明保留，不需逐项配置。',
  rateAvailabilityHint: '根据当前模态和能力声明，明确不适用的价格项已禁用；未声明项仍可填写。禁用不会清空已有数值，也不会改变渠道能力。',
  noImageInput: '当前声明不支持图像输入', noAudioInput: '当前声明不支持音频输入',
  noImageOutput: '当前声明不支持图像输出', noAudioOutput: '当前声明不支持音频输出',
  noPromptCache: '当前声明不支持提示缓存',
}

export const modelAdminZh = {
  admin: {
    modelPreset: modelPresetZh,
    modelPresetNotices: modelPresetNoticesZh,
    modelPriceOverview: {
      title: '计费一览（USD / 百万 Token）',
      input: '输入', output: '输出', cacheRead: '缓存读取',
      cacheWrite5m: '缓存写入 5 分钟', cacheWrite1h: '缓存写入 1 小时',
      official: '官方 {{price}}', officialTitle: '预设核对的官方标价；与售价不同时标黄',
      markup: '售价为官方 ×{{ratio}}',
      hint: '按当前倍率实时换算，未单独设置的时长档按通用写入倍率；分组折扣与计费规则另行生效。',
    },
    modelMeta: {
      ...modelEditorZh,
      capabilities: '常用能力标签',
      capabilitiesHint: '用于目录展示和价格输入项联动，不会启用接口、强加请求参数或改变计费及渠道协议；实际支持以渠道为准。',
    },
    modelSimple: {
      drawerHint: '先选模型预设，再配置价格；自定义模型可手动填写。价格保存为草稿，发布后生效。',
      ratioEditor: '调整倍率（高级）', modalCacheTitle: '多模态缓存价格（可选）',
      unavailableTitle: '不适用计价项（{{n}}）', billingNotes: '计价规则说明',
      baseHint: '基于 ${{price}} / 1M 自动换算倍率；基准价变动会同步影响单价。',
      metadataTitle: '模型信息（可选）', metadataHint: '规格可由预设带入，无需逐项填写。',
      metadataConfigured: '已有规格，按需调整即可。',
      advancedTitle: '更多计价与路由（可选）', advancedHint: '仅需特殊价格或降级规则时展开。',
      advancedConfigured: '已有配置，保存时保留。', modalTitle: '多模态计价',
    },
  },
}

/** Compose only model editor keys; do not replace other admin or locale resources. */
export function withModelEditorZh<T extends { admin: { modelMeta: object; modelSimple: object } }>(base: T) {
  return { ...base, admin: {
    ...base.admin, ...modelAdminZh.admin,
    modelMeta: { ...base.admin.modelMeta, ...modelAdminZh.admin.modelMeta },
    modelSimple: { ...base.admin.modelSimple, ...modelAdminZh.admin.modelSimple },
  } }
}
