import type { LucideIcon } from 'lucide-react'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

export interface QualityItem {
  icon: LucideIcon
  /// 图标块色相：四项各用一种，同 KPI 卡。
  accent: string
  label: string
  value: string
  /// 一行短说明；完整解释放在 `subTitle`（悬停可见）。
  sub?: string
  subTitle?: string
  /// 0–1 的占比条（成功率）；tone 决定颜色。
  meter?: { ratio: number; tone: 'good' | 'warn' | 'bad' }
}

const METER_TONE = {
  good: 'bg-success',
  warn: 'bg-warning',
  bad: 'bg-destructive',
} as const

/// 调用质量：四项指标挤在一张卡里，格子之间 1px 分隔线（gap-px 透出底色），不再是四张等高的大卡。
/// 窄屏两列、桌面一行四列；数值保留 title，便于悬停看全与按值定位。
export function QualityStrip({ label, items, loading }: { label: string; items: QualityItem[]; loading: boolean }) {
  return (
    <section
      aria-label={label}
      aria-busy={loading}
      className="grid min-w-0 grid-cols-2 gap-px overflow-hidden rounded-xl border border-border bg-border shadow-card xl:grid-cols-4"
    >
      {items.map((item) => (
        <div key={item.label} className="flex min-w-0 items-start gap-3 bg-card px-3 py-3 sm:px-4">
          {/* 窄屏两列时格子只有一百多像素宽：图标让位给数字，数字与标签宁可换行也不截断 */}
          <span className={cn('mt-0.5 hidden h-8 w-8 shrink-0 items-center justify-center rounded-lg sm:flex', item.accent)}>
            <item.icon aria-hidden className="h-4 w-4" />
          </span>
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="text-xs font-medium text-muted-foreground sm:truncate">{item.label}</span>
            {loading ? (
              <Skeleton className="mt-1 h-6 w-20" />
            ) : (
              <span title={item.value} className="text-lg font-semibold tracking-tight break-words tabular-nums">
                {item.value}
              </span>
            )}
            {!loading && item.meter && (
              <span aria-hidden className="mt-1 block h-1 w-full overflow-hidden rounded-full bg-muted">
                <span
                  className={cn('block h-full rounded-full', METER_TONE[item.meter.tone])}
                  style={{ width: `${Math.min(100, Math.max(0, item.meter.ratio * 100))}%` }}
                />
              </span>
            )}
            {!loading && item.sub && (
              <span title={item.subTitle ?? item.sub} className="mt-0.5 line-clamp-2 text-[11px] leading-4 text-muted-foreground">
                {item.sub}
              </span>
            )}
          </div>
        </div>
      ))}
    </section>
  )
}
