import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { useInbox } from '@/features/compat/api'
import { fmtAt } from '@/features/compat/format'
import type { SetTarget } from '@/features/compat/SetDrawer'
import type { InboxItem } from '@/features/compat/types'
import { describeError } from '@/lib/i18n'

/// 待批准的草稿：AI 经 MCP 提议的、或没有发布权限的人提交的。点「审阅」看差异后再批准——
/// 收件箱里不提供一键批准，批准一份没看过差异的规则等于把写权限交给提议者。
export function InboxTab({
  onOpen,
  onOpenGuard,
}: {
  onOpen: (target: SetTarget) => void
  onOpenGuard: () => void
}) {
  const { t } = useTranslation()
  const inbox = useInbox(true)

  if (inbox.isError) return <ErrorState message={describeError(inbox.error)} onRetry={() => void inbox.refetch()} />
  if (inbox.isPending) return <TableSkeleton rows={4} cols={7} />
  const rows = inbox.data
  if (rows.length === 0) return <EmptyState title={t('compat:inboxEmpty')} hint={t('compat:inboxEmptyHint')} />

  const review = (item: InboxItem) => {
    if (item.scope === 'guard') onOpenGuard()
    else onOpen({ scope: item.scope, id: item.id, name: item.name })
  }
  return (
    <Table aria-label={t('compat:tabInbox')}>
      <THead>
        <Tr>
          <Th>{t('compat:colScope')}</Th>
          <Th>{t('compat:colTarget')}</Th>
          <Th>{t('compat:colSource')}</Th>
          <Th>{t('compat:colRationale')}</Th>
          <Th>{t('compat:colTime')}</Th>
          <Th numeric>{t('compat:colRules')}</Th>
          <Th>{t('compat:colActions')}</Th>
        </Tr>
      </THead>
      <TBody>
        {rows.map((item) => (
          <Tr key={`${item.scope}:${item.id}`}>
            <Td>
              <Badge variant={item.scope === 'guard' ? 'warning' : item.scope === 'profile' ? 'muted' : 'default'}>
                {t(`compat:scope_${item.scope}`)}
              </Badge>
            </Td>
            <Td className="max-w-56">
              <div className="flex min-w-0 flex-col">
                <span className="truncate font-medium">{item.scope === 'guard' ? t('compat:guardTitle') : (item.name ?? `#${item.id}`)}</span>
                <span className="text-xs text-muted-foreground tabular-nums">
                  {t('compat:draftBasedOn', { version: item.version, base: item.base_version ?? 0 })}
                </span>
              </div>
            </Td>
            <Td>
              <div className="flex flex-wrap gap-1">
                <Badge variant={item.source === 'mcp' ? 'info' : 'muted'}>
                  {item.source === 'mcp' ? t('compat:sourceAi') : t('compat:sourceAdmin')}
                </Badge>
                {item.stale && <Badge variant="warning">{t('compat:stale')}</Badge>}
              </div>
            </Td>
            <Td className="max-w-72">
              <span className="line-clamp-2 text-xs text-muted-foreground" title={item.rationale ?? item.note ?? undefined}>
                {item.rationale ?? item.note ?? '—'}
              </span>
            </Td>
            <Td className="text-xs whitespace-nowrap tabular-nums text-muted-foreground">{fmtAt(item.created_at)}</Td>
            <Td numeric>{item.rules}</Td>
            <Td>
              <Button size="xs" onClick={() => review(item)}>
                {t('compat:review')}
              </Button>
            </Td>
          </Tr>
        ))}
      </TBody>
    </Table>
  )
}
