import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  Copy,
  Ellipsis,
  ExternalLink,
  FilePlus2,
  Gauge,
  Globe,
  KeyRound,
  LayoutGrid,
  LayoutList,
  Loader2,
  MessageSquareText,
  Pencil,
  Pin,
  Plus,
  Power,
  PowerOff,
  RotateCcw,
  Server,
  ShieldCheck,
  SlidersHorizontal,
  Trash2,
  Wallet,
} from 'lucide-react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelBalance, ChannelRow, ChannelTab } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { ActionGroup, ICON_BUTTON_CLASS, IconButton } from '@/components/ui/icon-button'
import { ChannelQuotaCell } from '@/features/channels/account-controls/ChannelQuotaCell'
import { BatchChannelsDrawer } from '@/features/channels/BatchChannelsDrawer'
import { ChannelCreateWorkspace } from '@/features/channels/ChannelCreateWorkspace'
import { ChannelDrawer, type ChannelEditFocus } from '@/features/channels/ChannelDrawer'
import { ChannelGridCard } from '@/features/channels/ChannelGridCard'
import { ChannelTryDrawer } from '@/features/channels/ChannelTryDrawer'
import { ChannelVerifyDrawer } from '@/features/channels/ChannelVerifyDrawer'
import { ProviderChip, ProviderMark } from '@/features/channels/ProviderChip'
import { Switch } from '@/components/ui/switch'
import {
  ChannelUsage,
  Health24h,
  KeyStateDetail,
  KeyStateSummary,
  LastBalance,
  LastProbe,
  channelRowHealth,
  useChannelHealth24h,
  useChannelUsage,
} from '@/features/channels/ChannelHealthCell'
import { RouteDiagnosisDrawer } from '@/features/channels/RouteDiagnosis'
import { BatchEgressDrawer } from '@/features/channels/ChannelEgress'
import { describeEgress } from '@/features/proxies/EgressPicker'
import { proxyGroupOptions, proxyOptions } from '@/features/proxies/options'
import { Checkbox } from '@/components/ui/checkbox'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Menu, MenuItem, MenuSeparator } from '@/components/ui/menu'
import { Tooltip } from '@/components/ui/tooltip'
import {
  CHANNEL_STATUS_FILTERS,
  CHANNEL_STATUS_VALUE,
  PROVIDERS,
  balanceSupported,
  costMilliToRatio,
  providerConsoleUrl,
} from '@/features/channels/types'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { SearchInput } from '@/components/ui/search-input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { SelectionBar } from '@/components/ui/selection-bar'
import { TableSkeleton } from '@/components/ui/skeleton'
import { TabPanel, Tabs } from '@/components/ui/tabs'
import { toast } from '@/components/ui/toast'
import { useDraft } from '@/hooks/use-draft'
import { usePagination } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { loadAll } from '@/lib/load-all'
import { describeError } from '@/lib/i18n'
import { maskAccount, maskChannelName } from '@/lib/mask'
import { formatUpstreamBalance } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { oneOf, text } from '@/lib/search-params'
import { useConfirm } from '@/components/ui/confirm'
import { cn } from '@/lib/utils'

const routeApi = getRouteApi('/admin/channels')

const CHANNEL_VIEW_KEY = 'okapi.channels.view'
type ChannelViewMode = 'list' | 'cards'

function readChannelViewMode(): ChannelViewMode {
  try {
    return localStorage.getItem(CHANNEL_VIEW_KEY) === 'cards' ? 'cards' : 'list'
  } catch {
    return 'list'
  }
}

/// 界面上显示的渠道名：渠道名就是订阅账号（邮箱，或等于某把 key 的账号标识）时遮住中间，其余原样。
const shownName = (c: ChannelRow) => maskChannelName(c.name, c.keys.map((k) => k.account_label))

/// 列表接口的一页：`total` / `enabled` 都是过滤集内的计数（未筛选时即全站）。
interface ChannelPage {
  data: ChannelRow[]
  total: number
  enabled: number
}

/// 渠道列表页。
///
/// 「所见即可点」的双层结构化渠道卡片行：
/// - 点击名称打开编辑抽屉；点击池 / 未入池直达渠道池配置；点击模型数悬停看全表、点击直达模型编辑；
///   点击优先级 / 成本直达调度参数；点击出口代理直达出口设置；点击测活延迟 / 错误码原地重测；
///   点击额度条直达额度上限；点击上游余额原地刷新；点击近 24h 任一位置看健康时间线；点击近 30 天消费直达该渠道请求日志。
export function ChannelsPage() {
  const { i18n, t } = useTranslation()
  const queryClient = useQueryClient()
  const [picked, setPicked] = useState<Set<number>>(new Set())
  const [attentionOnly, setAttentionOnly] = useState(false)
  const [viewMode, setViewModeState] = useState<ChannelViewMode>(readChannelViewMode)
  const setViewMode = (mode: ChannelViewMode) => {
    setViewModeState(mode)
    try {
      localStorage.setItem(CHANNEL_VIEW_KEY, mode)
    } catch {
      /* ignore storage failure */
    }
  }
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const activeTab: ChannelTab = search.tab ?? 'list'
  const setActiveTab = (next: ChannelTab) => {
    void navigate({
      search: (prev) => ({
        ...prev,
        tab: next === 'list' ? undefined : next,
      }),
    })
  }
  const [createSession, setCreateSession] = useState(0)
  const [createTemplate, setCreateTemplate] = useState<ChannelRow | undefined>(undefined)
  const query = search.q ?? ''
  const providerFilter = search.provider ?? ''
  const statusFilter = search.status ?? ''
  const [draft, setDraft] = useDraft(query)
  const [drawer, setDrawer] = useState<
    { channel: ChannelRow; focus?: ChannelEditFocus } | null
  >(null)
  const [diagnosing, setDiagnosing] = useState(false)
  const [trying, setTrying] = useState<ChannelRow | null>(null)
  const [verifying, setVerifying] = useState<ChannelRow | null>(null)
  const [batchEgress, setBatchEgress] = useState(false)
  const [batchEdit, setBatchEdit] = useState(false)
  const proxies = useQuery(proxyOptions())
  const proxyGroups = useQuery(proxyGroupOptions())
  const [testingId, setTestingId] = useState<number | null>(null)
  const [balancingId, setBalancingId] = useState<number | null>(null)
  const [pinningId, setPinningId] = useState<number | null>(null)
  const { confirm, dialog } = useConfirm()
  const pager = usePagination()

  const setFilters = (next: { q?: string; provider?: string; status?: string }) => {
    setAttentionOnly(false)
    void navigate({
      search: (prev) => ({
        ...prev,
        ...('q' in next ? { q: text(next.q) } : {}),
        ...('provider' in next ? { provider: oneOf(next.provider, PROVIDERS) } : {}),
        ...('status' in next ? { status: oneOf(next.status, CHANNEL_STATUS_FILTERS) } : {}),
        page: undefined,
      }),
    })
  }

  const health = useChannelHealth24h()
  const usage = useChannelUsage()
  const channels = useQuery({
    queryKey: [...qk.adminChannels, query, providerFilter, statusFilter, pager.offset, pager.limit],
    queryFn: () => {
      const params = new URLSearchParams({
        limit: String(pager.limit),
        offset: String(pager.offset),
      })
      if (query !== '') params.set('q', query)
      if (providerFilter !== '') params.set('provider', providerFilter)
      if (statusFilter !== '') params.set('status', CHANNEL_STATUS_VALUE[statusFilter])
      return apiFetch<ChannelPage>(`/admin/channels?${params}`)
    },
    placeholderData: keepPreviousData,
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminChannels })
  const fail = (err: unknown) => toast.error(describeError(err))

  const setStatus = useMutation({
    mutationFn: (arg: { id: number; status: number }) =>
      apiFetch(`/admin/channels/${arg.id}/status`, {
        method: 'POST',
        body: { status: arg.status },
      }),
    onSuccess: invalidate,
    onError: fail,
  })

  const setPinned = useMutation({
    mutationFn: (arg: { id: number; pinned: boolean }) => {
      setPinningId(arg.id)
      return apiFetch<{ ok: boolean; pinned: boolean; pinned_at: string | null }>(
        `/admin/channels/${arg.id}/pin`,
        {
          method: 'POST',
          body: { pinned: arg.pinned },
        },
      )
    },
    onSuccess: invalidate,
    onError: fail,
    onSettled: () => setPinningId(null),
  })

  const remove = useMutation({
    mutationFn: (id: number) => apiFetch(`/admin/channels/${id}`, { method: 'DELETE' }),
    onSuccess: () => {
      setDrawer(null)
      toast.success(t('common:success'))
      invalidate()
    },
    onError: fail,
  })

  const duplicate = useMutation({
    mutationFn: (c: ChannelRow) =>
      apiFetch(`/admin/channels/${c.id}/duplicate`, {
        method: 'POST',
        body: { name: `${c.name}-copy` },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: fail,
  })

  const probeOne = (id: number, model?: string) =>
    apiFetch<{
      ok: boolean
      latency_ms?: number
      error_code?: string
      http_status?: number
      scope?: string
      upstream_body?: string
    }>(`/admin/channels/${id}/test`, { method: 'POST', body: model === undefined ? {} : { model } })
  const test = useMutation({
    mutationFn: (c: ChannelRow) => {
      setTestingId(c.id)
      return probeOne(c.id, c.models[0])
    },
    onSuccess: (r, c) => {
      if (r.ok) toast.success(shownName(c), t('admin:testOk', { ms: r.latency_ms ?? 0 }))
      else if (r.error_code === 'upstream_not_json') toast.error(shownName(c), t('admin:testNotJson'))
      else if (r.scope === 'model')
        toast.error(
          shownName(c),
          t('admin:testModelFail', {
            model: c.models[0] ?? '',
            code: r.error_code ?? String(r.http_status ?? ''),
            detail: (r.upstream_body ?? '').slice(0, 120),
          }),
        )
      else toast.error(shownName(c), t('admin:testFail', { code: r.error_code ?? 'unknown' }))
      invalidate()
    },
    onError: fail,
    onSettled: () => setTestingId(null),
  })

  const balance = useMutation({
    mutationFn: (c: ChannelRow) => {
      setBalancingId(c.id)
      return apiFetch<ChannelBalance>(`/admin/channels/${c.id}/balance`)
    },
    onSuccess: (r, c) => {
      toast.success(
        shownName(c),
        t('admin:balanceResult', {
          amount: formatUpstreamBalance(r.balance_micro, r.currency, i18n.language),
          probe: r.probe,
        }),
      )
      invalidate()
    },
    onError: fail,
    onSettled: () => setBalancingId(null),
  })

  const testAll = useMutation({
    mutationFn: async () => {
      const rows = await loadAll<ChannelPage['data'][number]>('/admin/channels?status=1', (c) => c.id)
      const ids = rows.map((c) => c.id)
      let ok = 0
      let failed = 0
      const queue = [...ids]
      const worker = async () => {
        for (let id = queue.shift(); id !== undefined; id = queue.shift()) {
          try {
            const r = await probeOne(id)
            if (r.ok) ok += 1
            else failed += 1
          } catch {
            failed += 1
          }
        }
      }
      await Promise.all([worker(), worker(), worker()])
      return { ok, failed, total: ids.length }
    },
    onSuccess: (r) => {
      const msg = t('admin:testAllDone', r)
      if (r.failed > 0) toast.warning(msg)
      else toast.success(msg)
      invalidate()
    },
    onError: fail,
  })

  const batch = useMutation({
    mutationFn: (action: 'enable' | 'disable' | 'delete' | 'clear_cooldown') =>
      apiFetch<{ affected: number }>('/admin/channels/batch', {
        method: 'POST',
        body: { ids: [...picked], action },
      }),
    onSuccess: (r) => {
      toast.success(t('admin:batchDone', { n: r.affected }))
      setPicked(new Set())
      invalidate()
    },
    onError: fail,
  })

  const rows = channels.data?.data ?? []
  const total = channels.data?.total ?? 0
  const enabledCount = channels.data?.enabled ?? 0
  const enriched = rows.map((c) => {
    const enabled = c.status === 1
    const pools = c.pools ?? []
    const orphan = enabled && pools.length === 0
    const rowStat = health.data?.data.find((s) => s.channel_id === c.id)
    const rowUsage = usage.data?.data.find((u) => u.channel_id === c.id)
    const rowHealth = channelRowHealth(c.keys, enabled, orphan, rowStat)
    return { channel: c, enabled, pools, orphan, rowStat, rowUsage, rowHealth }
  })
  const issueCount = enriched.filter((r) => r.rowHealth === 'warning' || r.rowHealth === 'destructive').length
  const focusingIssues = attentionOnly && issueCount > 0
  const visibleRows = focusingIssues
    ? enriched.filter((r) => r.rowHealth === 'warning' || r.rowHealth === 'destructive')
    : enriched

  const allPicked = visibleRows.length > 0 && visibleRows.every((r) => picked.has(r.channel.id))
  const somePicked = visibleRows.some((r) => picked.has(r.channel.id))
  const filtered = query !== '' || providerFilter !== '' || statusFilter !== ''
  const applySearch = () => setFilters({ q: draft })
  const clearFilters = () => setFilters({ q: '', provider: '', status: '' })
  const openCreate = () => setActiveTab('create')
  const openCreateFromTemplate = (channel: ChannelRow) => {
    setCreateTemplate(channel)
    setCreateSession((n) => n + 1)
    setActiveTab('create')
  }
  const openEdit = (channel: ChannelRow, focus?: ChannelEditFocus) => setDrawer({ channel, focus })
  const handleCreated = () => {
    invalidate()
    setCreateTemplate(undefined)
    setCreateSession((n) => n + 1)
    setActiveTab('list')
  }
  const egressLabel = (c: ChannelRow) => {
    const egress = c.egress
    if (egress?.mode === 'proxy') {
      return proxies.data?.find((proxy) => proxy.id === egress.proxy_id)?.name ?? `#${egress.proxy_id}`
    }
    if (egress?.mode === 'group') {
      return proxyGroups.data?.find((group) => group.code === egress.group_code)?.name ?? egress.group_code
    }
    return describeEgress(t, egress, proxies.data, proxyGroups.data)
  }

  const togglePick = (id: number) =>
    setPicked((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  const editingChannel =
    drawer !== null
      ? (rows.find((c) => c.id === drawer.channel.id) ?? drawer.channel)
      : undefined

  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:channelsTitle')}
        description={t('admin:channelsDesc')}
        meta={channels.data && t('admin:channelsSummary', { total, enabled: enabledCount })}
        action={
          activeTab === 'create' ? (
            <Button variant="outline" onClick={() => setActiveTab('list')}>
              <ArrowLeft aria-hidden className="h-4 w-4" />
              {t('admin:channelBackToList')}
            </Button>
          ) : (
            <>
              <Button
                variant="outline"
                loading={testAll.isPending}
                disabled={enabledCount === 0 && !filtered}
                onClick={() => testAll.mutate()}
              >
                {testAll.isPending ? t('admin:testAllRunning') : t('admin:testAll')}
              </Button>
              <Button variant="outline" onClick={() => setDiagnosing(true)}>
                {t('admin:diagTitle')}
              </Button>
              <Button onClick={openCreate}>
                <Plus aria-hidden className="h-4 w-4" />
                {t('admin:createChannel')}
              </Button>
            </>
          )
        }
      />

      <Tabs
        id="channels-tabs"
        variant="underline"
        ariaLabel={t('admin:channelsTitle')}
        active={activeTab}
        onChange={(id) => setActiveTab(id as ChannelTab)}
        items={[
          {
            id: 'list',
            label: t('admin:channelTabList'),
            icon: Server,
            count: channels.data?.total,
            panelId: 'channels-panel-list',
          },
          {
            id: 'create',
            label: t('admin:channelTabCreate'),
            icon: Plus,
            panelId: 'channels-panel-create',
          },
        ]}
      />

      <TabPanel
        id="channels-panel-create"
        labelledBy="channels-tabs-create"
        active={activeTab === 'create'}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain pr-0.5 pb-6 [scrollbar-gutter:stable]"
      >
        <ChannelCreateWorkspace
          key={createSession}
          template={createTemplate}
          onClearTemplate={() => {
            setCreateTemplate(undefined)
            setCreateSession((n) => n + 1)
          }}
          onCancel={() => setActiveTab('list')}
          onCreated={handleCreated}
          onReset={() => {
            setCreateTemplate(undefined)
            setCreateSession((n) => n + 1)
          }}
        />
      </TabPanel>

      <TabPanel
        id="channels-panel-list"
        labelledBy="channels-tabs-list"
        active={activeTab === 'list'}
        className="list-page-section flex-1"
      >
      <Toolbar
        filters={
          <>
            <SearchInput
              id="ch-search"
              hideIcon
              className="w-85"
              aria-label={t('admin:channelSearchHint')}
              value={draft}
              placeholder={t('admin:channelSearchHint')}
              onChange={setDraft}
              onSubmit={applySearch}
            />
            <Segmented
              size="field"
              ariaLabel={t('common:status')}
              value={statusFilter}
              onChange={(status) => setFilters({ status })}
              options={[
                { value: '', label: t('common:all') },
                { value: 'enabled', label: t('admin:channelFilterEnabled') },
                { value: 'disabled', label: t('admin:channelFilterDisabled') },
              ]}
            />
            <Select
              id="ch-provider"
              className="w-40"
              aria-label={t('admin:provider')}
              value={providerFilter}
              onChange={(provider) => setFilters({ provider })}
              placeholder={t('admin:channelAllProviders')}
              options={PROVIDERS.map((p) => ({ value: p, label: p }))}
            />
            {filtered && (
              <Button size="sm" variant="ghost" onClick={clearFilters}>
                {t('common:clearFilters')}
              </Button>
            )}
          </>
        }
      />

      {channels.isError ? (
        <ErrorState message={describeError(channels.error)} onRetry={() => void channels.refetch()} />
      ) : channels.isPending ? (
        <TableSkeleton rows={6} cols={4} />
      ) : rows.length === 0 ? (
        filtered ? (
          <EmptyState
            title={t('common:noResults')}
            hint={t('common:noResultsHint')}
            action={
              <Button variant="outline" onClick={clearFilters}>
                {t('common:clearFilters')}
              </Button>
            }
          />
        ) : (
          <EmptyState
            icon={Server}
            hint={t('admin:channelsEmptyHint')}
            action={
              <Button onClick={openCreate}>
                <Plus className="h-4 w-4" />
                {t('admin:createChannel')}
              </Button>
            }
          />
        )
      ) : (
        <div data-slot="table-frame" className="relative flex min-w-0 w-full flex-col">
          <div
            data-slot="table-viewport"
            className="min-h-0 min-w-0 w-full flex-1 overflow-y-auto overscroll-contain px-0.5 pb-1"
          >
            {(() => {
              const listControlBar = (
                <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 px-4 py-1 sm:px-5">
                  <Checkbox
                    checked={allPicked}
                    indeterminate={somePicked && !allPicked}
                    label={t('admin:batchPickAll')}
                    className="shrink-0 text-muted-foreground [&_span.text-sm]:whitespace-nowrap [&_span.text-sm]:text-xs [&_span.text-sm]:font-medium [&_span.text-sm]:text-muted-foreground"
                    onChange={(on) =>
                      setPicked((prev) => {
                        const next = new Set(prev)
                        for (const r of visibleRows) {
                          if (on) next.add(r.channel.id)
                          else next.delete(r.channel.id)
                        }
                        return next
                      })
                    }
                  />
                  <div className="flex flex-wrap items-center gap-2">
                    {issueCount > 0 && (
                      <button
                        type="button"
                        data-slot="channel-attention-filter"
                        aria-pressed={focusingIssues}
                        onClick={() => setAttentionOnly((v) => !v)}
                        className={cn(
                          'inline-flex h-7 shrink-0 cursor-pointer items-center gap-1.5 rounded-full px-2.5 text-[11px] font-medium whitespace-nowrap tabular-nums ring-1 ring-inset outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary/40',
                          focusingIssues
                            ? 'bg-warning/15 text-warning ring-warning/35'
                            : 'bg-card text-muted-foreground ring-border/80 hover:border-warning/40 hover:text-foreground',
                        )}
                      >
                        <AlertTriangle aria-hidden className={cn('h-3 w-3', focusingIssues ? 'text-warning' : 'text-warning/85')} />
                        <span>{t('admin:channelAttentionOnly')}</span>
                        <span className={cn('rounded-full px-1.5 text-[10.5px] font-semibold', focusingIssues ? 'bg-warning/20 text-warning' : 'bg-muted text-foreground/80')}>
                          {issueCount}
                        </span>
                      </button>
                    )}
                    <div className="max-w-full shrink-0">
                      <Segmented<ChannelViewMode>
                        size="sm"
                        ariaLabel={t('admin:channelViewMode')}
                        dataSlot="channel-view-toggle"
                        value={viewMode}
                        onChange={setViewMode}
                        options={[
                          { value: 'list', label: t('admin:channelViewList'), icon: LayoutList },
                          { value: 'cards', label: t('admin:channelViewCards'), icon: LayoutGrid },
                        ]}
                      />
                    </div>
                  </div>
                </div>
              )

              if (viewMode === 'cards') {
                return (
                  <div className="flex flex-col gap-2">
                    <div className="sticky top-0 z-10 bg-background/95 backdrop-blur-sm">
                      {listControlBar}
                    </div>
                    <div
                      data-slot="channel-card-grid"
                      aria-label={t('admin:channelsTitle')}
                      className="grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3"
                    >
                      {visibleRows.map(({ channel: c, enabled, orphan, rowStat, rowUsage, rowHealth }) => {
                        const selected = picked.has(c.id)
                        const egress =
                          c.egress !== undefined && c.egress.mode !== 'inherit'
                            ? {
                                label: egressLabel(c),
                                title: describeEgress(t, c.egress, proxies.data, proxyGroups.data),
                                mono: c.egress.mode === 'proxy',
                              }
                            : undefined
                        return (
                          <ChannelGridCard
                            key={c.id}
                            channel={c}
                            enabled={enabled}
                            orphan={orphan}
                            selected={selected}
                            rowHealth={rowHealth}
                            rowStat={rowStat}
                            hourlyStart={health.data?.hourly_start}
                            rowUsage={rowUsage}
                            usageSettled={usage.data !== undefined}
                            egress={egress}
                            testing={testingId === c.id}
                            balancing={balancingId === c.id}
                            pinning={pinningId === c.id}
                            statusPending={setStatus.isPending}
                            onTogglePick={() => togglePick(c.id)}
                            onToggleStatus={(on) => setStatus.mutate({ id: c.id, status: on ? 1 : 2 })}
                            onTogglePin={() => setPinned.mutate({ id: c.id, pinned: !c.pinned })}
                            onTest={() => test.mutate(c)}
                            onBalance={() => balance.mutate(c)}
                            onEdit={(focus) => openEdit(c, focus)}
                            onTry={() => setTrying(c)}
                            onVerify={() => setVerifying(c)}
                            onDuplicate={() => duplicate.mutate(c)}
                            onUseAsTemplate={() => openCreateFromTemplate(c)}
                            onDelete={() =>
                              confirm({
                                title: t('common:confirmDeleteTitle', { name: shownName(c) }),
                                description: t('common:confirmChannelDelete'),
                                requireText: shownName(c),
                                onConfirm: () => remove.mutate(c.id),
                              })
                            }
                          />
                        )
                      })}
                    </div>
                  </div>
                )
              }

              return (
            <table
              aria-label={t('admin:channelsTitle')}
              className="-mt-2 w-full border-separate border-spacing-x-0 border-spacing-y-2"
            >
              <thead>
                <tr>
                  <th className="sticky top-0 z-10 bg-background/95 p-0 text-left font-normal backdrop-blur-sm">
                    {listControlBar}
                  </th>
                </tr>
              </thead>
              <tbody>
                {visibleRows.map(({ channel: c, enabled, orphan, rowStat, rowUsage, rowHealth }) => {
                  const selected = picked.has(c.id)
                  const pinned = Boolean(c.pinned)
                  const hasKeyAlert = enabled && c.keys.length > 0 && c.keys.some((k) => k.status !== 1)
                  const canQueryBalance = enabled && balanceSupported(c.provider)
                  const egress =
                    c.egress !== undefined && c.egress.mode !== 'inherit'
                      ? {
                          label: egressLabel(c),
                          title: describeEgress(t, c.egress, proxies.data, proxyGroups.data),
                          mono: c.egress.mode === 'proxy',
                        }
                      : undefined

                  return (
                    <tr
                      key={c.id}
                      data-selected={selected || undefined}
                      data-enabled={enabled || undefined}
                      data-pinned={pinned || undefined}
                      data-health={rowHealth}
                      className="group/row"
                    >
                      <td className="p-0 align-top">
                        <div
                          data-slot="channel-card"
                          className={cn(
                            'relative overflow-hidden rounded-2xl border bg-card shadow-card transition-[border-color,box-shadow,background-color] duration-150',
                            selected
                              ? 'border-primary/55 bg-primary/[0.025] ring-1 ring-primary/25 dark:bg-primary/[0.06]'
                              : rowHealth === 'destructive'
                                ? 'border-destructive/30 hover:border-destructive/50'
                                : rowHealth === 'warning'
                                  ? 'border-warning/35 hover:border-warning/55'
                                  : pinned
                                    ? 'border-primary/30 hover:border-primary/50 hover:shadow-[0_4px_14px_-4px_oklch(0.24_0.015_70/0.09),0_1px_2px_oklch(0.24_0.015_70/0.04)] dark:hover:shadow-[inset_0_1px_0_oklch(1_0_0/0.07),0_8px_22px_-6px_oklch(0_0_0/0.55)]'
                                    : enabled
                                      ? 'border-border/80 hover:border-border hover:shadow-[0_4px_14px_-4px_oklch(0.24_0.015_70/0.09),0_1px_2px_oklch(0.24_0.015_70/0.04)] dark:hover:shadow-[inset_0_1px_0_oklch(1_0_0/0.07),0_8px_22px_-6px_oklch(0_0_0/0.55)]'
                                      : 'border-border/65 bg-card/70 hover:border-border/90 hover:bg-card/90',
                          )}
                        >
                          {/* 左侧 3px 圆角语义状态条：内缩避开圆角裁切，异常、置顶与选中常亮，健康态悬停轻亮 */}
                          <span
                            aria-hidden
                            className={cn(
                              'pointer-events-none absolute top-3 bottom-3 left-[3px] w-[3px] rounded-full transition-colors',
                              selected
                                ? 'bg-primary'
                                : rowHealth === 'destructive'
                                  ? 'bg-destructive'
                                  : rowHealth === 'warning'
                                    ? 'bg-warning'
                                    : pinned
                                      ? 'bg-primary/55'
                                      : enabled
                                        ? 'bg-transparent group-hover/row:bg-primary/45'
                                        : 'bg-transparent',
                            )}
                          />

                          {/* 上层：身份与路由 + 实时状态与快捷控制 */}
                          <div className="px-4 pt-3 pb-2.5 sm:px-5">
                            <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-2.5">
                              {/* 左侧：勾选 + 品牌徽章 + 名称 / 协议档位 / 出口与路由（各项所见即可点） */}
                              <div className="flex min-w-0 flex-1 items-start gap-3">
                                <div className="flex h-10 items-center">
                                  <Checkbox
                                    srLabel={t('admin:batchPick', { name: shownName(c) })}
                                    checked={selected}
                                    onChange={() => togglePick(c.id)}
                                  />
                                </div>
                                <ChannelIdentity
                                  channel={c}
                                  egress={egress}
                                  onEdit={(focus) => openEdit(c, focus)}
                                />
                              </div>

                              {/* 右侧：状态胶囊 + 未入池 + 最近测活（点即重测）| 调度开关 + 动作组 */}
                              <div className="flex w-full shrink-0 flex-wrap items-center justify-between gap-x-3 gap-y-2 pl-7 sm:w-auto sm:justify-end sm:pl-2">
                                <div data-slot="channel-status" className="flex items-center gap-2">
                                  <KeyStateSummary
                                    keys={c.keys}
                                    enabled={enabled}
                                    onInspectKeys={() => openEdit(c, c.keys.length === 0 ? 'conn' : 'keys')}
                                  />
                                  {orphan && (
                                    <Tooltip
                                      content={
                                        <div className="w-[240px] max-w-full space-y-2 text-left">
                                          <div className="flex items-start gap-2 text-[11.5px] leading-relaxed text-foreground">
                                            <span aria-hidden className="mt-1.5 size-1.5 shrink-0 rounded-full bg-destructive" />
                                            <span>{t('admin:poolOrphanWarning')}</span>
                                          </div>
                                          <div className="border-t border-border/60 pt-1.5 text-[11px] text-muted-foreground">
                                            {t('admin:channelClickPools')}
                                          </div>
                                        </div>
                                      }
                                    >
                                      <button
                                        type="button"
                                        onClick={() => openEdit(c, 'pools')}
                                        className="inline-flex min-h-[22px] shrink-0 cursor-pointer items-center rounded-md bg-destructive/10 px-2 py-0 text-[11px] font-medium whitespace-nowrap text-destructive ring-1 ring-inset ring-destructive/22 outline-none transition-colors hover:bg-destructive/16 focus-visible:ring-2 focus-visible:ring-primary/40"
                                      >
                                        {t('admin:channelOrphan')}
                                      </button>
                                    </Tooltip>
                                  )}
                                  <LastProbe
                                    probe={c.last_test}
                                    probing={testingId === c.id}
                                    onProbe={() => test.mutate(c)}
                                  />
                                </div>

                                <span aria-hidden className="hidden h-5 w-px bg-border/70 sm:inline-block" />

                                <div className="flex items-center gap-2.5">
                                  <Switch
                                    hideLabel
                                    className="[&>span:last-child]:mt-0"
                                    label={t('admin:channelScheduleToggle', { name: shownName(c) })}
                                    checked={enabled}
                                    disabled={setStatus.isPending}
                                    onChange={(on) => setStatus.mutate({ id: c.id, status: on ? 1 : 2 })}
                                  />
                                  <ActionGroup label={t('common:actions')}>
                                    <IconButton
                                      icon={Pin}
                                      label={pinned ? t('admin:channelUnpin') : t('admin:channelPin')}
                                      pressed={pinned}
                                      loading={pinningId === c.id}
                                      onClick={() => setPinned.mutate({ id: c.id, pinned: !pinned })}
                                      className={cn(
                                        pinned && 'bg-primary/10 text-primary hover:bg-primary/15 hover:text-primary [&_svg]:fill-current',
                                      )}
                                    />
                                    <IconButton
                                      icon={Activity}
                                      label={t('admin:testChannel')}
                                      loading={testingId === c.id}
                                      onClick={() => test.mutate(c)}
                                    />
                                    <IconButton
                                      icon={Pencil}
                                      label={t('common:edit')}
                                      onClick={() => openEdit(c)}
                                    />
                                    <Menu
                                      strategy="fixed"
                                      className="min-w-44"
                                      trigger={
                                        <Button
                                          size="icon"
                                          variant="ghost"
                                          aria-label={t('admin:channelMoreActions')}
                                          className={ICON_BUTTON_CLASS}
                                        >
                                          <Ellipsis />
                                        </Button>
                                      }
                                    >
                                      <MenuItem icon={MessageSquareText} onSelect={() => setTrying(c)}>
                                        {t('admin:channelTryAction')}
                                      </MenuItem>
                                      <MenuItem icon={ShieldCheck} onSelect={() => setVerifying(c)}>
                                        {t('admin:channelVerifyAction')}
                                      </MenuItem>
                                      {balanceSupported(c.provider) && (
                                        <MenuItem
                                          icon={Wallet}
                                          disabled={balancingId === c.id}
                                          onSelect={() => balance.mutate(c)}
                                        >
                                          {t('admin:balanceQuery')}
                                        </MenuItem>
                                      )}
                                      <MenuItem
                                        icon={Gauge}
                                        onSelect={() => openEdit(c, 'limits')}
                                      >
                                        {t('admin:channelLimitsTitle')}
                                      </MenuItem>
                                      <MenuItem icon={Copy} onSelect={() => duplicate.mutate(c)}>
                                        {t('admin:duplicate')}
                                      </MenuItem>
                                      <MenuItem icon={FilePlus2} onSelect={() => openCreateFromTemplate(c)}>
                                        {t('admin:channelUseAsTemplate')}
                                      </MenuItem>
                                      <MenuSeparator />
                                      <MenuItem
                                        icon={Trash2}
                                        destructive
                                        onSelect={() =>
                                          confirm({
                                            title: t('common:confirmDeleteTitle', { name: shownName(c) }),
                                            description: t('common:confirmChannelDelete'),
                                            requireText: shownName(c),
                                            onConfirm: () => remove.mutate(c.id),
                                          })
                                        }
                                      >
                                        {t('common:delete')}
                                      </MenuItem>
                                    </Menu>
                                  </ActionGroup>
                                </div>
                              </div>
                            </div>

                            {/* 异常凭证诊断条：露出错误原因，整条点击直达凭证与恢复操作 */}
                            {hasKeyAlert && (
                              <div className="mt-2 pl-7 sm:pl-[4.75rem]">
                                <KeyStateDetail
                                  keys={c.keys}
                                  enabled={enabled}
                                  onInspect={() => openEdit(c, 'keys')}
                                />
                              </div>
                            )}
                          </div>

                          {/* 下层遥测仪表盘：三栏定比对齐——额度与余额 / 近 24h 健康与 TTFT / 近 30 天用量 */}
                          <div className="border-t border-border/60 bg-muted/[0.26] px-4 py-2.5 sm:px-5 dark:bg-white/[0.022]">
                            <div className="grid grid-cols-1 gap-2.5 md:grid-cols-[minmax(0,1.55fr)_minmax(0,1.05fr)_minmax(11.5rem,0.72fr)] md:items-center md:gap-0 md:divide-x md:divide-border/60">
                              {/* 区域一：额度仪表（点击直达额度上限）与上游余额（点时间即原地刷新） */}
                              <div
                                data-slot="channel-quota"
                                className="flex min-w-0 flex-col justify-center gap-2 md:pr-5"
                              >
                                <div
                                  title={enabled ? t('admin:channelClickLimits') : undefined}
                                  onClick={(e) => {
                                    if (!enabled) return
                                    if ((e.target as HTMLElement).closest('button, a')) return
                                    openEdit(c, 'limits')
                                  }}
                                  className={cn(
                                    'min-w-0 empty:hidden',
                                    enabled && '-mx-2 -my-1 cursor-pointer rounded-lg px-2 py-1 transition-colors hover:bg-accent/65',
                                  )}
                                >
                                  <ChannelQuotaCell
                                    channelId={c.id}
                                    provider={c.provider}
                                    control={c.settings?.account_control}
                                    enabled={enabled}
                                    fallback={
                                      c.last_balance ? null : canQueryBalance ? (
                                        <div className="flex flex-col gap-0.5">
                                          <span className="text-[11px] leading-4 font-medium text-muted-foreground/80">{t('admin:channelColQuota')}</span>
                                          <button
                                            type="button"
                                            disabled={balancingId === c.id}
                                            onClick={() => balance.mutate(c)}
                                            className="-ml-1.5 inline-flex w-fit cursor-pointer items-center gap-1 rounded-md px-1.5 py-0.5 text-xs font-medium text-primary outline-none transition-colors hover:bg-primary/10 focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-wait disabled:opacity-60"
                                          >
                                            {balancingId === c.id
                                              ? <Loader2 aria-hidden className="h-3 w-3 animate-spin" />
                                              : <Wallet aria-hidden className="h-3 w-3" />}
                                            {t('admin:balanceQuery')}
                                          </button>
                                        </div>
                                      ) : (
                                        <div className="flex flex-col gap-0.5">
                                          <span className="text-[11px] leading-4 font-medium text-muted-foreground/75">{t('admin:channelColQuota')}</span>
                                          <span className="text-sm leading-5 text-muted-foreground/60" aria-hidden>—</span>
                                        </div>
                                      )
                                    }
                                  />
                                </div>
                                <LastBalance
                                  balance={c.last_balance}
                                  refreshing={balancingId === c.id}
                                  onRefresh={balanceSupported(c.provider) ? () => balance.mutate(c) : undefined}
                                />
                              </div>

                              {/* 区域二：近 24h 健康度与首字延迟（整块点开时间线抽屉） */}
                              <div
                                data-slot="channel-health"
                                className="flex min-w-0 flex-col justify-center border-t border-border/45 pt-2.5 md:border-t-0 md:px-5 md:pt-0"
                              >
                                <Health24h
                                  stat={rowStat}
                                  hourlyStart={health.data?.hourly_start}
                                  channel={{ id: c.id, name: c.name, provider: c.provider }}
                                />
                              </div>

                              {/* 区域三：近 30 天花费与流量（点击直达该渠道请求日志） */}
                              <div className="flex min-w-0 flex-col justify-center border-t border-border/45 pt-2.5 md:border-t-0 md:pl-5 md:pt-0">
                                <ChannelUsage
                                  usage={rowUsage}
                                  channelId={c.id}
                                  settled={usage.data !== undefined}
                                />
                              </div>
                            </div>
                          </div>
                        </div>
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
              )
            })()}
          </div>
        </div>
      )}

      <Pagination {...pager} total={channels.data?.total} />
      </TabPanel>

      <SelectionBar count={activeTab === 'list' ? picked.size : 0} onClear={() => setPicked(new Set())}>
        <Button size="sm" variant="outline" loading={batch.isPending} onClick={() => batch.mutate('enable')}>
          <Power className="h-3.5 w-3.5" />
          {t('common:enabled')}
        </Button>
        <Button size="sm" variant="outline" loading={batch.isPending} onClick={() => batch.mutate('disable')}>
          <PowerOff className="h-3.5 w-3.5" />
          {t('common:disabled')}
        </Button>
        <Button size="sm" variant="outline" onClick={() => setBatchEdit(true)}>
          <SlidersHorizontal className="h-3.5 w-3.5" />
          {t('admin:channelBatchEdit')}
        </Button>
        <Button size="sm" variant="outline" onClick={() => setBatchEgress(true)}>
          <Globe className="h-3.5 w-3.5" />
          {t('admin:egressBatchAction')}
        </Button>
        <Button size="sm" variant="outline" loading={batch.isPending} onClick={() => batch.mutate('clear_cooldown')}>
          <RotateCcw className="h-3.5 w-3.5" />
          {t('admin:channelBatchClearCooldown')}
        </Button>
        <Button
          size="sm"
          variant="destructive"
          onClick={() =>
            confirm({
              title: t('common:confirmDeleteTitle', { name: `${picked.size}` }),
              description: t('common:confirmBatchDelete', { n: picked.size }),
              onConfirm: () => batch.mutate('delete'),
            })
          }
        >
          <Trash2 className="h-3.5 w-3.5" />
          {t('common:delete')}
        </Button>
      </SelectionBar>

      {dialog}
      {drawer !== null && editingChannel !== undefined && (
        <ChannelDrawer
          key={`${drawer.channel.id}:${drawer.focus ?? 'default'}`}
          channel={editingChannel}
          focus={drawer.focus}
          onClose={() => setDrawer(null)}
          onDone={invalidate}
        />
      )}
      {trying !== null && <ChannelTryDrawer channel={trying} onClose={() => setTrying(null)} />}
      {verifying !== null && <ChannelVerifyDrawer channel={verifying} onClose={() => setVerifying(null)} />}
      {diagnosing && <RouteDiagnosisDrawer onClose={() => setDiagnosing(false)} />}
      {batchEdit && (
        <BatchChannelsDrawer
          ids={[...picked]}
          onClose={() => setBatchEdit(false)}
          onDone={() => {
            setPicked(new Set())
            invalidate()
          }}
        />
      )}
      {batchEgress && (
        <BatchEgressDrawer
          ids={[...picked]}
          onClose={() => setBatchEgress(false)}
          onDone={() => {
            setPicked(new Set())
            invalidate()
          }}
        />
      )}
    </div>
  )
}

/// 上游地址只留主机名（完整地址在悬停提示与编辑抽屉里）；写不成地址的原样显示。
function hostOf(base: string): string {
  try {
    return new URL(base).host
  } catch {
    return base
  }
}

const SUBSCRIPTION_PROVIDERS = new Set(['anthropic_max', 'codex'])

/// 模型悬停卡片：以等宽标签展示实际服务的模型名（超过 12 个折成 +N），比逗号串更易扫读。
function ModelPreviewCard({ models, title, hint }: { models: string[]; title: string; hint: string }) {
  if (models.length === 0) return <span>{hint}</span>
  const shown = models.slice(0, 12)
  const extra = models.length - shown.length
  return (
    <div className="max-w-72 space-y-2 text-left">
      <div className="border-b border-border/60 pb-1.5 text-[11px] font-medium text-muted-foreground">
        {title}
      </div>
      <div className="flex flex-wrap gap-1">
        {shown.map((m, idx) => (
          <code
            key={m}
            className="rounded bg-muted/85 px-1.5 py-0.5 font-mono text-[11px] text-foreground/90 ring-1 ring-inset ring-border/60"
          >
            {m}
            {idx < shown.length - 1 && <span className="sr-only">, </span>}
          </code>
        ))}
        {extra > 0 && (
          <span className="inline-flex items-center rounded bg-muted/55 px-1.5 py-0.5 text-[11px] font-medium text-muted-foreground tabular-nums">
            +{extra}
          </span>
        )}
      </div>
      <div className="border-t border-border/55 pt-1.5 text-[11px] text-muted-foreground/85">{hint}</div>
    </div>
  )
}

function PoolsPreviewCard({ pools, title, hint }: { pools: string[]; title: string; hint: string }) {
  return (
    <div className="max-w-64 space-y-2 text-left">
      <div className="flex items-center justify-between gap-3 border-b border-border/60 pb-1.5 text-[11px] font-medium text-muted-foreground">
        <span>{title}</span>
        <span className="tabular-nums">{pools.length}</span>
      </div>
      <div className="flex flex-wrap gap-1">
        {pools.map((p) => (
          <code
            key={p}
            className="rounded bg-muted/85 px-1.5 py-0.5 font-mono text-[11px] text-foreground/90 ring-1 ring-inset ring-border/60"
          >
            {p}
          </code>
        ))}
      </div>
      <div className="border-t border-border/55 pt-1.5 text-[11px] text-muted-foreground/85">{hint}</div>
    </div>
  )
}

/// 渠道身份与路由摘要块（所见即可点）：
/// 第一行：名称（点击编辑接入）+ #ID + 平台/档位胶囊 + 供应商控制台直达；
/// 第二行：账号或主机名 + 出口代理（点击直达出口配置）+ 所属池（点击直达池配置）+ 模型数（悬停见模型表、点击直达模型编辑）+ 优先级/成本（点击直达调度）+ 多 key 数。
function ChannelIdentity({
  channel: c,
  egress,
  onEdit,
}: {
  channel: ChannelRow
  egress?: { label: string; title: string; mono: boolean }
  onEdit: (focus?: ChannelEditFocus) => void
}) {
  const { t } = useTranslation()
  const plans = [...new Set(c.keys.map((k) => k.account_plan).filter((v): v is string => Boolean(v)))]
  const accounts = [...new Set(c.keys.map((k) => k.account_label).filter((v): v is string => Boolean(v)))]
  const extra = accounts.filter((label) => label !== c.name)
  const consoleUrl = providerConsoleUrl(c.provider, c.api_base)
  const host = !SUBSCRIPTION_PROVIDERS.has(c.provider) && c.api_base ? hostOf(c.api_base) : null
  const enabled = c.status === 1
  const pools = c.pools ?? []
  const hasConnectionMeta = extra.length > 0 || host !== null || egress !== undefined

  return (
    <div className="flex min-w-0 flex-1 items-start gap-3">
      <ProviderMark
        provider={c.provider}
        size="md"
        className={cn(enabled ? 'group-hover/row:scale-[1.03]' : 'opacity-45 grayscale')}
      />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        {/* 第一行：主标题（点击直达编辑）+ 编号 + 平台档位 + 控制台外链 */}
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <button
            type="button"
            data-slot="channel-name"
            onClick={() => onEdit('conn')}
            className={cn(
              'max-w-full cursor-pointer truncate rounded-sm text-left text-[15px] leading-5 font-semibold tracking-[-0.012em] underline-offset-2 outline-none transition-colors hover:text-primary hover:underline focus-visible:ring-2 focus-visible:ring-primary/40',
              !enabled && 'text-muted-foreground',
            )}
            title={shownName(c)}
          >
            {shownName(c)}
          </button>
          <span className="shrink-0 font-mono text-[11px] font-normal text-muted-foreground/55 tabular-nums">
            #{c.id}
          </span>
          {c.pinned && (
            <span
              data-slot="channel-pinned-badge"
              className="inline-flex h-[18px] shrink-0 items-center gap-0.5 rounded bg-primary/10 px-1.5 text-[10.5px] font-medium text-primary ring-1 ring-inset ring-primary/20"
            >
              <Pin aria-hidden className="h-2.5 w-2.5 fill-current" />
              {t('admin:channelPinned')}
            </span>
          )}
          <ProviderChip provider={c.provider} plans={plans} compact className={cn(!enabled && 'opacity-75')} />
          {consoleUrl !== null && (
            <a
              href={consoleUrl}
              target="_blank"
              rel="noreferrer noopener"
              className="-my-1 shrink-0 rounded-md p-1 text-muted-foreground opacity-0 transition-opacity group-hover/row:opacity-100 hover:bg-muted hover:text-foreground focus-visible:opacity-100 [@media(hover:none)]:opacity-100"
              title={t('admin:providerConsole')}
              aria-label={t('admin:providerConsole')}
            >
              <ExternalLink className="h-3.5 w-3.5" />
            </a>
          )}
        </div>

        {/* 第二行：账号/主机 · 出口 · 所属池 · 模型数 · 优先级 · 非 1.0 相对成本 · 多 key 数 */}
        <div
          data-slot="channel-meta"
          className={cn(
            'flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-xs leading-5 text-muted-foreground',
            !enabled && 'opacity-80',
          )}
        >
          {extra.length > 0 ? (
            <span className="inline-flex min-w-0 items-center gap-1">
              <span className="sr-only">{t('admin:channelAccount')}</span>
              <span
                data-slot="channel-account"
                className="max-w-[14rem] truncate text-foreground/80"
                title={accounts.map(maskAccount).join('\n')}
              >
                {maskAccount(extra[0])}
              </span>
              {extra.length > 1 && <span className="shrink-0 text-[11px]">+{extra.length - 1}</span>}
            </span>
          ) : (
            host !== null && (
              <span
                data-slot="channel-host"
                className="max-w-[14rem] truncate font-mono text-[11.5px] text-muted-foreground/90"
                title={c.api_base ?? undefined}
              >
                {host}
              </span>
            )
          )}

          {egress !== undefined && (
            <button
              type="button"
              data-slot="channel-egress"
              onClick={() => onEdit('egress')}
              className="-mx-1 inline-flex min-w-0 cursor-pointer items-center gap-1 rounded px-1 outline-none transition-colors hover:bg-accent/80 hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40"
              aria-label={`${t('admin:channelStatusEgress')}: ${egress.label} · ${t('admin:channelClickEgress')}`}
            >
              <span className="inline-flex shrink-0 text-muted-foreground/70" title={t('admin:channelStatusEgress')}>
                <Globe aria-hidden className="h-3 w-3" strokeWidth={1.8} />
                <span className="sr-only">{t('admin:channelStatusEgress')}</span>
              </span>
              <span className={cn('max-w-[11rem] truncate text-[11.5px]', egress.mono && 'tabular-nums')} title={egress.title}>
                {egress.label}
              </span>
            </button>
          )}

          {hasConnectionMeta && (
            <span aria-hidden className="hidden h-3 w-px bg-border/80 sm:inline-block" />
          )}

          <span
            data-slot="channel-schedule"
            className="inline-flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1"
          >
            <span className="sr-only">{t('admin:channelColSchedule')}</span>
            {pools.length > 0 && (
              <Tooltip content={<PoolsPreviewCard pools={pools} title={t('admin:poolsTitle')} hint={t('admin:channelClickPools')} />}>
                <button
                  type="button"
                  onClick={() => onEdit('pools')}
                  className="inline-flex h-[19px] max-w-[9rem] shrink-0 cursor-pointer items-center truncate rounded-md bg-muted px-1.5 font-mono text-[11px] font-medium text-foreground/85 ring-1 ring-inset ring-border/75 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 dark:bg-white/[0.08]"
                >
                  {pools.length <= 2 ? pools.join(', ') : `${pools[0]} +${pools.length - 1}`}
                </button>
              </Tooltip>
            )}
            <Tooltip
              content={
                <ModelPreviewCard
                  models={c.models}
                  title={t('admin:channelModelCount', { n: c.models.length })}
                  hint={t('admin:channelClickModels')}
                />
              }
            >
              <button
                type="button"
                onClick={() => onEdit('models')}
                className="-mx-1 shrink-0 cursor-pointer rounded px-1 font-medium whitespace-nowrap text-foreground/80 outline-none transition-colors hover:bg-accent/80 hover:text-primary focus-visible:ring-2 focus-visible:ring-primary/40"
              >
                {t('admin:channelModelCount', { n: c.models.length })}
              </button>
            </Tooltip>
            <Tooltip content={t('admin:channelClickSchedule')}>
              <button
                type="button"
                onClick={() => onEdit('schedule')}
                className="-mx-1 inline-flex shrink-0 cursor-pointer items-center gap-1.5 rounded px-1 whitespace-nowrap tabular-nums outline-none transition-colors hover:bg-accent/80 hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40"
              >
                <span>
                  {t('admin:priority')}{' '}
                  <span className={cn(c.priority > 0 ? 'font-semibold text-foreground' : 'font-medium text-foreground/75')}>{c.priority}</span>
                </span>
                {c.cost_milli !== 1000 && (
                  <span className="rounded bg-primary/10 px-1 py-px font-mono text-[11px] font-semibold text-primary ring-1 ring-inset ring-primary/20">
                    ×{costMilliToRatio(c.cost_milli)}
                  </span>
                )}
              </button>
            </Tooltip>
            {c.keys.length > 1 && (
              <Tooltip content={t('admin:keyStateAllActive', { n: c.keys.length })}>
                <button
                  type="button"
                  onClick={() => onEdit('keys')}
                  className="inline-flex h-[19px] shrink-0 cursor-pointer items-center gap-1 rounded-md bg-muted/75 px-1.5 text-[11px] font-medium text-foreground/80 tabular-nums ring-1 ring-inset ring-border/60 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 dark:bg-white/[0.06]"
                >
                  <KeyRound aria-hidden className="h-3 w-3 opacity-75" />
                  {c.keys.length}
                </button>
              </Tooltip>
            )}
          </span>
        </div>
      </div>
    </div>
  )
}
