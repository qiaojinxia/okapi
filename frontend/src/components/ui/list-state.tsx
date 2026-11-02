import type { ReactNode } from 'react'
import { TableSkeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { describeError } from '@/lib/i18n'

/// 列表的四态：出错（带重试）→ 首次加载（骨架）→ 空（下一步动作）→ 有数据（children）。
/// 配 usePagedQuery 用时翻页保留上一页数据，所以骨架只在第一次进页面时出现。
export function ListState({ query, skeleton, empty, children }: {
  query: { isError: boolean; error: unknown; isPending: boolean; refetch: () => unknown; data?: { data: readonly unknown[] } }
  skeleton: { rows: number; cols: number }
  empty: ReactNode
  children: ReactNode
}) {
  if (query.isError) return <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} />
  if (query.isPending) return <TableSkeleton rows={skeleton.rows} cols={skeleton.cols} />
  if ((query.data?.data.length ?? 0) === 0) return empty
  return children
}
