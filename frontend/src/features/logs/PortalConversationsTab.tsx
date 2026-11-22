import { useQuery } from '@tanstack/react-query'
import { MessageSquareText, RotateCw, Search } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Input } from '@/components/ui/input'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { ConversationViewer } from '@/features/logs/ConversationArchiveSection'
import {
  type ArchiveSummary,
  fetchPortalConversation,
  fetchPortalConversations,
} from '@/features/logs/conversation-api'
import { describeError } from '@/lib/i18n'

/// 门户「我的对话存档」列表视图（仅在站点管理员开启 `allow_user_view` 后显示，兼容 ClickHouse 与纯 PG 模式）。
export function PortalConversationsTab({ scope, keyId }: { scope: 'key' | 'user'; keyId: string }) {
  const { t, i18n } = useTranslation()
  const [searchDraft, setSearchDraft] = useState('')
  const [modelDraft, setModelDraft] = useState('')
  const [applied, setApplied] = useState({ search: '', model: '', offset: 0 })
  const [selected, setSelected] = useState<ArchiveSummary | null>(null)

  const query = useQuery({
    queryKey: ['portal', 'conversations', scope, keyId, applied],
    queryFn: () => {
      const p = new URLSearchParams({ scope, limit: '50', offset: String(applied.offset) })
      if (scope === 'user' && keyId) p.set('api_key_id', keyId)
      if (applied.search.trim()) p.set('search', applied.search.trim())
      if (applied.model.trim()) p.set('model', applied.model.trim())
      return fetchPortalConversations(p)
    },
  })

  const detail = useQuery({
    queryKey: ['portal', 'conversation-archive', selected?.request_id ?? ''],
    queryFn: () => fetchPortalConversation(selected!.request_id),
    enabled: selected !== null,
  })

  const applyFilter = () => {
    setApplied({ search: searchDraft, model: modelDraft, offset: 0 })
  }

  const rows = query.data?.data ?? []

  return (
    <div data-slot="portal-conversations-tab" className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <Input
          aria-label={t('logs:conversationSearchPlaceholder')}
          placeholder={t('logs:conversationSearchPlaceholder')}
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
      </div>

      {query.isError ? (
        <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} />
      ) : query.isPending ? (
        <TableSkeleton dense rows={6} cols={4} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('logs:conversationEmptyHint')} />
      ) : (
        <Table dense stickyHeader aria-label={t('logs:conversationTitle')}>
          <THead>
            <Tr>
              <Th>{t('logs:time')}</Th>
              <Th>{t('pricing:model')}</Th>
              <Th>{t('logs:conversationPreview')}</Th>
              <Th>{t('logs:endpoint')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => (
              <Tr key={r.request_id} className="cursor-pointer" onClick={() => setSelected(r)}>
                <Td className="whitespace-nowrap font-mono text-xs text-muted-foreground">
                  {new Date(r.created_at).toLocaleString(i18n.language)}
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
                <Td className="font-mono text-xs text-muted-foreground">{r.endpoint || '—'}</Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {selected !== null && (
        <Drawer
          open
          onClose={() => setSelected(null)}
          size="xl"
          icon={<MessageSquareText className="h-5 w-5 text-primary" />}
          title={selected.model}
          description={selected.request_id}
          footer={
            <Button variant="ghost" onClick={() => setSelected(null)}>
              {t('common:close')}
            </Button>
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
