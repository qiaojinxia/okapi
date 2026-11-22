import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Database, MessageSquareText, RotateCw, Search, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { Drawer } from '@/components/ui/drawer'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { ConversationViewer } from '@/features/logs/ConversationArchiveSection'
import {
  type ArchiveSummary,
  deleteAdminConversation,
  fetchAdminConversation,
  fetchAdminConversations,
} from '@/features/logs/conversation-api'
import { describeError } from '@/lib/i18n'

const HOURS_OPTIONS = [
  { value: '24', key: 'admin:archiveRange24h' },
  { value: '168', key: 'admin:archiveRange7d' },
  { value: '720', key: 'admin:archiveRange30d' },
  { value: '', key: 'admin:archiveRangeAll' },
] as const

/// 管理端「对话存档」列表页签：无论 ClickHouse 是否启用均可检索并查看已存档对话。
export function AdminConversationsTab() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()

  const [searchDraft, setSearchDraft] = useState('')
  const [modelDraft, setModelDraft] = useState('')
  const [hours, setHours] = useState('168')
  const [applied, setApplied] = useState({ search: '', model: '', hours: '168', offset: 0 })
  const [selected, setSelected] = useState<ArchiveSummary | null>(null)

  const query = useQuery({
    queryKey: ['admin', 'conversations', applied],
    queryFn: () => {
      const p = new URLSearchParams({ limit: '50', offset: String(applied.offset) })
      if (applied.search.trim()) p.set('search', applied.search.trim())
      if (applied.model.trim()) p.set('model', applied.model.trim())
      if (applied.hours !== '') p.set('hours', applied.hours)
      return fetchAdminConversations(p)
    },
  })

  const detail = useQuery({
    queryKey: ['admin', 'conversation-archive', selected?.request_id ?? ''],
    queryFn: () => fetchAdminConversation(selected!.request_id),
    enabled: selected !== null,
  })

  const remove = useMutation({
    mutationFn: (requestId: string) => deleteAdminConversation(requestId),
    onSuccess: () => {
      toast.success(t('common:deleted'))
      setSelected(null)
      void queryClient.invalidateQueries({ queryKey: ['admin', 'conversations'] })
      void queryClient.invalidateQueries({ queryKey: ['admin', 'conversation-archive-stats'] })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const applyFilter = () => {
    setApplied({ search: searchDraft, model: modelDraft, hours, offset: 0 })
  }

  const rows = query.data?.data ?? []

  return (
    <div data-slot="admin-conversations-tab" className="flex flex-col gap-3">
      {dialog}
      <div className="flex flex-wrap items-center gap-2">
        <Input
          aria-label={t('admin:archiveSearchPlaceholder')}
          placeholder={t('admin:archiveSearchPlaceholder')}
          className="h-9 w-full sm:w-64"
          value={searchDraft}
          onChange={(e) => setSearchDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') applyFilter()
          }}
        />
        <Input
          aria-label={t('pricing:model')}
          placeholder={t('pricing:model')}
          className="h-9 w-full sm:w-44"
          value={modelDraft}
          onChange={(e) => setModelDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') applyFilter()
          }}
        />
        <Select
          aria-label={t('admin:archiveRangeLabel')}
          className="h-9 w-36"
          value={hours}
          onChange={(v) => {
            setHours(v)
            setApplied((prev) => ({ ...prev, hours: v, offset: 0 }))
          }}
          options={HOURS_OPTIONS.map((o) => ({ value: o.value, label: t(o.key) }))}
        />
        <Button size="sm" onClick={applyFilter}>
          <Search className="h-3.5 w-3.5" />
          {t('common:search')}
        </Button>
        <Button
          size="sm"
          variant="outline"
          loading={query.isFetching}
          onClick={() => void query.refetch()}
        >
          {!query.isFetching && <RotateCw className="h-3.5 w-3.5" />}
          {t('common:refresh')}
        </Button>
        {query.data !== undefined && (
          <Badge
            variant={query.data.clickhouse_enabled ? 'success' : 'info'}
            dot
            className="ml-auto"
          >
            <Database className="h-3 w-3" />
            {query.data.backend === 'clickhouse' ? 'ClickHouse' : 'PostgreSQL'}
          </Badge>
        )}
      </div>

      {query.isError ? (
        <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} />
      ) : query.isPending ? (
        <TableSkeleton dense rows={8} cols={6} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:archiveEmptyHint')} />
      ) : (
        <Table dense stickyHeader aria-label={t('admin:archiveTabLabel')}>
          <THead>
            <Tr>
              <Th>{t('logs:time')}</Th>
              <Th>{t('admin:logsUser')}</Th>
              <Th>{t('pricing:model')}</Th>
              <Th>{t('admin:archivePreview')}</Th>
              <Th>{t('admin:archiveStorage')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => (
              <Tr
                key={r.request_id}
                className="cursor-pointer"
                onClick={() => setSelected(r)}
              >
                <Td className="whitespace-nowrap font-mono text-xs text-muted-foreground">
                  {new Date(r.created_at).toLocaleString(i18n.language)}
                </Td>
                <Td className="truncate text-xs">
                  {r.username || `ID ${r.user_id}`}
                </Td>
                <Td className="font-mono text-xs">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate">{r.model}</span>
                    {r.truncated && (
                      <Badge variant="warning" className="min-h-4 px-1.5 py-0 text-[10px]">
                        {t('logs:conversationTruncated')}
                      </Badge>
                    )}
                  </div>
                </Td>
                <Td className="max-w-md">
                  <div className="truncate text-xs text-foreground">
                    {r.response_preview || r.request_preview || '—'}
                  </div>
                  <div className="truncate font-mono text-[11px] text-muted-foreground">
                    {r.request_id}
                  </div>
                </Td>
                <Td>
                  <Badge variant="muted" className="font-mono text-[11px]">
                    {r.storage === 'clickhouse' ? 'ClickHouse' : 'PostgreSQL'}
                  </Badge>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {rows.length > 0 && (
        <div className="flex items-center justify-end gap-2">
          <Button
            size="sm"
            variant="outline"
            disabled={applied.offset === 0}
            onClick={() =>
              setApplied((prev) => ({ ...prev, offset: Math.max(0, prev.offset - 50) }))
            }
          >
            {t('common:prevPage', { defaultValue: '←' })}
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={rows.length < 50}
            onClick={() => setApplied((prev) => ({ ...prev, offset: prev.offset + 50 }))}
          >
            {t('common:nextPage', { defaultValue: '→' })}
          </Button>
        </div>
      )}

      {selected !== null && (
        <Drawer
          open
          onClose={() => setSelected(null)}
          size="xl"
          icon={<MessageSquareText className="h-5 w-5 text-primary" />}
          title={`${selected.model} · ${selected.username || `ID ${selected.user_id}`}`}
          description={selected.request_id}
          footer={
            <div className="flex w-full items-center justify-between gap-2">
              <Button
                variant="outline"
                size="sm"
                className="text-destructive hover:bg-destructive/10"
                disabled={remove.isPending}
                onClick={() =>
                  confirm({
                    title: t('admin:archiveDeleteTitle'),
                    description: t('admin:archiveDeleteDesc'),
                    confirmLabel: t('common:delete'),
                    onConfirm: () => remove.mutate(selected.request_id),
                  })
                }
              >
                <Trash2 className="h-3.5 w-3.5" />
                {t('common:delete')}
              </Button>
              <Button variant="ghost" onClick={() => setSelected(null)}>
                {t('common:close')}
              </Button>
            </div>
          }
        >
          {detail.isPending ? (
            <div className="h-40 animate-pulse rounded-xl bg-muted/50" />
          ) : detail.isError ? (
            <ErrorState message={describeError(detail.error)} onRetry={() => void detail.refetch()} />
          ) : (
            <ConversationViewer record={detail.data.data} />
          )}
        </Drawer>
      )}
    </div>
  )
}
