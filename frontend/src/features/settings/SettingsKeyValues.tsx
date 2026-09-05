import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { ArrowUpRight, Coins, Gauge, Lock, Settings2, ShieldCheck, Users } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { SearchInput } from '@/components/ui/search-input'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { cn } from '@/lib/utils'
import { SettingEditorDrawer } from './SettingEditorDrawer'
import { containsSecret, isRecord, SETTING_GROUPS, settingMeta } from './setting-catalog'
import type { SettingGroup, SettingRow, SettingsSection } from './setting-catalog'

const GROUP_ICONS = { payment: Coins, identity: Users, traffic: Gauge, security: ShieldCheck, other: Settings2 }

export function SettingsCard({ onOpenSection }: { onOpenSection: (section: SettingsSection) => void }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const canWrite = usePermission()('settings.write')
  const [editing, setEditing] = useState<SettingRow | null>(null)
  const [filter, setFilter] = useState('')
  const [category, setCategory] = useState<SettingGroup | 'all'>('all')
  const settings = useQuery({
    queryKey: ['admin', 'settings'],
    queryFn: () => apiFetch<{ data: SettingRow[] }>('/admin/settings'),
  })
  const save = useMutation({
    mutationFn: (arg: { key: string; value: unknown }) => apiFetch('/admin/settings', { method: 'POST', body: arg }),
    onSuccess: (_r, arg) => {
      toast.success(t('common:saved'))
      setEditing(null)
      void queryClient.invalidateQueries({ queryKey: ['admin', 'settings'] })
      void queryClient.invalidateQueries({ queryKey: ['setting', arg.key] })
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const all = settings.data?.data ?? []
  const terms = filter.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean)
  const matches = all.filter((row) => {
    const meta = settingMeta(row.key)
    const group = SETTING_GROUPS.find((g) => g.id === meta.group)!
    const text = `${row.key} ${meta.label ? t(meta.label) : ''} ${t(meta.description)} ${t(group.label)}`.toLocaleLowerCase()
    return terms.every((term) => text.includes(term))
  })
  const groups = SETTING_GROUPS.map((group) => ({
    ...group, rows: matches.filter((row) => settingMeta(row.key).group === group.id),
  }))
  const visible = groups.filter((group) => (category === 'all' || category === group.id) && group.rows.length > 0)
  const count = visible.reduce((n, group) => n + group.rows.length, 0)
  const reset = () => { setFilter(''); setCategory('all') }

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <SearchInput className="w-full sm:max-w-sm" value={filter} onChange={setFilter} aria-label={t('admin:settingSearch')} placeholder={t('admin:settingSearch')} />
        {(filter !== '' || category !== 'all') && <Button variant="ghost" onClick={reset}>{t('admin:settingClearFilters')}</Button>}
        <span className="text-xs text-muted-foreground sm:ml-auto" role="status">{t('common:resultCount', { n: count })}</span>
      </div>
      <div role="group" aria-label={t('admin:settingCategories')} className="flex flex-wrap gap-2">
        {[{ id: 'all', label: 'common:all', rows: matches }, ...groups].map((group) => (
          <Button key={group.id} variant={category === group.id ? 'secondary' : 'ghost'} className={cn('min-h-11 px-3 md:min-h-9', category === group.id && 'ring-1 ring-border')} aria-pressed={category === group.id} onClick={() => setCategory(group.id as SettingGroup | 'all')}>
            {t(group.label)} <span className="text-xs text-muted-foreground">{group.rows.length}</span>
          </Button>
        ))}
      </div>
      {settings.isError ? <ErrorState message={describeError(settings.error)} onRetry={() => void settings.refetch()} />
        : settings.isPending ? <TableSkeleton rows={6} cols={2} />
        : count === 0 ? <EmptyState title={filter || category !== 'all' ? t('common:noResults') : undefined} hint={t('admin:settingEmptyHint')} />
        : <div className="grid items-start gap-4 xl:grid-cols-2">
          {visible.map((group) => {
            const Icon = GROUP_ICONS[group.id]
            return (
              <section key={group.id} aria-label={t(group.label)} className="min-w-0 overflow-hidden rounded-xl border border-border bg-card shadow-card">
                <div className="flex items-center gap-2.5 border-b border-border bg-muted/30 px-4 py-3">
                  <Icon aria-hidden className="h-4 w-4 text-primary" />
                  <h2 className="flex-1 text-sm font-semibold">{t(group.label)}</h2>
                  <span className="text-xs text-muted-foreground">{t('common:resultCount', { n: group.rows.length })}</span>
                </div>
                <div className="divide-y divide-border">
                  {group.rows.map((row) => {
                    const meta = settingMeta(row.key)
                    const label = meta.label ? t(meta.label) : row.key
                    return (
                      <article key={row.key} aria-label={label} className="flex flex-col gap-3 p-4">
                        <div className="flex items-start gap-3">
                          <div className="min-w-0 flex-1">
                            <h3 className="text-sm font-medium break-words">{label}</h3>
                            <p className="mt-1 text-xs leading-5 text-muted-foreground">{t(meta.description)}</p>
                          </div>
                          {canWrite && <Button variant="outline" className="min-h-11 shrink-0 px-3 md:min-h-9" aria-label={t('admin:settingConfigureName', { name: label })} onClick={() => meta.section ? onOpenSection(meta.section) : setEditing(row)}>
                            {meta.section ? t('admin:settingOpenForm') : t('admin:settingConfigure')}
                            {meta.section && <ArrowUpRight aria-hidden className="h-3.5 w-3.5" />}
                          </Button>}
                        </div>
                        <div className="flex flex-wrap items-center gap-2 text-sm">
                          <SettingSummary row={row} />
                          {(row.is_secret || containsSecret(row.value)) && <span className="inline-flex items-center gap-1 text-[11px] text-muted-foreground"><Lock aria-hidden className="h-3 w-3" />{t('admin:settingSecretsHidden')}</span>}
                        </div>
                        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 text-[11px] text-muted-foreground/80">
                          <code className="min-w-0 break-all">{row.key}</code>
                          {row.updated_at && <time dateTime={row.updated_at} title={dayjs(row.updated_at).format('YYYY-MM-DD HH:mm:ss')}>{t('admin:settingUpdatedShort', { time: dayjs(row.updated_at).format('MM-DD HH:mm') })}</time>}
                        </div>
                      </article>
                    )
                  })}
                </div>
              </section>
            )
          })}
        </div>}
      {editing && <SettingEditorDrawer key={editing.key} row={editing} pending={save.isPending} onCancel={() => setEditing(null)} onSave={(value) => save.mutate({ key: editing.key, value })} />}
    </div>
  )
}

function SettingSummary({ row }: { row: SettingRow }) {
  const { t, i18n } = useTranslation()
  if (row.is_secret) return <Badge variant={row.configured ? 'success' : 'muted'} dot>{t(row.configured ? 'admin:settingSet' : 'admin:settingUnset')}</Badge>
  const value = row.value
  if (row.key === 'aff_percent_bp' && typeof value === 'number') return <span className="font-semibold tabular-nums">{(value / 100).toLocaleString(i18n.language, { maximumFractionDigits: 2 })}% <span className="font-normal text-muted-foreground">{t('admin:settingRebateUnit')}</span></span>
  if (row.key === 'ssrf_policy' && isRecord(value)) return <>
    <Badge variant={value.allow_http ? 'warning' : 'muted'}>{t(value.allow_http ? 'admin:settingHttpAllowed' : 'admin:settingHttpsOnly')}</Badge>
    <Badge variant={value.allow_private ? 'warning' : 'muted'}>{t(value.allow_private ? 'admin:settingPrivateAllowed' : 'admin:settingPublicOnly')}</Badge>
  </>
  if (typeof value === 'boolean') return <Badge variant={value ? 'success' : 'muted'} dot>{t(value ? 'common:enabled' : 'common:disabled')}</Badge>
  if (isRecord(value) && (row.key === 'payment_epay' || row.key === 'payment_stripe')) {
    const required = row.key === 'payment_epay' ? ['gateway_url', 'pid', 'key'] : ['secret_key', 'webhook_secret']
    const complete = required.every((key) => typeof value[key] === 'string' && String(value[key]).trim() !== '')
    return <>
      <Badge variant={complete ? 'success' : 'warning'} dot>{t(complete ? 'admin:settingSet' : 'admin:settingIncomplete')}</Badge>
      {row.key === 'payment_epay' && typeof value.pid === 'string' && <span className="max-w-full truncate text-xs text-muted-foreground">{t('admin:settingMerchantSummary', { id: value.pid })}</span>}
    </>
  }
  if (row.key === 'model_rpm_limits' && isRecord(value)) return <>
    <Badge variant="muted">{t('admin:settingLimitCount', { n: Object.keys(value).length })}</Badge>
    {Object.entries(value).slice(0, 2).map(([model, limit]) => typeof limit === 'number' && <span key={model} className="inline-flex min-w-0 max-w-full items-center gap-1 text-xs text-muted-foreground"><span className="truncate">{model}</span><span className="shrink-0">· {limit > 0 ? `${limit} RPM` : t('admin:settingUnlimited')}</span></span>)}
  </>
  if (Array.isArray(value)) return <Badge variant="muted">{t('admin:settingEntries', { n: value.length })}</Badge>
  if (isRecord(value)) return <Badge variant="muted">{t('admin:settingFieldCount', { n: Object.keys(value).length })}</Badge>
  if (value === null || value === undefined || value === '') return <Badge variant="muted">{t('admin:settingUnset')}</Badge>
  return <span className="max-w-full truncate tabular-nums">{String(value)}</span>
}
