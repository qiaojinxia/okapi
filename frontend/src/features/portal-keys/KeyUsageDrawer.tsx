import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { ArrowUpRight, KeyRound, RotateCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { LogSummary } from '@/features/logs/LogSummary'
import type { LogStats } from '@/features/logs/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

interface UsageKey {
  id: number
  name: string
  key_prefix: string
  status: number
}

// Only mounted after opening a key: no per-row summary requests or account-wide fallback.
export function KeyUsageDrawer({ apiKey, onClose }: { apiKey: UsageKey; onClose: () => void }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const stats = useQuery({
    queryKey: qk.keyUsage(apiKey.id),
    queryFn: () => apiFetch<LogStats>(`/api/me/logs/stat?scope=user&api_key_id=${apiKey.id}`),
    retry: false,
  })
  const openDetails = () => void navigate({
    to: '/portal/logs',
    search: { scope: 'user', api_key_id: apiKey.id },
  })

  return <Drawer
    open
    onClose={onClose}
    title={t('portal:keyUsageTitle')}
    description={t('portal:keyUsageHint')}
    footer={<>
      <Button variant="outline" onClick={onClose}>{t('common:close')}</Button>
      <Button onClick={openDetails}>{t('portal:keyUsageDetails')}<ArrowUpRight aria-hidden className="h-4 w-4" /></Button>
    </>}
  >
    <div className="space-y-4">
      <div className="flex items-start gap-3 rounded-xl border border-primary/15 bg-primary/5 p-4">
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
          <KeyRound aria-hidden className="h-4 w-4" />
        </span>
        <div className="min-w-0 flex-1 space-y-1.5">
          <div className="flex flex-wrap items-center gap-2">
            <span className="min-w-0 font-medium [overflow-wrap:anywhere]">{apiKey.name}</span>
            <Badge dot variant={apiKey.status === 1 ? 'success' : 'muted'}>
              {apiKey.status === 1 ? t('common:enabled') : t('common:disabled')}
            </Badge>
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs text-muted-foreground">
            <span>#{apiKey.id}</span><code>{apiKey.key_prefix}…</code>
          </div>
        </div>
      </div>

      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-medium text-muted-foreground">{t('portal:keyUsageScope')}</span>
        <Button size="sm" variant="ghost" loading={stats.isFetching} onClick={() => void stats.refetch()}>
          {!stats.isFetching && <RotateCw aria-hidden className="h-3.5 w-3.5" />}{t('common:refresh')}
        </Button>
      </div>
      <LogSummary
        data={stats.data}
        loading={stats.isPending}
        error={stats.isError}
        onRetry={() => void stats.refetch()}
        layout="panel"
        onOpenDetails={openDetails}
      />
      {stats.data?.records === 0 && !stats.isError && <p role="status" className="rounded-lg bg-muted/60 px-3 py-2.5 text-xs text-muted-foreground">{t('portal:keyUsageEmpty')}</p>}
    </div>
  </Drawer>
}
