import { cn } from '@/lib/utils'

interface TableProps extends React.TableHTMLAttributes<HTMLTableElement> {
  /// 外框类名（滚动容器）。
  wrapperClassName?: string
  /// 紧凑行高：日志这类一屏要看几十行的表。
  dense?: boolean
  /// 表头随容器滚动固定；容器需有高度上限才有意义（`wrapperClassName="max-h-[70vh]"`）。
  stickyHeader?: boolean
}

/// 表格外框：白底卡片 + 圆角 + 横向滚动。表头/行样式由子组件负责。
export function Table({
  className,
  wrapperClassName,
  dense = false,
  stickyHeader = false,
  ...props
}: TableProps) {
  return (
    <div
      className={cn(
        'relative w-full overflow-auto rounded-lg border border-border bg-card shadow-card',
        wrapperClassName,
      )}
    >
      <table
        className={cn(
          'w-full caption-bottom text-sm',
          dense ? '[&_td]:py-1.5 [&_th]:py-2' : '',
          stickyHeader && '[&_thead]:sticky [&_thead]:top-0 [&_thead]:z-10',
          className,
        )}
        {...props}
      />
    </div>
  )
}

export function THead({ className, ...props }: React.HTMLAttributes<HTMLTableSectionElement>) {
  return <thead className={cn('bg-muted/60 [&_tr]:border-b [&_tr]:border-border', className)} {...props} />
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
        'h-10 px-3 text-left text-xs font-semibold whitespace-nowrap text-muted-foreground',
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
      className={cn('px-3 py-2.5 align-middle', numeric && 'text-right tabular-nums', className)}
      {...props}
    />
  )
}
