import { useQuery } from '@tanstack/react-query'
import { Link, getRouteApi } from '@tanstack/react-router'
import { Activity, ArrowUpRight, CalendarDays, Coins, Cpu, KeyRound, Monitor, ShieldCheck, UserRound } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { PageBody, PageHeader } from '@/components/ui/page'
import { UsageScope } from '@/components/usage-scope'
import { Tabs, TabPanel } from '@/components/ui/tabs'
import { Segmented } from '@/components/ui/segmented'
import { useUsageScope } from '@/hooks/use-usage-scope'
import { Stat } from '@/components/ui/stat'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { Table, THead, TBody, Tr, Th, Td } from '@/components/ui/table'
import { roleLabel } from '@/features/users/types'
import { ActiveSessionsCard } from '@/features/security/ActiveSessionsCard'
import { RecentLoginsCard } from '@/features/security/RecentLoginsCard'
import { useMe } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { BasicInfo } from './BasicInfo'
import type { AccountProfile } from './BasicInfo'
import { buildActivity, calendarDate } from './activity'
import type { ActivityDay, ActivityResponse, Metric } from './activity'
import { UsageHeatmap } from './UsageHeatmap'

const routeApi = getRouteApi('/portal/profile')

export function ProfilePage() {
  const { t, i18n } = useTranslation()
  const me = useMe()
  const search = routeApi.useSearch(), navigate = routeApi.useNavigate()
  const tab = search.tab ?? 'activity'
  const profile = useQuery({ queryKey: qk.myProfile, enabled: me.data?.has_web_session === true, retry: false, staleTime: 60_000, queryFn: () => apiFetch<AccountProfile>('/api/me/profile') })
  const usageScope = useUsageScope()
  const { scope } = usageScope
  const [year, setYear] = useState<number>()
  const query = useQuery({
    queryKey: qk.myActivity(scope, year),
    queryFn: () => apiFetch<ActivityResponse>(`/api/me/stats/activity?scope=${scope}${year === undefined ? '' : `&year=${year}`}`),
    retry: false,
    enabled: usageScope.ready && tab === 'activity',
    refetchInterval: 60_000,
  })
  const currentYear = Number(query.data?.today.slice(0, 4) ?? new Date().getUTCFullYear())
  const selectedYear = year ?? query.data?.year ?? currentYear
  const firstYear = Math.min(query.data?.first_year ?? selectedYear, selectedYear)

  return (
    <PageBody>
      <PageHeader title={t('profile:title')} description={t('profile:description')} icon={UserRound} />
      <section className="flex flex-wrap items-center justify-between gap-5 rounded-xl border border-border bg-card p-5 shadow-card" aria-label={t('profile:account')}>
        <div className="flex min-w-0 items-center gap-4">
          <span className="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl bg-primary/10 text-primary"><UserRound className="h-7 w-7" /></span>
          <div className="min-w-0 space-y-1.5">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="min-w-0 break-all text-lg font-semibold">{profile.data?.username || (me.data ? t('common:userId', { id: me.data.user_id }) : '—')}</h2>
              {me.data && <Badge>{roleLabel(me.data.role, t)}</Badge>}
            </div>
            <p className="break-all text-sm text-muted-foreground">{me.data?.group} · {t('profile:balance')}: {me.data ? formatMoney(me.data.balance_micro, i18n.language) : '—'}</p>
          </div>
        </div>
        <div className="flex flex-wrap gap-2 text-sm">
          <Link to="/portal/keys" className="inline-flex min-h-11 items-center gap-2 rounded-lg border border-border px-3 hover:bg-accent focus-visible:outline-2 focus-visible:outline-primary"><KeyRound className="h-4 w-4" />{t('portal:keys')}<ArrowUpRight className="h-3.5 w-3.5 text-muted-foreground" /></Link>
          <Link to="/portal/security" className="inline-flex min-h-11 items-center gap-2 rounded-lg border border-border px-3 hover:bg-accent focus-visible:outline-2 focus-visible:outline-primary"><ShieldCheck className="h-4 w-4" />{t('security:nav')}</Link>
        </div>
      </section>
      <Tabs id="profile-tabs" variant="underline" ariaLabel={t('profile:sections')} active={tab}
        onChange={(next) => { void navigate({ search: next === 'info' || next === 'signins' ? { tab: next } : {}, hash: '' }) }} items={[
          { id: 'activity', label: t('profile:usageHistory'), icon: Activity, panelId: 'profile-activity' },
          { id: 'info', label: t('profile:basicInfo'), icon: UserRound, panelId: 'profile-info' },
          { id: 'signins', label: t('profile:signins'), icon: Monitor, panelId: 'profile-signins' },
        ]} />
      <TabPanel id="profile-activity" labelledBy="profile-tabs-activity" active={tab === 'activity'}>
        <div className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3 pt-2">
            <div>
              <h2 className="font-semibold">{t('profile:usageHistory')}</h2>
              <p className="mt-1 text-xs text-muted-foreground">{scope === 'key' ? t('profile:keyScopeHint', { id: me.data?.key_id ?? '—' }) : t('profile:userScopeHint')}</p>
            </div>
            <div className="flex flex-wrap items-center gap-3">
              <UsageScope {...usageScope} ariaLabel={t('profile:scope')} onChange={usageScope.setScope} />
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <label htmlFor="profile-year">{t('profile:year')}</label>
                <select id="profile-year" value={selectedYear} onChange={(e) => setYear(Number(e.target.value))} className="h-11 rounded-lg border border-border bg-card px-3 font-medium text-foreground outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
                  {Array.from({ length: currentYear - firstYear + 1 }, (_, i) => currentYear - i).map((y) => <option key={y} value={y}>{y}</option>)}
                </select>
              </div>
            </div>
          </div>
          {query.isPending ? <LoadingState /> : query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} />
            : <ActivityContent key={`${query.data.scope}-${query.data.year}`} data={query.data} />}
        </div>
      </TabPanel>
      <TabPanel id="profile-info" labelledBy="profile-tabs-info" active={tab === 'info'}><BasicInfo active={tab === 'info'} /></TabPanel>
      <TabPanel id="profile-signins" labelledBy="profile-tabs-signins" active={tab === 'signins'}>
        <div className="grid min-w-0 items-start gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
          <RecentLoginsCard active={tab === 'signins'} />
          <ActiveSessionsCard active={tab === 'signins'} />
        </div>
      </TabPanel>
    </PageBody>
  )
}

function ActivityContent({ data }: { data: ActivityResponse }) {
  const { t, i18n } = useTranslation()
  const activity = useMemo(() => buildActivity(data), [data])
  const [metric, setMetric] = useState<Metric>('tokens')
  const lastDay = `${data.year}-12-31` < data.today ? `${data.year}-12-31` : data.today
  const [selected, setSelected] = useState(() => activity.days.findLast((day) => day.requests > 0)?.day ?? lastDay)
  const day = activity.lookup.get(selected)!
  const total = activity.total
  return (
    <>
      <div className="grid grid-cols-2 gap-3 max-sm:[&>div>span]:hidden max-sm:[&>div>div>span:nth-child(2)]:text-lg xl:grid-cols-4" aria-label={t('profile:yearSummary')}>
        <Stat label={t('profile:totalTokens')} value={formatCount(total.tokens, i18n.language)} icon={Cpu} sub={t('profile:tokenHint')} />
        <Stat label={t('profile:totalRequests')} value={formatCount(total.requests, i18n.language)} icon={Activity} sub={t('profile:selectedYear', { year: data.year })} />
        <Stat label={t('profile:totalSpend')} value={formatMoney(total.amount_micro, i18n.language)} icon={Coins} sub={t('profile:selectedYear', { year: data.year })} />
        <Stat label={t('profile:activeDays')} value={String(total.activeDays)} icon={CalendarDays} sub={t('profile:longestStreak', { count: activity.longestStreak })} />
      </div>
      <section className="min-w-0 space-y-5 rounded-xl border border-border bg-card p-4 shadow-card sm:p-5" aria-label={t('profile:activity')}>
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h3 className="font-semibold">{t('profile:activity')}</h3>
            <p className="mt-1 text-xs text-muted-foreground">{t('profile:timezone', { timezone: data.timezone })}</p>
          </div>
          <Segmented ariaLabel={t('profile:metric')} value={metric} onChange={setMetric} options={[
            { value: 'tokens', label: t('profile:tokens') },
            { value: 'requests', label: t('profile:requests') },
            { value: 'amount_micro', label: t('profile:spend') },
          ]} className="[&_button]:min-h-9" />
        </div>
        <UsageHeatmap days={activity.days} today={data.today} metric={metric} selected={selected} onSelect={setSelected} />
        {total.requests === 0 && <p className="rounded-lg bg-muted/60 px-3 py-2 text-sm text-muted-foreground">{t('profile:emptyYear')}</p>}
      </section>
      <section className="rounded-xl border border-border bg-card shadow-card" aria-label={t('profile:dayDetails')}>
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4">
          <div>
            <h3 className="font-semibold">{t('profile:dayDetails')}</h3>
            <p className="mt-1 text-sm text-muted-foreground" aria-live="polite">{new Intl.DateTimeFormat(i18n.language, { dateStyle: 'full', timeZone: 'UTC' }).format(calendarDate(selected))}</p>
          </div>
          <label className="flex items-center gap-2 text-sm text-muted-foreground">
            {t('profile:chooseDay')}
            <input type="date" value={selected} min={`${data.year}-01-01`} max={lastDay}
              onChange={(event) => { const value = event.target.value; if (activity.lookup.has(value) && value <= lastDay) setSelected(value) }}
              className="h-11 max-w-full rounded-lg border border-border bg-background px-3 text-foreground outline-none focus-visible:ring-2 focus-visible:ring-primary/40 dark:[color-scheme:dark]" />
          </label>
        </div>
        <DayDetails day={day} />
      </section>
    </>
  )
}

function DayDetails({ day }: { day: ActivityDay }) {
  const { t, i18n } = useTranslation()
  if (day.requests === 0) return <EmptyState title={t('profile:emptyDay')} hint={t('profile:emptyDayHint')} className="m-5 border-0 py-8" />
  const count = (n: number) => n.toLocaleString(i18n.language)
  return (
    <div className="space-y-5 p-5">
      <dl className="grid grid-cols-2 gap-x-4 gap-y-5 sm:grid-cols-3 xl:grid-cols-6">
        {[
          [t('profile:tokens'), count(day.tokens), ''],
          [t('profile:input'), count(day.prompt_tokens), `${t('portal:tokCached')} ${day.cache_read_known_requests === day.requests ? count(day.cached_tokens) : '—'} · ${t('charts:cacheWrite')} ${day.cache_write_tokens == null ? '—' : count(day.cache_write_tokens)}`],
          [t('profile:output'), count(day.completion_tokens), t('profile:reasoning', { count: day.reasoning_tokens })],
          [t('profile:requests'), count(day.requests), ''],
          [t('profile:spend'), formatMoney(day.amount_micro, i18n.language), ''],
          [t('profile:errors'), count(day.errors), ''],
        ].map(([label, value, hint]) => <div key={label} className="min-w-0"><dt className="text-xs text-muted-foreground">{label}</dt><dd className="mt-1 break-all text-lg font-semibold tabular-nums">{value}</dd>{hint && <dd className="mt-1 text-xs text-muted-foreground">{hint}</dd>}</div>)}
      </dl>
      <div className="min-w-0 space-y-3">
        <h3 className="text-sm font-medium">{t('profile:models')}</h3>
        <Table stickyHeader aria-label={t('profile:models')}>
          <THead><Tr>
            {[t('profile:model'), t('profile:tokens'), t('profile:requests'), t('profile:spend')].map((label, index) => <Th key={label} numeric={index > 0}>{label}</Th>)}
          </Tr></THead>
          <TBody>{[...day.models].sort((a, b) => (b.prompt_tokens + b.completion_tokens) - (a.prompt_tokens + a.completion_tokens)).map((model) => (
            <Tr key={model.model}>
              <Td className="max-w-64 break-all font-medium">{model.model}</Td>
              <Td numeric>{count(model.prompt_tokens + model.completion_tokens)}</Td>
              <Td numeric>{count(model.requests)}</Td>
              <Td numeric>{formatMoney(model.amount_micro, i18n.language)}</Td>
            </Tr>
          ))}</TBody>
        </Table>
      </div>
    </div>
  )
}
