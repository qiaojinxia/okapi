import type { LucideIcon } from 'lucide-react'
import { CopyButton } from '@/components/ui/copy-button'
import { cn } from '@/lib/utils'

/// 日志详情（用户端 / 管理端共用）的信息展示原语。
///
/// 此前详情里有的分区带边框、有的只有一条分隔线，字段有的等宽小字、有的常规字，
/// 读起来像十几块风格各异的拼贴。这里统一成：卡片分区（图标 + 标题）→ 键值网格 → 数值/标识的固定样式。
/// 只负责样式，不产生任何文案——文案仍由各业务组件经 t() 传入。

/// 详情统一使用站点正文：标签 12px、字段 13px / 20px，只有标题和总金额加重。
/// 等宽字体留给原始快照，不让名称、模型、路径和请求 ID 各自换一套字体。
export function DetailBody({ id, children }: { id?: string; children: React.ReactNode }) {
  return <div id={id} data-slot="log-detail-body" className="flex min-w-0 flex-col gap-3 font-sans text-[13px] leading-5 font-normal">{children}</div>
}

export function DetailAmount({ label, value, children }: {
  label: string
  value: string
  children?: React.ReactNode
}) {
  return (
    <div data-slot="detail-amount" className="grid gap-3 rounded-xl border border-primary/15 bg-gradient-to-br from-primary/5 via-card to-card p-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
      <dl className="min-w-0">
        <dt className="text-xs leading-5 text-muted-foreground">{label}</dt>
        <dd data-slot="detail-amount-value" className="mt-1 break-words text-2xl leading-8 font-semibold tracking-tight tabular-nums">{value}</dd>
      </dl>
      {children}
    </div>
  )
}

/// 分区卡片：图标标题栏 + 内容区。根节点是 `section`，标题是 `h3`（既有用例按标题取分区）。
export function DetailSection({ title, hint, icon: Icon, className, children }: {
  title: string
  hint?: string
  icon?: LucideIcon
  className?: string
  children: React.ReactNode
}) {
  return (
    <section data-slot="detail-section" className={cn('min-w-0 overflow-hidden rounded-xl border border-border bg-card', className)}>
      <header className="flex items-start gap-2 border-b border-border/60 bg-muted/20 px-4 py-2.5">
        {Icon && <span aria-hidden className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-md bg-primary/8 text-primary"><Icon className="h-3.5 w-3.5" /></span>}
        <div className="min-w-0">
          <h3 className="text-sm leading-6 font-semibold">{title}</h3>
          {hint !== undefined && <p className="text-xs leading-5 text-muted-foreground">{hint}</p>}
        </div>
      </header>
      <div className="flex min-w-0 flex-col gap-3 p-4">{children}</div>
    </section>
  )
}

/// 键值网格。`dt` / `dd` 必须同属一个 `div`（既有用例用 `locator('..')` 从标签取值）。
export function InfoGrid({ cols = 3, label, className, children }: {
  cols?: 2 | 3 | 4
  label?: string
  className?: string
  children: React.ReactNode
}) {
  return (
    <dl aria-label={label} className={cn('grid grid-cols-2 gap-x-5 gap-y-3', cols === 3 && 'sm:grid-cols-3', cols === 4 && 'sm:grid-cols-4', className)}>
      {children}
    </dl>
  )
}

const DOT = { info: 'bg-info', success: 'bg-success', primary: 'bg-primary', warning: 'bg-warning', muted: 'bg-muted-foreground/50' } as const
export type InfoDot = keyof typeof DOT

/// 一个字段：小号弱化标签 + 常规字重数值。`mono` 用于标识 / 代码；`wide` 占满整行；
/// `dot` 在标签前放一个色块，与同区的堆叠条互为图例；`copy` 在值后给复制按钮。
export function InfoItem({ label, children, mono = false, strong = false, wide = false, dot, copy, className }: {
  label: React.ReactNode
  children: React.ReactNode
  mono?: boolean
  strong?: boolean
  wide?: boolean
  dot?: InfoDot
  copy?: string
  className?: string
}) {
  return (
    <div data-slot="detail-field" className={cn('min-w-0 space-y-0.5', wide && 'col-span-full', className)}>
      <dt className="flex items-center gap-1.5 text-xs leading-5 text-muted-foreground">
        {dot && <span aria-hidden className={cn('h-2 w-2 shrink-0 rounded-sm', DOT[dot])} />}
        {label}
      </dt>
      <dd className={cn('flex min-w-0 items-start gap-2 text-[13px] leading-5 tabular-nums', strong ? 'font-semibold' : 'font-normal', mono && 'font-mono')}>
        <span className="min-w-0 flex-1 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{children}</span>
        {copy && <CopyButton value={copy} size="xs" />}
      </dd>
    </div>
  )
}

/// 标识行：沿用字段字体和字号；复制按钮靠右，完整 ID 折行而不撑破抽屉。
export function IdRow({ label, value }: { label: string; value: string }) {
  return (
    <div data-slot="detail-id" className="min-w-0 rounded-lg bg-muted/35 px-3 py-2">
      <dt className="mb-0.5 text-xs leading-5 text-muted-foreground">{label}</dt>
      <dd className="flex min-w-0 items-start gap-2 text-[13px] leading-5 font-normal tabular-nums">
        <span className="min-w-0 flex-1 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{value}</span>
        <CopyButton value={value} size="xs" />
      </dd>
    </div>
  )
}

/// 指标磁贴：与其他字段同字号，靠字重强调数值。`dt` / `dd` 同属一个 `div`。
export function StatTile({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div data-slot="detail-stat" className="min-w-0 rounded-lg border border-border/60 bg-muted/20 px-3 py-2.5">
      <dt className="text-xs leading-5 text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 break-words text-[13px] leading-5 font-medium tabular-nums">{children}</dd>
    </div>
  )
}
