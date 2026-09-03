import { ChevronLeft, ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

interface PaginationProps {
  total: number
  limit: number
  offset: number
  onOffset: (offset: number) => void
  className?: string
}

/// 页码序列：首尾 + 当前页邻域，其余折成省略号（null）。
function pageList(current: number, pages: number): (number | null)[] {
  if (pages <= 7) return Array.from({ length: pages }, (_, i) => i + 1)
  const set = new Set<number>([1, pages, current - 1, current, current + 1])
  if (current <= 3) [2, 3, 4].forEach((p) => set.add(p))
  if (current >= pages - 2) [pages - 3, pages - 2, pages - 1].forEach((p) => set.add(p))
  const sorted = [...set].filter((p) => p >= 1 && p <= pages).sort((a, b) => a - b)
  const out: (number | null)[] = []
  for (let i = 0; i < sorted.length; i++) {
    if (i > 0 && sorted[i] - sorted[i - 1] > 1) out.push(null)
    out.push(sorted[i])
  }
  return out
}

/// 分页器。后端列表一直返回 `total`，但此前前端只取首页——数据超过一页就再也翻不到，
/// 管理员只能改 URL 参数。
///
/// 页码只在 ≤ 十来页时全部铺开、更多时折省略号：管理面列表带搜索与过滤，
/// 实际使用是"筛小再看"，但"跳到最后一页看最新/最老"这种需求确实存在。
export function Pagination({ total, limit, offset, onOffset, className }: PaginationProps) {
  const { t } = useTranslation()
  if (total <= limit) return null
  const pages = Math.ceil(total / limit)
  const current = Math.floor(offset / limit) + 1
  const from = offset + 1
  const to = Math.min(offset + limit, total)
  return (
    <nav
      aria-label={t('common:pagination')}
      className={cn('flex flex-wrap items-center justify-between gap-3', className)}
    >
      <span className="text-xs text-muted-foreground tabular-nums">
        {t('common:pageRange', { from, to, total })}
      </span>
      <div className="flex items-center gap-1">
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          disabled={current === 1}
          aria-label={t('common:prevPage')}
          onClick={() => onOffset(Math.max(0, offset - limit))}
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        {pageList(current, pages).map((p, i) =>
          p === null ? (
            <span key={`gap-${i}`} className="px-1 text-xs text-muted-foreground">
              …
            </span>
          ) : (
            <Button
              key={p}
              variant={p === current ? 'default' : 'ghost'}
              size="icon"
              className="h-8 min-w-8 px-2 text-xs tabular-nums"
              aria-current={p === current ? 'page' : undefined}
              onClick={() => onOffset((p - 1) * limit)}
            >
              {p}
            </Button>
          ),
        )}
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          disabled={current >= pages}
          aria-label={t('common:nextPage')}
          onClick={() => onOffset(offset + limit)}
        >
          <ChevronRight className="h-4 w-4" />
        </Button>
      </div>
    </nav>
  )
}
