import type { LucideIcon } from 'lucide-react'
import { StatSkeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

export type StatTone = 'default' | 'good' | 'warn' | 'bad' | 'info'

interface StatProps {
  icon?: LucideIcon
  label: string
  value: React.ReactNode
  /// 副行：对照锚点（昨日 / 窗口累计）或一句解释。
  sub?: React.ReactNode
  tone?: StatTone
  /// 数据未回来时渲染同尺寸骨架，避免卡片从空壳跳到有字。
  loading?: boolean
  /// 右侧附加区（sparkline / 徽章）。
  aside?: React.ReactNode
  className?: string
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
  onClick,
}: StatProps) {
  if (loading) return <StatSkeleton />
  const Comp = onClick ? 'button' : 'div'
  return (
    <Comp
      type={onClick ? 'button' : undefined}
      onClick={onClick}
      className={cn(
        'flex items-start gap-3 rounded-lg border border-border bg-card p-4 text-left shadow-card',
        onClick && 'transition-colors hover:border-primary/40 hover:bg-accent/40',
        className,
      )}
    >
      {Icon && (
        <span className={cn('mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-md', TONE_ICON[tone])}>
          <Icon className="h-4 w-4" />
        </span>
      )}
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-xs font-medium text-muted-foreground">{label}</span>
        <span className={cn('truncate text-xl font-semibold tracking-tight', TONE_VALUE[tone])}>
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
