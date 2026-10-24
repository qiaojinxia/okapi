import { Skeleton } from '@/components/ui/skeleton'
import { Sparkline } from '@/components/ui/sparkline'
import { cn } from '@/lib/utils'

/// 指标条的外框：一张卡，格子之间 1px 分隔线（gap-px 透出底色）。
/// 数字条与调用质量条共用，随所在容器的宽度折成 4 / 2 / 1 列（容器查询，不靠视口断点）；
/// 四格只会 4 列、2×2 或单列，不会出现 3+1 的孤格把底色露出来。外层要套一层 `@container`。
export const STRIP = 'grid min-w-0 grid-cols-1 gap-px overflow-hidden rounded-xl border border-border bg-border @xl:grid-cols-2 @4xl:grid-cols-4'

export interface KpiCell {
  label: string
  value: string
  /// 右上角的小胶囊（较前一周期的涨跌、余额还能用几天）；完整说明放 `pillTitle`。
  pill?: string
  pillTitle?: string
  pillTone?: 'default' | 'warn' | 'bad'
  sub?: React.ReactNode
  /// 数值的颜色（余额临近耗尽转黄 / 转红）。
  tone?: 'default' | 'warn' | 'bad'
  /// 逐日数值的迷你趋势（线条，不填充）；没有就不画。
  spark?: number[]
  /// 悬停和测试用的稳定标识。
  slot?: string
}

const PILL = { default: 'bg-muted text-foreground/70', warn: 'bg-warning/15 text-warning', bad: 'bg-destructive/10 text-destructive' } as const
const VALUE = { default: '', warn: 'text-warning', bad: 'text-destructive' } as const

/// 总览的数字条：Tokens / 请求数 / 消费 / 账户余额。
/// 数据没回来时每格给同尺寸骨架，数据回来页面不跳。
export function KpiStrip({ label, cells, loading }: { label: string; cells: (KpiCell & { loading?: boolean })[]; loading: boolean }) {
  return <div className="@container min-w-0"><section aria-label={label} aria-busy={loading} className={STRIP}>
    {cells.map((cell) => <div key={cell.label} data-slot={cell.slot} className="@container flex min-w-0 flex-col gap-2.5 bg-card px-5 py-5">
      <div className="flex min-w-0 items-center justify-between gap-2 text-[13px] text-muted-foreground">
        <span className="min-w-0 truncate">{cell.label}</span>
        {!cell.loading && cell.pill && <span title={cell.pillTitle} className={cn('max-w-full shrink-0 truncate rounded-full px-2 py-0.5 text-xs tabular-nums', PILL[cell.pillTone ?? 'default'])}>{cell.pill}</span>}
      </div>
      {cell.loading ? <>
        <Skeleton className="h-8 w-32" />
        <Skeleton className="h-3.5 w-36" />
      </> : <>
        <div className="flex min-w-0 items-end justify-between gap-3">
          <span title={cell.value} className={cn('min-w-0 text-[26px] leading-8 font-semibold tracking-tight tabular-nums [overflow-wrap:anywhere]', VALUE[cell.tone ?? 'default'])}>{cell.value}</span>
          {cell.spark && <Sparkline values={cell.spark} area={false} className="h-8 w-[72px] shrink-0 @max-[13rem]:hidden" />}
        </div>
        {cell.sub !== undefined && cell.sub !== '' && <div className="flex flex-wrap gap-x-2 text-xs text-muted-foreground/80 tabular-nums">{cell.sub}</div>}
      </>}
    </div>)}
  </section></div>
}
