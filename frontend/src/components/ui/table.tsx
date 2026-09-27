import { useId, useLayoutEffect, useRef, useState } from 'react'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

interface TableProps extends React.TableHTMLAttributes<HTMLTableElement> {
  /// 外框类名（滚动容器）。
  wrapperClassName?: string
  /// 紧凑行高：日志这类一屏要看几十行的表。
  dense?: boolean
  /// 表头随容器滚动固定。不另给 `wrapperClassName` 时自带一个视口高度上限——
  /// 只写 `stickyHeader` 却忘了限高，表头没有可粘的滚动容器，等于没开。
  stickyHeader?: boolean
  /// 明确以名称为第一列的明细表可固定该列，横向读数时保留行上下文。
  stickyFirstColumn?: boolean
  /// 换页或筛选后从首行开始看，保留宽表的横向滚动位置。
  scrollResetKey?: string
}

/// 表格外框：白底卡片 + 圆角 + 横向滚动。表头/行样式由子组件负责。
export function Table({
  className,
  wrapperClassName,
  dense = false,
  stickyHeader = false,
  stickyFirstColumn = false,
  scrollResetKey,
  ...props
}: TableProps) {
  const { t } = useTranslation()
  const scrollId = useId()
  const wrapper = useRef<HTMLDivElement>(null)
  const table = useRef<HTMLTableElement>(null)
  const [scroll, setScroll] = useState({ horizontal: false, vertical: false, back: false, forward: false })
  useLayoutEffect(() => {
    const viewport = wrapper.current, content = table.current
    if (!viewport || !content) return
    const measure = () => {
      const next = {
        horizontal: viewport.scrollWidth > viewport.clientWidth + 1,
        vertical: viewport.scrollHeight > viewport.clientHeight + 1,
        back: viewport.scrollLeft > 1,
        forward: viewport.scrollLeft + viewport.clientWidth < viewport.scrollWidth - 1,
      }
      setScroll((previous) => previous.horizontal === next.horizontal && previous.vertical === next.vertical && previous.back === next.back && previous.forward === next.forward ? previous : next)
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(viewport)
    observer.observe(content)
    viewport.addEventListener('scroll', measure, { passive: true })
    return () => { observer.disconnect(); viewport.removeEventListener('scroll', measure) }
  }, [scroll.horizontal])
  useLayoutEffect(() => {
    if (scrollResetKey !== undefined && wrapper.current) wrapper.current.scrollTop = 0
  }, [scrollResetKey])
  const move = (direction: number) => {
    const viewport = wrapper.current
    if (!viewport) return
    const firstCell = table.current?.rows[0]?.cells[0]
    const pinnedWidth = stickyFirstColumn && firstCell && !firstCell.hasAttribute('colspan') ? firstCell.getBoundingClientRect().width : 0
    // 按真正可读的区域翻列，留出重叠，避免跳过固定列背后的数据。
    const readableWidth = Math.max(1, viewport.clientWidth - pinnedWidth)
    viewport.scrollBy({ left: direction * Math.max(1, Math.floor(readableWidth * 0.75)), behavior: 'instant' })
  }
  return (
    <div
      data-slot="table-frame"
      data-density={dense ? 'compact' : 'default'}
      className={cn(
        'relative flex min-w-0 w-full flex-col overflow-hidden rounded-xl border border-border bg-card shadow-card',
        // 限高由调用方覆盖；缺省留出顶栏 + 页头 + 工具条的高度，长列表才不会把分页器推到天边。
        // 嵌入式明细按内容收缩；独立列表由 .list-page 统一分配剩余高度。
        stickyHeader && 'max-h-[max(12rem,calc(100dvh-19rem))]',
        wrapperClassName,
      )}
    >
      {/* 控件不放进滚动内容，鼠标、键盘或辅助技术聚焦时都不会改变表格位置。 */}
      {scroll.horizontal && <div role="group" aria-label={t('common:tableScrollControls')} className="flex h-11 w-full shrink-0 items-center justify-between gap-2 border-b border-border bg-card px-2 md:h-9">
        <span id={`${scrollId}-hint`} className="min-w-0 text-xs leading-4 text-muted-foreground">{t('common:tableScrollHint')}</span>
        <div className="flex shrink-0 items-center gap-1">
          <button type="button" aria-label={t('common:previousColumns')} aria-controls={scrollId} disabled={!scroll.back} onClick={() => move(-1)} className="flex h-11 w-11 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40 disabled:opacity-35 md:h-8 md:w-8"><ChevronLeft aria-hidden className="h-4 w-4" /></button>
          <button type="button" aria-label={t('common:nextColumns')} aria-controls={scrollId} disabled={!scroll.forward} onClick={() => move(1)} className="flex h-11 w-11 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40 disabled:opacity-35 md:h-8 md:w-8"><ChevronRight aria-hidden className="h-4 w-4" /></button>
        </div>
      </div>}
      <div ref={wrapper} id={scrollId} data-slot="table-viewport" className="min-h-0 min-w-0 w-full flex-1 overflow-auto overscroll-contain focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary/40"
        tabIndex={scroll.horizontal || scroll.vertical ? 0 : undefined}
        role={scroll.horizontal || scroll.vertical ? 'region' : undefined}
        aria-label={scroll.horizontal || scroll.vertical ? t('common:scrollableTable', { name: props['aria-label'] ?? t('common:table') }) : undefined}
        aria-describedby={scroll.horizontal ? `${scrollId}-hint` : undefined}>
      <table
        ref={table}
        data-slot="table"
        className={cn(
          'w-full caption-bottom text-[length:var(--table-font-size)] leading-[var(--table-line-height)]',
          // 粘性表头保持不透明，行不会从表头底下透出来。
          // 另外 sticky 元素上的 border-bottom 不随边框合并渲染，靠 th 的 box-shadow 补一条分隔线。
          stickyHeader &&
            '[&_thead]:sticky [&_thead]:top-0 [&_thead]:z-10 [&_thead]:bg-card [&_thead_th]:bg-muted [&_thead_th]:shadow-[inset_0_-1px_0_var(--border)]',
          stickyFirstColumn && '[&_tr>:first-child:not([colspan])]:sticky [&_tr>:first-child:not([colspan])]:left-0 [&_tr>:first-child:not([colspan])]:z-1 [&_tr>:first-child:not([colspan])]:shadow-[1px_0_0_var(--border)] [&_tbody_tr>:first-child:not([colspan])]:bg-card [&_thead_tr>:first-child:not([colspan])]:bg-muted',
          className,
        )}
        {...props}
      />
      </div>
    </div>
  )
}

export function THead({ className, ...props }: React.HTMLAttributes<HTMLTableSectionElement>) {
  return <thead className={cn('bg-muted [&_tr]:border-b [&_tr]:border-border', className)} {...props} />
}

export function TBody({ className, ...props }: React.HTMLAttributes<HTMLTableSectionElement>) {
  return <tbody className={cn('divide-y divide-border [&_tr:last-child]:border-0', className)} {...props} />
}

interface TrProps extends React.HTMLAttributes<HTMLTableRowElement> {
  /// 选中态（批量勾选）：整行提色，比只看行首的勾选框更容易确认选了哪几条。
  selected?: boolean
}

export function Tr({ className, selected = false, ...props }: TrProps) {
  return (
    <tr
      data-selected={selected || undefined}
      className={cn(
        'transition-colors hover:bg-accent/40 data-[selected]:bg-primary/5 data-[selected]:hover:bg-primary/8',
        className,
      )}
      {...props}
    />
  )
}

interface ThProps extends React.ThHTMLAttributes<HTMLTableCellElement> {
  /// 数字列右对齐（金额/次数）；表头与单元格一并右对齐才对得齐小数点。
  numeric?: boolean
}

export function Th({ className, numeric = false, ...props }: ThProps) {
  return (
    <th
      className={cn(
        'h-[var(--table-header-height)] px-3 py-2 text-left text-xs leading-4 font-semibold align-middle whitespace-nowrap text-muted-foreground',
        numeric && 'text-right',
        className,
      )}
      {...props}
    />
  )
}

interface TdProps extends React.TdHTMLAttributes<HTMLTableCellElement> {
  numeric?: boolean
}

export function Td({ className, numeric = false, ...props }: TdProps) {
  return (
    <td
      className={cn(
        'h-[var(--table-row-height)] px-3 py-[var(--table-cell-padding)] align-middle',
        numeric && 'text-right tabular-nums whitespace-nowrap',
        className,
        // 单元格统一读数尺寸；二级说明仍可在内部元素使用 text-xs。
        !props.colSpan && 'text-[length:var(--table-font-size)] leading-[var(--table-line-height)]',
      )}
      {...props}
    />
  )
}
