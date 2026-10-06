import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { ArrowUpRight, KeyRound, RotateCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { KeyUsageSeries } from './KeyUsageSeries'
import { LogSummary } from '@/features/logs/LogSummary'
import type { LogStats } from '@/features/logs/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

interface UsageKey {
  id: number
  name: string
  key_prefix: string
  status: number
  created_at?: string
  last_used_at?: string | null
}

// Only mounted after opening a key: no per-row summary requests or account-wide fallback.
export function KeyUsageDrawer({ apiKey, onClose }: { apiKey: UsageKey; onClose: () => void }) {
  const { t, i18n } = useTranslation()
  const navigate = useNavigate()
  const date = (value: string | null | undefined) => {
    const time = value ? Date.parse(value) : NaN
    return Number.isNaN(time) ? null : new Intl.DateTimeFormat(i18n.language, { dateStyle: 'medium' }).format(time)
  }
  const created = date(apiKey.created_at)
  const lastUsed = date(apiKey.last_used_at)
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
      <div className="flex items-start gap-3 rounded-xl border border-primary/15 bg-gradient-to-br from-primary/10 via-primary/5 to-transparent p-4">
        <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-primary/12 text-primary ring-1 ring-primary/15">
          <KeyRound aria-hidden className="h-4.5 w-4.5" />
        </span>
        <div className="min-w-0 flex-1 space-y-1.5">
          <div className="flex flex-wrap items-center gap-2">
            <span className="min-w-0 text-base font-semibold [overflow-wrap:anywhere]">{apiKey.name}</span>
            <Badge dot variant={apiKey.status === 1 ? 'success' : 'muted'}>
              {apiKey.status === 1 ? t('common:enabled') : t('common:disabled')}
            </Badge>
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs text-muted-foreground">
            <span>#{apiKey.id}</span><code>{apiKey.key_prefix}…</code>
          </div>
          {(created || lastUsed || apiKey.last_used_at === null) && (
            <p className="flex flex-wrap gap-x-3 text-xs text-muted-foreground">
              {created && <span>{t('portal:keyUsageCreated', { date: created })}</span>}
              {/* 只有后端明确回 null 才是"尚未使用"；字段缺失（旧后端）不下结论 */}
              {lastUsed ? <span>{t('portal:keyUsageLastUsed', { date: lastUsed })}</span> : apiKey.last_used_at === null && <span>{t('portal:keyUsageNeverUsed')}</span>}
            </p>
          )}
        </div>
      </div>

      <KeyUsageSeries key={apiKey.id} keyId={apiKey.id} name={apiKey.name} />

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
