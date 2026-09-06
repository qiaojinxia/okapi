import { ChevronLeft, ChevronRight } from 'lucide-react'
import { useEffect, useId } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { clampOffset } from '@/hooks/use-pagination'
import { cn } from '@/lib/utils'

interface PaginationProps {
  /// 总条数；`undefined` = 还没回来（骨架屏在位），此时整条不渲染。
  total?: number
  /// 不做 count 的列表（CH 明细）给这个，通常 = 本页装满了。
  /// **给了它就切到"未知总数"口径**：不画页码，只画上一页 / 下一页，第一页也常驻。
  hasMore?: boolean
  limit: number
  offset: number
  onOffset: (offset: number) => void
  /// 每页条数档位；与 `onLimit` 一起给才显示切换器。
  pageSizes?: readonly number[]
  onLimit?: (limit: number) => void
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

/// 分页器；配合 `usePagination()` 用：`<Pagination {...pager} total={data?.total} />`。
///
/// 两种口径：
/// - 已知 total（PG 列表 / 前端切片）：区间文字 + 页码按钮。管理面列表带搜索与过滤，
///   实际使用是"筛小再看"，但"跳到最后一页看最新/最老"这种需求确实存在，
///   页码 ≤ 七页全铺开、更多时折省略号；
/// - 未知 total（CH 明细不 count，多扫一遍不值）：只给上一页 / 下一页，
///   下一页能否点由 `hasMore` 决定。
///
/// 何时不渲染：骨架屏与空态各自已占位，故 total 未回来或为 0 时整条不画；一页装得下时
/// **只有没带页宽切换器的调用方**（自带页宽 UI 的公开定价页）才整条隐藏。带切换器的一律常驻：
/// 它是"每页看多少"的唯一入口，跟着列表长度忽隐忽现的话，切到 100/页后就再也换不回 20/页，
/// 短列表页（分组 / 池 / 角色 / 套餐）更会看起来"分页没了"。
export function Pagination({
  total,
  hasMore,
  limit,
  offset,
  onOffset,
  pageSizes,
  onLimit,
  className,
}: PaginationProps) {
  const { t } = useTranslation()
  const sizeId = useId()
  // 档位里补上当前页宽：调用方给了不在档位里的 limit 时，下拉才不会显示成空
  const sizes =
    onLimit === undefined ? [] : [...new Set([...(pageSizes ?? []), limit])].sort((a, b) => a - b)
  const showSizes = sizes.length > 1
  // 给了 hasMore = 未知总数口径（CH 明细）；否则按 total 走已知总数口径
  const counted = hasMore === undefined
  // 结果集收缩到当前页之前（删掉末页最后一条 / 筛选变窄）：退回最后一页，不停在空页上
  const clamped = total === undefined ? offset : clampOffset(offset, limit, total)
  useEffect(() => {
    if (clamped !== offset) onOffset(clamped)
  }, [clamped, offset, onOffset])

  if (counted) {
    // 加载中与空列表：骨架屏 / 空态已经占了位，不必再画一条"0–0 / 共 0"
    if (total === undefined || total === 0) return null
    if (total <= limit && !showSizes) return null
  } else if (offset === 0 && !hasMore && !showSizes) {
    return null
  }

  const current = Math.floor(clamped / limit) + 1
  const pages = total === undefined ? null : Math.max(1, Math.ceil(total / limit))
  return (
    <nav
      aria-label={t('common:pagination')}
      className={cn('flex flex-wrap items-center justify-between gap-3', className)}
    >
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-xs text-muted-foreground tabular-nums">
          {total === undefined
            ? t('common:pageN', { page: current })
            : t('common:pageRange', {
                from: clamped + 1,
                to: Math.min(clamped + limit, total),
                total,
              })}
        </span>
        {showSizes && (
          <span className="flex items-center gap-1.5">
            <Label htmlFor={sizeId}>{t('common:pageSize')}</Label>
            <Select
              id={sizeId}
              className="[&>select]:h-8 [&>select]:text-xs"
              value={String(limit)}
              onChange={(v) => onLimit?.(Number(v))}
              options={sizes.map((n) => ({ value: String(n), label: t('common:perPage', { n }) }))}
            />
          </span>
        )}
      </div>
      <div className="flex items-center gap-1">
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          disabled={current === 1}
          aria-label={t('common:prevPage')}
          onClick={() => onOffset(Math.max(0, clamped - limit))}
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        {pages !== null &&
          pageList(current, pages).map((p, i) =>
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
          disabled={pages !== null ? current >= pages : !hasMore}
          aria-label={t('common:nextPage')}
          onClick={() => onOffset(clamped + limit)}
        >
          <ChevronRight className="h-4 w-4" />
        </Button>
      </div>
    </nav>
  )
}
