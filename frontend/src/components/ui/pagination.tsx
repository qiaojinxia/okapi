import { ChevronLeft, ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'

interface PaginationProps {
  total: number
  limit: number
  offset: number
  onOffset: (offset: number) => void
}

/// 分页器。后端列表一直返回 `total`，但此前前端只取首页——数据超过一页就再也翻不到，
/// 管理员只能改 URL 参数。
///
/// 不做页码跳转按钮：管理面列表带搜索与过滤，实际使用是"筛小再看"而非翻到第 37 页；
/// 上一页/下一页加计数已够，省掉一堆边界逻辑。
export function Pagination({ total, limit, offset, onOffset }: PaginationProps) {
  const { t } = useTranslation()
  if (total <= limit) return null
  const from = offset + 1
  const to = Math.min(offset + limit, total)
  return (
    <div className="flex items-center justify-end gap-2">
      <span className="text-xs text-muted-foreground">
        {t('common:pageRange', { from, to, total })}
      </span>
      <Button
        variant="outline"
        size="sm"
        disabled={offset === 0}
        aria-label={t('common:prevPage')}
        onClick={() => onOffset(Math.max(0, offset - limit))}
      >
        <ChevronLeft className="h-4 w-4" />
      </Button>
      <Button
        variant="outline"
        size="sm"
        disabled={to >= total}
        aria-label={t('common:nextPage')}
        onClick={() => onOffset(offset + limit)}
      >
        <ChevronRight className="h-4 w-4" />
      </Button>
    </div>
  )
}
