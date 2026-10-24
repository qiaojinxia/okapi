import { cn } from '@/lib/utils'

/// 排行里的占比条：6px 高，宽度是占整个时段的比例（基点），再小也留 2px 看得见。
/// 排行靠透明度递减表达名次（同一个色相），构成类的行用 `className` 指定各自的颜色。
/// `data-share-bp` 带着原始基点，便于核对画出来的比例没有被取整或截断。
export function ShareBar({ bp, opacity = 1, className }: { bp: number | null; opacity?: number; className?: string }) {
  const width = bp == null ? 0 : Math.min(100, Math.max(0, bp / 100))
  return <span aria-hidden data-slot="share-bar" className="block h-1.5 overflow-hidden rounded-full bg-muted">
    <span data-share-bp={bp ?? undefined} className={cn('block h-full min-w-0.5 rounded-full', className ?? 'bg-primary')} style={{ width: `${width}%`, opacity }} />
  </span>
}
