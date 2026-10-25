import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { useSetActions, useSetOverview, useVersionDetail, useVersions } from '@/features/compat/api'
import { pretty } from '@/features/compat/diff'
import { describeCompatError, fmtAt } from '@/features/compat/format'
import { RulesDiffView } from '@/features/compat/RulesDiffView'
import type { SetScope, VersionRow } from '@/features/compat/types'
import { describeError } from '@/lib/i18n'

const STATUS_VARIANT = { published: 'success', draft: 'info', archived: 'muted' } as const

/// 版本详情：线上那一版直接给内容，其它版本给「相对线上版本的差异」——回滚之前最想知道的就是会改什么。
function VersionDetail({ scope, id, version, live }: { scope: SetScope; id: number; version: number; live: number | null }) {
  const { t } = useTranslation()
  const detail = useVersionDetail(scope, id, version)
  const current = useVersionDetail(scope, id, live !== null && live !== version ? live : null)
  if (detail.isError) return <p role="alert" className="text-xs text-destructive">{describeError(detail.error)}</p>
  if (detail.isPending) return <p className="text-xs text-muted-foreground">{t('common:loading')}</p>
  if (live === version || live === null) {
    return (
      <pre className="max-h-72 overflow-auto rounded-md border border-border bg-muted/30 p-2 font-mono text-xs leading-5">
        {pretty(detail.data.rules)}
      </pre>
    )
  }
  if (current.isPending) return <p className="text-xs text-muted-foreground">{t('common:loading')}</p>
  return (
    <div className="flex flex-col gap-2">
      <p className="text-xs text-muted-foreground">{t('compat:historyDiffHint', { version: live })}</p>
      <RulesDiffView before={current.data?.rules ?? []} after={detail.data.rules} />
    </div>
  )
}

/// 版本历史：谁在什么时候发布了什么，随时可以把任一旧版本作为新版本重新发布（回滚不改写历史）。
export function HistoryPanel({ scope, id, canPublish }: { scope: SetScope; id: number; canPublish: boolean }) {
  const { t } = useTranslation()
  const versions = useVersions(scope, id, true)
  const overview = useSetOverview(scope, id)
  const actions = useSetActions(scope, id)
  const { confirm, dialog } = useConfirm()
  const [open, setOpen] = useState<number | null>(null)
  const live = overview.data?.published?.version ?? null

  const rollback = (row: VersionRow) =>
    confirm({
      title: t('compat:rollbackTitle', { version: row.version }),
      description: t('compat:rollbackBody'),
      confirmLabel: t('compat:rollback'),
      tone: 'default',
      onConfirm: () =>
        actions.rollback.mutate(row.version, {
          onSuccess: (done) => toast.success(t('compat:rolledBack', { version: done.version })),
          onError: (err) => toast.error(describeCompatError(err, t)),
        }),
    })

  if (versions.isError) return <ErrorState message={describeError(versions.error)} onRetry={() => void versions.refetch()} />
  if (versions.isPending) return <TableSkeleton rows={4} cols={6} />
  const rows = versions.data
  if (rows.length === 0) return <EmptyState title={t('compat:historyEmpty')} hint={t('compat:historyEmptyHint')} />
  return (
    <>
      {dialog}
      <Table aria-label={t('compat:historyTitle')} dense>
        <THead>
          <Tr>
            <Th>{t('compat:colVersion')}</Th>
            <Th>{t('compat:colStatus')}</Th>
            <Th>{t('compat:colSource')}</Th>
            <Th>{t('compat:colNote')}</Th>
            <Th>{t('compat:colTime')}</Th>
            <Th numeric>{t('compat:colRules')}</Th>
            <Th>{t('compat:colActions')}</Th>
          </Tr>
        </THead>
        <TBody>
          {rows.flatMap((row) => {
            const isLive = row.version === live
            const expanded = open === row.version
            const cells = (
              <Tr key={row.version} selected={expanded}>
                <Td className="font-medium tabular-nums">v{row.version}</Td>
                <Td>
                  <Badge variant={STATUS_VARIANT[row.status]}>
                    {isLive ? t('compat:status_live') : t(`compat:status_${row.status}`)}
                  </Badge>
                </Td>
                <Td>
                  <Badge variant={row.source === 'mcp' ? 'info' : 'muted'}>
                    {row.source === 'mcp' ? t('compat:sourceAi') : t('compat:sourceAdmin')}
                  </Badge>
                </Td>
                <Td className="max-w-64 truncate text-xs text-muted-foreground" title={row.note ?? undefined}>
                  {row.note ?? '—'}
                </Td>
                <Td className="text-xs whitespace-nowrap tabular-nums text-muted-foreground">
                  {fmtAt(row.published_at ?? row.created_at)}
                </Td>
                <Td numeric>{row.rules}</Td>
                <Td>
                  <div className="flex gap-1.5">
                    <Button
                      size="xs"
                      variant="outline"
                      aria-expanded={expanded}
                      onClick={() => setOpen(expanded ? null : row.version)}
                    >
                      {expanded ? t('compat:hide') : t('compat:view')}
                    </Button>
                    {canPublish && row.status !== 'draft' && !isLive && (
                      <Button size="xs" variant="outline" disabled={actions.rollback.isPending} onClick={() => rollback(row)}>
                        {t('compat:rollback')}
                      </Button>
                    )}
                  </div>
                </Td>
              </Tr>
            )
            return expanded
              ? [
                  cells,
                  <Tr key={`${row.version}-detail`}>
                    <Td colSpan={7} className="bg-muted/20">
                      <VersionDetail scope={scope} id={id} version={row.version} live={live} />
                    </Td>
                  </Tr>,
                ]
              : [cells]
          })}
        </TBody>
      </Table>
    </>
  )
}
