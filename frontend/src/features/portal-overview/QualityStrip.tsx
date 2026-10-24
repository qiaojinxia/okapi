import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { STRIP } from './KpiStrip'

export interface QualityItem {
  label: string
  value: string
  /// 一行短说明；完整解释放在 `subTitle`（悬停可见）。
  sub?: string
  subTitle?: string
  /// 数值着色（成功率）：warn 转黄、bad 转红，与运营概览的质量指标同口径；不画进度条。
  tone?: 'good' | 'warn' | 'bad'
}

const VALUE_TONE = {
  good: '',
  warn: 'text-warning',
  bad: 'text-destructive',
} as const

/// 调用质量：四项指标挤在一张卡里，格子之间 1px 分隔线，比数字条矮一档（数值 18px、没有迷你趋势）。
/// 随宽度折成两列 / 一列；数值保留 title，便于悬停看全与按值定位。
export function QualityStrip({ label, items, loading }: { label: string; items: QualityItem[]; loading: boolean }) {
  return (
    <div className="@container min-w-0"><section aria-label={label} aria-busy={loading} className={STRIP}>
      {items.map((item) => (
        <div key={item.label} className="flex min-w-0 flex-col gap-1 bg-card px-5 py-3.5">
          <span className="text-xs text-muted-foreground">{item.label}</span>
          {loading ? (
            <Skeleton className="h-6 w-20" />
          ) : (
            <span title={item.value} className={cn('text-lg leading-7 font-semibold break-words tabular-nums', item.tone && VALUE_TONE[item.tone])}>
              {item.value}
            </span>
          )}
          {!loading && item.sub && (
            <span title={item.subTitle ?? item.sub} className="line-clamp-2 text-xs text-muted-foreground/80 tabular-nums">
              {item.sub}
            </span>
          )}
        </div>
      ))}
    </section></div>
  )
}
