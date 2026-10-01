import { ArrowDownRight, ArrowUpRight, Minus } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { StatSkeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

export type StatTone = 'default' | 'good' | 'warn' | 'bad' | 'info'

interface StatProps {
  icon?: LucideIcon
  label: React.ReactNode
  value: React.ReactNode
  /// 副行：对照锚点（昨日 / 窗口累计）或一句解释。
  sub?: React.ReactNode
  tone?: StatTone
  /// 数据未回来时渲染同尺寸骨架，避免卡片从空壳跳到有字。
  loading?: boolean
  /// 右侧附加区（sparkline / 徽章）。
  aside?: React.ReactNode
  className?: string
  /// 只覆盖图标块的底色与字色（不影响数值颜色）：同一排数字卡用不同色相区分指标，而不是一排同色。
  iconClassName?: string
  /// 窄卡片将图标放在标签行，给数值保留整行宽度。
  layout?: 'inline' | 'stacked'
  compact?: boolean
  /// 可点击（跳转到明细）。
  onClick?: () => void
}

const TONE_ICON: Record<StatTone, string> = {
  default: 'bg-primary/10 text-primary',
  good: 'bg-success/12 text-success',
  warn: 'bg-warning/14 text-warning',
  bad: 'bg-destructive/12 text-destructive',
  info: 'bg-info/12 text-info',
}

const TONE_VALUE: Record<StatTone, string> = {
  default: '',
  good: 'text-success',
  warn: 'text-warning',
  bad: 'text-destructive',
  info: '',
}

/// 统一的 KPI 卡。
///
/// 此前总览、门户看板、实时条、日志统计条各自手写了一版几乎相同的"图标 + 标签 +
/// 大数字 + 小字"，字号/间距/色调三处三样。统一后同一站点里的"数字卡"只有一种长相，
/// 也才有机会给它们统一补上骨架屏。
export function Stat({
  icon: Icon,
  label,
  value,
  sub,
  tone = 'default',
  loading = false,
  aside,
  className,
  iconClassName,
  layout = 'inline',
  compact = false,
  onClick,
}: StatProps) {
  if (loading) return <StatSkeleton layout={layout} compact={compact} className={className} icon={!!Icon} sub={sub !== undefined && sub !== ''} />
  const Comp = onClick ? 'button' : 'div'
  return (
    <Comp
      type={onClick ? 'button' : undefined}
      onClick={onClick}
      className={cn(
        'relative flex items-start gap-3 rounded-lg border border-border bg-card p-4 text-left shadow-card',
        compact && 'p-3',
        onClick && 'transition-colors hover:border-primary/40 hover:bg-accent/40',
        className,
      )}
    >
      {Icon && (
        <span className={cn('mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-md', layout === 'stacked' && 'absolute right-4 top-3 h-7 w-7', iconClassName ?? TONE_ICON[tone])}>
          <Icon className="h-4 w-4" />
        </span>
      )}
      <div className="flex min-w-0 flex-1 flex-col">
        <span className={cn('text-xs font-medium text-muted-foreground', layout === 'stacked' ? 'min-h-7 pr-8' : 'truncate', compact && layout === 'stacked' && 'min-h-5')}>{label}</span>
        <span title={typeof value === 'string' ? value : undefined} className={cn('text-xl font-semibold tracking-tight tabular-nums', layout === 'stacked' ? 'break-words' : 'truncate', TONE_VALUE[tone])}>
          {value}
        </span>
        {sub !== undefined && sub !== '' && (
          <span className="mt-0.5 flex flex-wrap gap-x-2 text-xs text-muted-foreground">{sub}</span>
        )}
      </div>
      {aside}
    </Comp>
  )
}

/// 一行紧凑数字（实时条 / 统计条）：无卡片边框，靠间距分组。
export function InlineStat({
  label,
  value,
  tone = 'default',
  className,
}: {
  label: string
  value: React.ReactNode
  tone?: StatTone
  className?: string
}) {
  return (
    <div className={cn('flex min-w-0 flex-col', className)}>
      <span className="text-[11px] font-medium text-muted-foreground">{label}</span>
      <span className={cn('text-base font-semibold tabular-nums', TONE_VALUE[tone])}>{value}</span>
    </div>
  )
}

/// 环比芯片：把"今天 1,560 / 昨日 1,678"这道心算交给界面来做。
///
/// 为什么需要：KPI 卡的副行一直给着昨日与窗口两个锚点，但"涨了还是跌了、涨多少"
/// 得读数的人自己两位两位比——五张卡就是五次心算，而这恰恰是打开总览页第一眼想知道的。
///
/// 极性可反转：错误率涨是坏事、跌是好事，跟请求数正相反；`invert` 让颜色跟着语义走
/// 而不是跟着箭头方向走。基数为 0 时不出芯片——"从 0 涨到 5"没有百分比可言，
/// 硬写 +∞% 只会添噪。
export function DeltaChip({
  current,
  previous,
  invert = false,
  locale,
}: {
  current: number
  previous: number
  /// true = 数值变大是坏事（错误率、延迟）。
  invert?: boolean
  locale: string
}) {
  if (previous === 0) return null
  const pct = ((current - previous) / Math.abs(previous)) * 100
  // ±0.5% 以内当持平：整点抖动不该被涂成红绿
  if (Math.abs(pct) < 0.5) {
    return (
      <span className="inline-flex items-center gap-0.5 rounded bg-muted/70 px-1 text-muted-foreground">
        <Minus className="h-3 w-3" />
        0%
      </span>
    )
  }
  const up = pct > 0
  const good = invert ? !up : up
  const Icon = up ? ArrowUpRight : ArrowDownRight
  // 基数很小时涨幅会是 24,927,436.2% 这种没有信息量的数：千倍以上统一封顶成 999%+，完整数值留在悬停提示里。
  const magnitude = Math.abs(pct)
  const exact = `${magnitude.toLocaleString(locale, { maximumFractionDigits: 1 })}%`
  return (
    <span
      title={magnitude >= 1000 ? exact : undefined}
      className={cn(
        'inline-flex items-center gap-0.5 rounded px-1 font-medium tabular-nums',
        good ? 'bg-success/10 text-success' : 'bg-destructive/10 text-destructive',
      )}
    >
      <Icon className="h-3 w-3" />
      {magnitude >= 1000 ? '999%+' : exact}
    </span>
  )
}
