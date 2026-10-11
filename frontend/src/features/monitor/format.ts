import type { StatTone } from '@/components/ui/stat'

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']

export function formatBytes(n: number | null | undefined, locale: string): string {
  if (n == null || !Number.isFinite(n)) return '—'
  let value = Math.abs(n)
  let unit = 0
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1
  return `${n < 0 ? '-' : ''}${new Intl.NumberFormat(locale, { maximumFractionDigits: digits }).format(value)} ${UNITS[unit]}`
}

/// 一组字节数共用的显示单位：按最大值挑，整组除以同一个基数（图表纵轴用）。
export function byteScale(values: (number | null | undefined)[]): { unit: string; scale: (n: number | null | undefined) => number | null } {
  const max = Math.max(0, ...values.filter((v): v is number => v != null && Number.isFinite(v)))
  let unit = 0
  while (max / 1024 ** (unit + 1) >= 1 && unit < UNITS.length - 1) unit += 1
  const base = 1024 ** unit
  return { unit: UNITS[unit], scale: (n) => (n == null ? null : Math.round((n / base) * 100) / 100) }
}

export function formatRate(bps: number | null | undefined, locale: string): string {
  return bps == null ? '—' : `${formatBytes(bps, locale)}/s`
}

export function formatPercent(n: number | null | undefined, locale: string, digits = 1): string {
  if (n == null || !Number.isFinite(n)) return '—'
  return `${new Intl.NumberFormat(locale, { maximumFractionDigits: digits }).format(n)}%`
}

export function formatCount(n: number | null | undefined, locale: string): string {
  if (n == null || !Number.isFinite(n)) return '—'
  return new Intl.NumberFormat(locale, { notation: Math.abs(n) >= 100_000 ? 'compact' : 'standard', maximumFractionDigits: 1 }).format(n)
}

/// 运行时长：取最大的两级单位（3 天 4 小时 / 5 分钟）。
export function formatUptime(secs: number | null | undefined, t: (key: string, o?: Record<string, unknown>) => string): string {
  if (secs == null || !Number.isFinite(secs)) return '—'
  const days = Math.floor(secs / 86400)
  const hours = Math.floor((secs % 86400) / 3600)
  const minutes = Math.floor((secs % 3600) / 60)
  if (days > 0) return t('monitor:uptimeDays', { days, hours })
  if (hours > 0) return t('monitor:uptimeHours', { hours, minutes })
  return t('monitor:uptimeMinutes', { minutes: Math.max(minutes, 0) })
}

/// 占用率 → 状态：≥ 90% 危险、≥ 75% 警告。没有数（不支持 / 不可达）按默认。
export function usageTone(percent: number | null | undefined): StatTone {
  if (percent == null) return 'default'
  if (percent >= 90) return 'bad'
  if (percent >= 75) return 'warn'
  return 'good'
}

export function ratio(used: number | null | undefined, total: number | null | undefined): number | null {
  return used == null || !total ? null : (used / total) * 100
}
