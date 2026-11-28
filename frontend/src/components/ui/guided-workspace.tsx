import { Check, Circle } from 'lucide-react'
import type { ReactNode } from 'react'
import { Card } from '@/components/ui/card'
import { Steps, type StepItem } from '@/components/ui/steps'
import { cn } from '@/lib/utils'

interface GuidedWorkspaceProps {
  /// 工作区标题（供语义区域与页内卡片标题展示）。
  title: string
  description?: string
  icon?: ReactNode
  /// 标题区右侧控件（如引导模式 / 完整表单切换、重置按钮）。
  headerAction?: ReactNode
  /// 分步引导条（仅引导模式传；切到完整表单模式时可省略）。
  steps?: {
    items: StepItem[]
    active: string
    reachable: number
    onSelect: (id: string) => void
    ariaLabel: string
  }
  /// 右侧实时预览 / 检查清单侧栏（桌面端 sticky 并排，窄屏自然落在主区下方）。
  aside?: ReactNode
  /// 底部固定操作栏左侧状态提示（例如当前步骤还缺哪些必填项）。
  hint?: string | null
  /// 底部固定操作栏动作区（上一步 / 下一步 / 创建等）。
  footer: ReactNode
  dataSlot?: string
  className?: string
  children: ReactNode
}

/// 页内引导式工作区壳子：承载复杂资源的新建与分步向导（如新建渠道、策略编排等）。
///
/// 相比窄右侧抽屉，页内工作区作为页面主内容区的一枚页签呈现：
/// - 顶部统一卡片头 + 可选分步引导条（`Steps variant="cards"`）+ 模式切换；
/// - 中部双栏布局：左侧宽敞表单分区（复用 `.form-sections` 卡片节奏），右侧实时摘要与就绪检查清单；
/// - 底部吸底操作栏：缺项提示与主次按钮始终可见，长表单滚动不丢操作出口。
export function GuidedWorkspace({
  title,
  description,
  icon,
  headerAction,
  steps,
  aside,
  hint,
  footer,
  dataSlot = 'guided-workspace',
  className,
  children,
}: GuidedWorkspaceProps) {
  return (
    <section
      role="region"
      aria-label={title}
      data-slot={dataSlot}
      className={cn('flex min-w-0 flex-col gap-4 animate-fade-in', className)}
    >
      <Card className="overflow-hidden">
        <div className="flex flex-col gap-4 border-b border-border/80 bg-card px-5 py-4 sm:px-6">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="flex min-w-0 flex-1 items-start gap-3">
              {icon}
              <div className="flex min-w-0 flex-col gap-0.5 pt-0.5">
                <h2 className="truncate text-[17px] leading-6 font-semibold tracking-[-0.015em] text-foreground">
                  {title}
                </h2>
                {description !== undefined && (
                  <p className="text-xs leading-5 text-muted-foreground">{description}</p>
                )}
              </div>
            </div>
            {headerAction !== undefined && (
              <div className="flex shrink-0 flex-wrap items-center gap-2">{headerAction}</div>
            )}
          </div>
          {steps !== undefined && (
            <Steps
              variant="cards"
              ariaLabel={steps.ariaLabel}
              items={steps.items}
              active={steps.active}
              reachable={steps.reachable}
              onSelect={steps.onSelect}
            />
          )}
        </div>

        <div
          className={cn(
            'grid min-w-0 gap-6 p-5 sm:p-6',
            aside !== undefined && 'xl:grid-cols-[minmax(0,1fr)_20rem] xl:items-start',
          )}
        >
          <div className="min-w-0">{children}</div>
          {aside !== undefined && (
            <aside className="min-w-0 xl:sticky xl:top-4">{aside}</aside>
          )}
        </div>

        <footer className="sticky bottom-0 z-10 flex flex-wrap items-center justify-end gap-3 border-t border-border/85 bg-card/95 px-5 py-3.5 shadow-[0_-4px_16px_-12px_var(--muted-fg)] backdrop-blur-sm sm:px-6 [&>button]:min-h-10 sm:[&>button]:min-h-9">
          <p
            role="status"
            data-slot="step-hint"
            className="mr-auto min-w-0 basis-full text-xs leading-5 text-muted-foreground empty:hidden sm:basis-0 sm:flex-1"
          >
            {hint ?? ''}
          </p>
          {footer}
        </footer>
      </Card>
    </section>
  )
}

/// 引导模式下当前步骤的顶部说明条：用一句话明确这一步的目的与预期结果。
export function GuidedStepIntro({
  stepNumber,
  title,
  description,
}: {
  stepNumber: number
  title: string
  description: string
}) {
  return (
    <div className="mb-4 flex items-start gap-3 rounded-xl border border-primary/20 bg-primary/[0.04] px-4 py-3">
      <span
        aria-hidden
        className="mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-primary/12 text-xs font-semibold text-primary tabular-nums ring-1 ring-inset ring-primary/20"
      >
        {stepNumber}
      </span>
      <div className="min-w-0">
        <p className="text-sm font-medium text-foreground">{title}</p>
        <p className="mt-0.5 text-xs leading-5 text-muted-foreground">{description}</p>
      </div>
    </div>
  )
}

/// 右侧实时配置预览与就绪检查卡片：与全站次级卡片保持同一表面层次。
export function GuidedSummaryCard({
  title,
  subtitle,
  children,
  className,
}: {
  title: string
  subtitle?: string
  children: ReactNode
  className?: string
}) {
  return (
    <div
      data-slot="guided-summary"
      className={cn(
        'flex flex-col gap-4 rounded-xl border border-border/85 bg-muted/25 p-4 dark:bg-muted/20',
        className,
      )}
    >
      <div className="flex flex-col gap-0.5">
        <h3 className="text-xs font-semibold tracking-wide text-foreground">{title}</h3>
        {subtitle !== undefined && (
          <p className="text-[11px] leading-4 text-muted-foreground">{subtitle}</p>
        )}
      </div>
      {children}
    </div>
  )
}

export function GuidedSummaryRow({
  label,
  value,
  mono = false,
}: {
  label: string
  value: ReactNode
  mono?: boolean
}) {
  return (
    <div className="flex items-baseline justify-between gap-3 text-xs">
      <span className="shrink-0 text-muted-foreground">{label}</span>
      <span className={cn('min-w-0 truncate text-right font-medium text-foreground', mono && 'font-mono')}>
        {value}
      </span>
    </div>
  )
}

export interface GuidedChecklistItem {
  id: string
  label: string
  done: boolean
  detail?: string
  onClick?: () => void
  disabled?: boolean
}

export function GuidedChecklist({ items }: { items: GuidedChecklistItem[] }) {
  return (
    <ul className="flex flex-col gap-1.5">
      {items.map((item) => {
        const content = (
          <>
            <span
              aria-hidden
              className={cn(
                'mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-full',
                item.done
                  ? 'bg-success/15 text-success'
                  : 'text-muted-foreground/55',
              )}
            >
              {item.done ? <Check className="h-3 w-3" strokeWidth={2.5} /> : <Circle className="h-3 w-3" />}
            </span>
            <span className="flex min-w-0 flex-1 flex-col">
              <span className={cn('truncate text-xs', item.done ? 'font-medium text-foreground' : 'text-muted-foreground')}>
                {item.label}
              </span>
              {item.detail !== undefined && (
                <span className="truncate text-[11px] text-muted-foreground/85">{item.detail}</span>
              )}
            </span>
          </>
        )
        if (item.onClick && !item.disabled) {
          return (
            <li key={item.id}>
              <button
                type="button"
                onClick={item.onClick}
                className="flex w-full cursor-pointer items-start gap-2 rounded-lg px-2 py-1.5 text-left transition-colors outline-none hover:bg-card focus-visible:ring-2 focus-visible:ring-primary/40"
              >
                {content}
              </button>
            </li>
          )
        }
        return (
          <li key={item.id} className="flex items-start gap-2 px-2 py-1.5">
            {content}
          </li>
        )
      })}
    </ul>
  )
}
