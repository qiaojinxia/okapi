// 金额统一入口：后端一律 micro-USD 整数（$1 = 1_000_000），
// quota 视图 = USD × 500_000（仅展示层，DESIGN §3）。组件禁手写换算。

const QUOTA_PER_USD = 500_000

export function formatMoney(micro: number, locale: string): string {
  const usd = micro / 1_000_000
  return new Intl.NumberFormat(locale, {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: 2,
    maximumFractionDigits: 4,
  }).format(usd)
}

/// 聚合金额（KPI / 报表合计）。
///
/// 与 formatMoney 分开是刻意的：单笔调用可能只花 $0.0012，两位小数会显示成
/// $0.00 像是免费，故 formatMoney 保留四位；而站点级合计带四位小数（$485.4252）
/// 只是噪音，且金额大到百万时需要紧凑记法才不撑破卡片。
export function formatMoneyAggregate(micro: number, locale: string): string {
  const usd = micro / 1_000_000
  return new Intl.NumberFormat(locale, {
    style: 'currency',
    currency: 'USD',
    notation: Math.abs(usd) >= 1_000_000 ? 'compact' : 'standard',
    minimumFractionDigits: Math.abs(usd) >= 1_000_000 ? 0 : 2,
    maximumFractionDigits: 2,
  }).format(usd)
}

export function formatQuota(micro: number, locale: string): string {
  const quota = (micro / 1_000_000) * QUOTA_PER_USD
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(quota)
}

export function formatCount(n: number, locale: string): string {
  return new Intl.NumberFormat(locale, { notation: n >= 100_000 ? 'compact' : 'standard' }).format(n)
}

/// 倍率展示：后端 NUMERIC 原样下发（"1.000000" / "0.9000"），表格里一列六位零
/// 是噪音——去掉尾零、最多保留 4 位（对齐 new-api 倍率表的写法 1 / 2.5 / 0.075）。
/// 字符串处理不经浮点：倍率进 JS Number 再回字符串会把 0.075 变成 0.07500000000000001 之类。
export function formatRatio(raw: string | number | null | undefined): string {
  if (raw === null || raw === undefined || raw === '') return '—'
  const s = String(raw)
  if (!s.includes('.')) return s
  const [int, frac] = s.split('.')
  const trimmed = frac.slice(0, 4).replace(/0+$/, '')
  return trimmed === '' ? int : `${int}.${trimmed}`
}

/// 后端占比一律以基点（万分之一）整数下发，展示层统一转百分比。
export function formatBp(bp: number, locale: string): string {
  return new Intl.NumberFormat(locale, {
    style: 'percent',
    minimumFractionDigits: 1,
    maximumFractionDigits: 2,
  }).format(bp / 10_000)
}

/// 生成速度后端下发「每千秒 token 数」整数（避免浮点），展示层还原 tok/s。
export function formatTokensPerSec(per1kSec: number, locale: string): string {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(per1kSec / 1_000)
}

export interface SimulatorInput {
  modelRatio: number
  completionRatio: number
  cacheRatio: number
  /// 缓存写入倍率（Anthropic cache_creation；缺省 1 = 按常规输入计）。
  cacheWriteRatio?: number
  groupRatio: number
  promptTokens: number
  cachedTokens: number
  cacheWriteTokens?: number
  completionTokens: number
}

/// 定价模拟器（展示层估算；权威语义在后端 pricing 引擎）：
/// (常规×1 + 缓存读×cache + 缓存写×cacheWrite + 补全×completion) × model × group × $2/1M。
/// prompt 三段互斥，与后端 `TokenUsage::prompt_uncached()` 同口径。
export function simulateChargeMicro(input: SimulatorInput): number {
  const cached = Math.min(input.cachedTokens, input.promptTokens)
  const cacheWrite = Math.min(input.cacheWriteTokens ?? 0, input.promptTokens - cached)
  const weighted =
    (input.promptTokens - cached - cacheWrite) +
    cached * input.cacheRatio +
    cacheWrite * (input.cacheWriteRatio ?? 1) +
    input.completionTokens * input.completionRatio
  return Math.round(weighted * input.modelRatio * input.groupRatio * 2)
}
