import { useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Download, RotateCw } from 'lucide-react'
import type { LogSearch } from '@/routes/admin.logs'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { PageHeader } from '@/components/ui/page'
import type { EntityKind, EntityOption } from '@/features/entity-search/EntitySearchInput'
import { DEFAULT_PAGE_SIZE, PAGE_SIZES, usePagination } from '@/hooks/use-pagination'
import { maskEmails } from '@/lib/mask'
import { qk } from '@/lib/query-keys'
import { FilterBar } from './FilterBar'
import { LogTable } from './LogTable'
import { RangePicker } from './RangePicker'
import { StatBar } from './StatBar'
import { useAdminLogs } from './api'
import { exportCsv } from './export-csv'
import { fromSearch, invalidId, numericFilters, routeApi, toParams, toSearch, validRange } from './search'
import type { Draft } from './search'

/// 全站日志页（对齐 new-api 的日志页 + 统计条，数据源换成 CH raw）。
///
/// 版面三段：紧凑检索工具栏 → 窗口统计条 → 铺满剩余高度的明细表。
/// 明细行点开详情抽屉——请求 ID / 上游请求 ID / 节点 / 重试与切换计数
/// 是工单三件套，放主表列会把表撑到横向滚动，收进展开区各取所需。
export function AdminLogsPage() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const applied = fromSearch(search)
  const [draft, setDraft] = useState<Draft>(applied)

  // 地址变了（看板深链跳过来、浏览器前进后退）→ 表单跟着地址走。
  // 依赖用序列化后的字符串：search 对象每次渲染都是新引用；只看过滤条件，翻页不算。
  const appliedKey = JSON.stringify(toSearch(applied))
  useEffect(() => {
    setDraft(fromSearch(JSON.parse(appliedKey) as LogSearch))
  }, [appliedKey])
  // 页宽档位到 200 为止（后端钳制上限）；CH 明细不 count，翻页靠"本页满 = 可能还有"。
  // 页码也在地址里；`commit` 整体替换 search 时不带 page，过滤一变自然回第一页
  const pager = usePagination({ pageSizes: [...PAGE_SIZES, 200] })
  const logs = useAdminLogs(applied, pager)
  const rows = logs.isError || logs.isPlaceholderData ? [] : logs.data?.data ?? []
  const scope = logs.isError || logs.isPlaceholderData ? undefined : logs.data?.scope
  const known: Partial<Record<EntityKind, EntityOption[]>> = {
    user: rows.filter((row) => row.username).map((row) => ({ id: row.user_id, name: row.username })),
    channel: rows.filter((row) => row.channel_name && row.channel_id > 0).map((row) => ({ id: row.channel_id, name: maskEmails(row.channel_name), description: row.provider })),
    api_key: rows.filter((row) => row.key_name || row.key_prefix).map((row) => ({ id: row.api_key_id, name: row.key_name || t('flow:unnamed_api_key'), description: [row.username, row.key_prefix ? `${row.key_prefix}…` : undefined].filter(Boolean).join(' · ') })),
  }
  // 回填按已提交 ID 匹配；旧页占位数据和其他对象的名字不能套到新条件上。
  if (scope?.user && scope.user.id === search.user_id && scope.user.username != null) known.user?.push({ id: scope.user.id, name: scope.user.username || t('flow:unnamed_user') })
  if (scope?.channel && scope.channel.id === search.channel_id && scope.channel.name != null) known.channel?.push({ id: scope.channel.id, name: scope.channel.name || t('flow:unnamed_channel'), description: scope.channel.provider ?? undefined })
  if (scope?.api_key && scope.api_key.id === search.api_key_id && scope.api_key.name != null) known.api_key?.push({
    id: scope.api_key.id, name: scope.api_key.name || t('flow:unnamed_api_key'),
    description: [scope.api_key.username, scope.api_key.key_prefix ? `${scope.api_key.key_prefix}…` : undefined].filter(Boolean).join(' · '),
  })

  const commit = (next: Draft) => {
    if (numericFilters.some((field) => invalidId(next[field])) || ((next.from || next.to) && !validRange(next))) return
    void navigate({ search: toSearch(next) })
  }

  return (
    <div className="list-page [--page-gap:8px]">
      <PageHeader
        title={t('admin:logsNav')}
        description={t('admin:logsDesc')}
        compact
        className="[&_p]:text-xs [&_p]:leading-5"
        action={<>
          <Button size="sm" variant="outline" disabled={rows.length === 0 || logs.isFetching} title={t('logs:exportPageHint', { n: rows.length })} onClick={() => exportCsv(rows)}>
            <Download className="h-3.5 w-3.5" />{t('logs:exportPage')}
          </Button>
          <Button size="sm" variant="outline" loading={logs.isFetching} onClick={() => {
            void logs.refetch()
            void client.invalidateQueries({ queryKey: qk.adminLogStat(toParams(applied, 0, DEFAULT_PAGE_SIZE)) })
          }}>
            {!logs.isFetching && <RotateCw className="h-3.5 w-3.5" />}{t('common:refresh')}
          </Button>
        </>}
      />
      <Card aria-label={t('logs:filters')} data-slot="admin-log-filters" className="shrink-0 rounded-xl px-2 py-1">
        <FilterBar draft={draft} onChange={setDraft} onApply={() => commit(draft)} known={known} />
        <div className="flex min-w-0 flex-wrap items-start gap-2 border-t border-border/60 pt-2">
          <RangePicker
            draft={draft}
            onPreset={(h) => commit({ ...draft, hours: h, from: '', to: '' })}
            onRange={(from, to) => setDraft({ ...draft, from, to })}
            onApplyRange={() => commit(draft)}
          />
        </div>
      </Card>
      <StatBar applied={applied} />
      <LogTable applied={applied} pager={pager} q={logs} />
    </div>
  )
}
