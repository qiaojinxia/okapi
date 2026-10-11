/// 总览用的人读格式：金额两位小数（不足一美分才多给有效位），时长过一秒换成秒。
/// 明细页仍用 `formatMoney` 的四位精度——那里要能对账。
export function formatUsd(micro: number, locale: string): string {
  const usd = micro / 1_000_000
  const tiny = usd !== 0 && Math.abs(usd) < 0.01
  return new Intl.NumberFormat(locale, {
    style: 'currency',
    currency: 'USD',
    ...(tiny ? { maximumSignificantDigits: 2 } : { minimumFractionDigits: 2, maximumFractionDigits: 2 }),
  }).format(usd)
}

export function formatDuration(ms: number, locale: string): string {
  if (ms < 1000) return `${Math.round(ms).toLocaleString(locale)} ms`
  return `${(ms / 1000).toLocaleString(locale, { maximumFractionDigits: ms < 10_000 ? 1 : 0 })} s`
}

/// 与上一段等长窗口相比的变化；上期为 0 时没有可比的百分比。
export function delta(current: number, previous: number | undefined): number | null {
  if (previous === undefined || previous <= 0) return null
  return (current - previous) / previous
}
