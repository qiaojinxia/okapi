import {
  Activity,
  Copy,
  Ellipsis,
  ExternalLink,
  Gauge,
  Globe,
  KeyRound,
  MessageSquareText,
  Pencil,
  Plus,
  Power,
  PowerOff,
  Server,
  Trash2,
  Wallet,
} from 'lucide-react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelBalance, ChannelRow } from '@/features/channels/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ActionGroup, ICON_BUTTON_CLASS, IconButton } from '@/components/ui/icon-button'
import { ChannelQuotaCell } from '@/features/channels/account-controls/ChannelQuotaCell'
import { ChannelDrawer } from '@/features/channels/ChannelDrawer'
import { ChannelTryDrawer } from '@/features/channels/ChannelTryDrawer'
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
/// 采用「双层结构化渠道卡片行」：
/// - 上层（身份与操作）：勾选框 + 品牌徽章 + 名称 / 协议档位 / 账号或主机 / 出口 / 路由信息（池、模型数、优先级、成本系数、多 key），
///   右侧是实时健康胶囊、最近测试、调度开关与快捷动作组；若存在冷却或异常 key，下方直接展开诊断告警条。
/// - 下层（遥测仪表盘）：三栏定比网格（额度与余额 / 近 24h 健康与 TTFT / 近 30 天消费与流量），
///   每个额度窗口独占一行满宽展开，各卡片同一列严格纵向对齐，窄屏自动流式堆叠、告别横向滚动。
export function ChannelsPage() {
  const { i18n, t } = useTranslation()
  const queryClient = useQueryClient()
  const [picked, setPicked] = useState<Set<number>>(new Set())
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const query = search.q ?? ''
  const providerFilter = search.provider ?? ''
  const statusFilter = search.status ?? ''
  const [draft, setDraft] = useDraft(query)
  const [drawer, setDrawer] = useState<
    { mode: 'create' } | { mode: 'edit'; channel: ChannelRow; focus?: 'limits' } | null
  >(null)
  const [diagnosing, setDiagnosing] = useState(false)
  const [trying, setTrying] = useState<ChannelRow | null>(null)
  const [batchEgress, setBatchEgress] = useState(false)
  const proxies = useQuery(proxyOptions())
  const proxyGroups = useQuery(proxyGroupOptions())
  const [testingId, setTestingId] = useState<number | null>(null)
  const [balancingId, setBalancingId] = useState<number | null>(null)
  const { confirm, dialog } = useConfirm()
  const pager = usePagination()

  const setFilters = (next: { q?: string; provider?: string; status?: string }) =>
    void navigate({
      search: (prev) => ({
        ...prev,
        ...('q' in next ? { q: text(next.q) } : {}),
        ...('provider' in next ? { provider: oneOf(next.provider, PROVIDERS) } : {}),
        ...('status' in next ? { status: oneOf(next.status, CHANNEL_STATUS_FILTERS) } : {}),
        page: undefined,
      }),
    })

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
    mutationFn: (action: 'enable' | 'disable' | 'delete') =>
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
  const allPicked = rows.length > 0 && rows.every((c) => picked.has(c.id))
  const somePicked = rows.some((c) => picked.has(c.id))
  const filtered = query !== '' || providerFilter !== '' || statusFilter !== ''
  const applySearch = () => setFilters({ q: draft })
  const clearFilters = () => setFilters({ q: '', provider: '', status: '' })
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
    drawer?.mode === 'edit'
      ? (rows.find((c) => c.id === drawer.channel.id) ?? drawer.channel)
      : undefined

  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:channelsTitle')}
        description={t('admin:channelsDesc')}
        meta={channels.data && t('admin:channelsSummary', { total, enabled: enabledCount })}
        action={
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
            <Button onClick={() => setDrawer({ mode: 'create' })}>
              <Plus aria-hidden className="h-4 w-4" />
              {t('admin:createChannel')}
            </Button>
          </>
        }
      />

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
              <Button onClick={() => setDrawer({ mode: 'create' })}>
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
            <table
              aria-label={t('admin:channelsTitle')}
              className="-mt-2 w-full border-separate border-spacing-x-0 border-spacing-y-2"
            >
              <thead>
                <tr>
                  <th className="sticky top-0 z-10 bg-background/95 p-0 text-left font-normal backdrop-blur-sm">
                    <div className="flex items-center justify-between gap-3 px-4 py-1 sm:px-5">
                      <Checkbox
                        checked={allPicked}
                        indeterminate={somePicked && !allPicked}
                        label={t('admin:batchPickAll')}
                        className="text-muted-foreground [&_span.text-sm]:text-xs [&_span.text-sm]:font-medium [&_span.text-sm]:text-muted-foreground"
                        onChange={(on) =>
                          setPicked((prev) => {
                            const next = new Set(prev)
                            for (const c of rows) {
                              if (on) next.add(c.id)
                              else next.delete(c.id)
                            }
                            return next
                          })
                        }
                      />
                    </div>
                  </th>
                </tr>
              </thead>
              <tbody>
                {rows.map((c) => {
                  const enabled = c.status === 1
                  const selected = picked.has(c.id)
                  const pools = c.pools ?? []
                  const orphan = enabled && pools.length === 0
                  const rowStat = health.data?.data.find((s) => s.channel_id === c.id)
                  const rowUsage = usage.data?.data.find((u) => u.channel_id === c.id)
                  const rowHealth = channelRowHealth(c.keys, enabled, orphan, rowStat)
                  const hasKeyAlert = enabled && c.keys.length > 0 && c.keys.some((k) => k.status !== 1)
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
                                  : enabled
                                    ? 'border-border/80 hover:border-border hover:shadow-[0_4px_14px_-4px_oklch(0.24_0.015_70/0.09),0_1px_2px_oklch(0.24_0.015_70/0.04)] dark:hover:shadow-[inset_0_1px_0_oklch(1_0_0/0.07),0_8px_22px_-6px_oklch(0_0_0/0.55)]'
                                    : 'border-border/65 bg-card/70 hover:border-border/90 hover:bg-card/90',
                          )}
                        >
                          {/* 左侧 3.5px 语义状态条：异常与选中常亮，健康态悬停轻亮 */}
                          <span
                            aria-hidden
                            className={cn(
                              'pointer-events-none absolute inset-y-0 left-0 w-[3.5px] transition-colors',
                              selected
                                ? 'bg-primary'
                                : rowHealth === 'destructive'
                                  ? 'bg-destructive'
                                  : rowHealth === 'warning'
                                    ? 'bg-warning'
                                    : enabled
                                      ? 'bg-transparent group-hover/row:bg-primary/45'
                                      : 'bg-transparent',
                            )}
                          />

                          {/* 上层：身份与路由 + 实时状态与快捷控制 */}
                          <div className="px-4 pt-3 pb-2.5 sm:px-5">
                            <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-2.5">
                              {/* 左侧：勾选 + 品牌徽章 + 名称 / 协议档位 / 出口与路由 */}
                              <div className="flex min-w-0 flex-1 items-start gap-3">
                                <div className="flex h-10 items-center">
                                  <Checkbox
                                    srLabel={t('admin:batchPick', { name: shownName(c) })}
                                    checked={selected}
                                    onChange={() => togglePick(c.id)}
                                  />
                                </div>
                                <ChannelIdentity channel={c} egress={egress} />
                              </div>

                              {/* 右侧：状态胶囊 + 未入池 + 最近测活 | 调度开关 + 动作组 */}
                              <div className="flex w-full shrink-0 flex-wrap items-center justify-between gap-x-3 gap-y-2 pl-7 sm:w-auto sm:justify-end sm:pl-2">
                                <div data-slot="channel-status" className="flex items-center gap-2">
                                  <KeyStateSummary keys={c.keys} enabled={enabled} />
                                  {orphan && (
                                    <Badge
                                      variant="destructive"
                                      className="min-h-[22px] shrink-0 rounded-md px-2 py-0 text-[11px]"
                                      title={t('admin:poolOrphanWarning')}
                                    >
                                      {t('admin:channelOrphan')}
                                    </Badge>
                                  )}
                                  <LastProbe probe={c.last_test} />
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
                                      icon={Activity}
                                      label={t('admin:testChannel')}
                                      loading={testingId === c.id}
                                      onClick={() => test.mutate(c)}
                                    />
                                    <IconButton
                                      icon={Pencil}
                                      label={t('common:edit')}
                                      onClick={() => setDrawer({ mode: 'edit', channel: c })}
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
                                        onSelect={() => setDrawer({ mode: 'edit', channel: c, focus: 'limits' })}
                                      >
                                        {t('admin:channelLimitsTitle')}
                                      </MenuItem>
                                      <MenuItem icon={Copy} onSelect={() => duplicate.mutate(c)}>
                                        {t('admin:duplicate')}
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

                            {/* 异常凭证诊断条：仅当有冷却 / 限流 / 失效 key 时展开，不打扰健康渠道 */}
                            {hasKeyAlert && (
                              <div className="mt-2 pl-7 sm:pl-[4.75rem]">
                                <KeyStateDetail keys={c.keys} enabled={enabled} />
                              </div>
                            )}
                          </div>

                          {/* 下层遥测仪表盘：三栏定比对齐——额度与余额 / 近 24h 健康与 TTFT / 近 30 天用量 */}
                          <div className="border-t border-border/55 bg-muted/[0.22] px-4 py-2 sm:px-5 dark:bg-white/[0.018]">
                            <div className="grid grid-cols-1 gap-2.5 md:grid-cols-[minmax(0,1.55fr)_minmax(0,1fr)_minmax(11.5rem,0.72fr)] md:items-center md:gap-0 md:divide-x md:divide-border/55">
                              {/* 区域一：额度仪表与上游余额 */}
                              <div
                                data-slot="channel-quota"
                                className="flex min-w-0 flex-col justify-center gap-2 md:pr-5"
                              >
                                <ChannelQuotaCell
                                  channelId={c.id}
                                  provider={c.provider}
                                  control={c.settings?.account_control}
                                  enabled={enabled}
                                  fallback={
                                    c.last_balance ? null : (
                                      <div className="flex items-center gap-2 text-xs text-muted-foreground/65">
                                        <span className="text-[11px]">{t('admin:channelColQuota')}</span>
                                        <span aria-hidden>—</span>
                                      </div>
                                    )
                                  }
                                />
                                <LastBalance balance={c.last_balance} />
                              </div>

                              {/* 区域二：近 24h 健康度与首字延迟 */}
                              <div
                                data-slot="channel-health"
                                className="flex min-w-0 flex-col justify-center border-t border-border/45 pt-2.5 md:border-t-0 md:px-5 md:pt-0"
                              >
                                <Health24h
                                  stat={rowStat}
                                  channel={{ id: c.id, name: c.name, provider: c.provider }}
                                />
                              </div>

                              {/* 区域三：近 30 天花费与流量 */}
                              <div className="flex min-w-0 flex-col justify-center border-t border-border/45 pt-2.5 md:border-t-0 md:pl-5 md:pt-0">
                                <ChannelUsage usage={rowUsage} settled={usage.data !== undefined} />
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
          </div>
        </div>
      )}

      <Pagination {...pager} total={channels.data?.total} />

      <SelectionBar count={picked.size} onClear={() => setPicked(new Set())}>
        <Button size="sm" variant="outline" loading={batch.isPending} onClick={() => batch.mutate('enable')}>
          <Power className="h-3.5 w-3.5" />
          {t('common:enabled')}
        </Button>
        <Button size="sm" variant="outline" loading={batch.isPending} onClick={() => batch.mutate('disable')}>
          <PowerOff className="h-3.5 w-3.5" />
          {t('common:disabled')}
        </Button>
        <Button size="sm" variant="outline" onClick={() => setBatchEgress(true)}>
          <Globe className="h-3.5 w-3.5" />
          {t('admin:egressBatchAction')}
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
      {drawer !== null && (
        <ChannelDrawer
          channel={editingChannel}
          focus={drawer.mode === 'edit' ? drawer.focus : undefined}
          onClose={() => setDrawer(null)}
          onDone={invalidate}
        />
      )}
      {trying !== null && <ChannelTryDrawer channel={trying} onClose={() => setTrying(null)} />}
      {diagnosing && <RouteDiagnosisDrawer onClose={() => setDiagnosing(false)} />}
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

/// 渠道身份与路由摘要块：
/// 第一行是名称 + #ID + 平台/档位胶囊 + 供应商控制台直达；
/// 第二行是账号或主机名 + 显式出口 + 调度路由信息（所属池、模型数、优先级、非默认成本系数、多 key 计数）。
function ChannelIdentity({
  channel: c,
  egress,
}: {
  channel: ChannelRow
  egress?: { label: string; title: string; mono: boolean }
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
        {/* 第一行：主标题 + 编号 + 平台档位 + 控制台外链 */}
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <span
            data-slot="channel-name"
            className={cn(
              'max-w-full truncate text-[15px] leading-5 font-semibold tracking-[-0.012em]',
              !enabled && 'text-muted-foreground',
            )}
            title={shownName(c)}
          >
            {shownName(c)}
          </span>
          <span className="shrink-0 font-mono text-[11px] font-normal text-muted-foreground/55 tabular-nums">
            #{c.id}
          </span>
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
            <span data-slot="channel-egress" className="inline-flex min-w-0 items-center gap-1">
              <span className="inline-flex shrink-0 text-muted-foreground/70" title={t('admin:channelStatusEgress')}>
                <Globe aria-hidden className="h-3 w-3" strokeWidth={1.8} />
                <span className="sr-only">{t('admin:channelStatusEgress')}</span>
              </span>
              <span className={cn('max-w-[11rem] truncate text-[11.5px]', egress.mono && 'tabular-nums')} title={egress.title}>
                {egress.label}
              </span>
            </span>
          )}

          {hasConnectionMeta && (
            <span aria-hidden className="hidden text-border sm:inline">·</span>
          )}

          <span
            data-slot="channel-schedule"
            className="inline-flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1"
          >
            <span className="sr-only">{t('admin:channelColSchedule')}</span>
            {pools.length > 0 && (
              <span
                className="inline-flex h-[18px] max-w-[9rem] shrink-0 items-center truncate rounded bg-muted/85 px-1.5 font-mono text-[10.5px] font-medium text-muted-foreground ring-1 ring-inset ring-border/60 dark:bg-white/[0.06]"
                title={pools.join(', ')}
              >
                {pools.length <= 2 ? pools.join(', ') : `${pools[0]} +${pools.length - 1}`}
              </span>
            )}
            <span
              className="shrink-0 whitespace-nowrap"
              title={c.models.length > 0 ? c.models.join(', ') : undefined}
            >
              {t('admin:channelModelCount', { n: c.models.length })}
            </span>
            <span className="shrink-0 whitespace-nowrap tabular-nums">
              {t('admin:priority')}{' '}
              <span className={cn(c.priority > 0 && 'font-medium text-foreground/85')}>{c.priority}</span>
            </span>
            {c.cost_milli !== 1000 && (
              <span
                className="shrink-0 font-mono text-[11px] font-medium text-primary tabular-nums"
                title={t('admin:channelCostMilli')}
              >
                ×{costMilliToRatio(c.cost_milli)}
              </span>
            )}
            {c.keys.length > 1 && (
              <span
                className="inline-flex shrink-0 items-center gap-0.5 text-[11px] text-muted-foreground/90 tabular-nums"
                title={t('admin:keyStateAllActive', { n: c.keys.length })}
              >
                <KeyRound aria-hidden className="h-3 w-3 opacity-75" />
                {c.keys.length}
              </span>
            )}
          </span>
        </div>
      </div>
    </div>
  )
}
