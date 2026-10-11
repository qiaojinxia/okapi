import {
  Activity,
  Copy,
  ExternalLink,
  Globe,
  Pencil,
  Plus,
  Power,
  PowerOff,
  Server,
  Stethoscope,
  Trash2,
  UserRound,
  Wallet,
} from 'lucide-react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelBalance, ChannelRow } from '@/features/channels/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ChannelQuotaCell } from '@/features/channels/account-controls/ChannelQuotaCell'
import { ChannelDrawer } from '@/features/channels/ChannelDrawer'
import {
  ChannelUsage,
  Health24h,
  KeyStateSummary,
  LastBalance,
  LastProbe,
  useChannelHealth24h,
  useChannelUsage,
} from '@/features/channels/ChannelHealthCell'
import { RouteDiagnosisDrawer } from '@/features/channels/RouteDiagnosis'
import { BatchEgressDrawer } from '@/features/channels/ChannelEgress'
import { describeEgress } from '@/features/proxies/EgressPicker'
import { proxyGroupOptions, proxyOptions } from '@/features/proxies/options'
import { Checkbox } from '@/components/ui/checkbox'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { PROVIDERS, balanceSupported, providerConsoleUrl } from '@/features/channels/types'
import { PageHeader, Toolbar, ToolbarSearch } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { SearchInput } from '@/components/ui/search-input'
import { Select } from '@/components/ui/select'
import { SelectionBar } from '@/components/ui/selection-bar'
import { TableSkeleton } from '@/components/ui/skeleton'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { useDraft } from '@/hooks/use-draft'
import { usePagination } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatUpstreamBalance } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { oneOf, text } from '@/lib/search-params'
import { useConfirm } from '@/components/ui/confirm'

const routeApi = getRouteApi('/admin/channels')

/// 列表接口的一页：`total` / `enabled` 都是过滤集内的计数（未筛选时即全站）。
interface ChannelPage {
  data: ChannelRow[]
  total: number
  enabled: number
}

/// 渠道列表页。
///
/// 只负责"找到渠道并对其批量处理"：新建与编辑都在抽屉里完成，页面本身不放表单。
/// 此前建表单、列表、展开式编辑器堆在同一屏，用户要滚过整片表单才看到数据。
///
/// 操作反馈全部走 toast：测活结果、批量结果、失败原因此前都是工具栏下一行 12px 灰字，
/// 测活成功与失败长得一样，看完也不会消失。
export function ChannelsPage() {
  const { i18n, t } = useTranslation()
  const queryClient = useQueryClient()
  const [picked, setPicked] = useState<Set<number>>(new Set())
  // 关键词 / 协议筛选 / 页码都在地址里（刷新 / 分享 / 后退不丢）。
  // 搜索：草稿 → 回车 / 点搜索才提交（服务端 ILIKE，与用户 / 令牌列表同一形态）
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const query = search.q ?? ''
  const providerFilter = search.provider ?? ''
  const [draft, setDraft] = useDraft(query)
  const [drawer, setDrawer] = useState<
    { mode: 'create' } | { mode: 'edit'; channel: ChannelRow } | null
  >(null)
  const [diagnosing, setDiagnosing] = useState(false)
  const [batchEgress, setBatchEgress] = useState(false)
  // 出口徽章要代理 / 组的名字（§11.41）；目录整表缓存，列表翻页不重拉
  const proxies = useQuery(proxyOptions())
  const proxyGroups = useQuery(proxyGroupOptions())
  const [testingId, setTestingId] = useState<number | null>(null)
  const [balancingId, setBalancingId] = useState<number | null>(null)
  const { confirm, dialog } = useConfirm()
  const pager = usePagination()
  // 过滤器与页码同一次导航更新：只按"新条件 + 第一页"请求一次
  const setFilters = (next: { q?: string; provider?: string }) =>
    void navigate({
      search: (prev) => ({
        ...prev,
        ...('q' in next ? { q: text(next.q) } : {}),
        ...('provider' in next ? { provider: oneOf(next.provider, PROVIDERS) } : {}),
        page: undefined,
      }),
    })

  // 近 24h 健康：一次整表查询按 channel_id 分发到各行（CH 未启用则各行显示 —）
  const health = useChannelHealth24h()
  const usage = useChannelUsage()
  // 过滤与切片都在服务端：几百条渠道每条带 keys / pools，整表拉回来再筛既慢又占内存。
  // 列表接口不传 limit 才回全量——那是给"测活全部"和模型页的渠道计数用的。
  const channels = useQuery({
    queryKey: [...qk.adminChannels, query, providerFilter, pager.offset, pager.limit],
    queryFn: () => {
      const params = new URLSearchParams({
        limit: String(pager.limit),
        offset: String(pager.offset),
      })
      if (query !== '') params.set('q', query)
      if (providerFilter !== '') params.set('provider', providerFilter)
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
      // 单条测活直接验第一个模型：只探 /models 只能说明"凭证认、网络通"，
      // 聚合型上游按套餐授权模型，那种 ok 会对一个实际 403 的模型报成功
      return probeOne(c.id, c.models[0])
    },
    onSuccess: (r, c) => {
      if (r.ok) toast.success(c.name, t('admin:testOk', { ms: r.latency_ms ?? 0 }))
      else if (r.scope === 'model')
        toast.error(
          c.name,
          t('admin:testModelFail', {
            model: c.models[0] ?? '',
            code: r.error_code ?? String(r.http_status ?? ''),
            detail: (r.upstream_body ?? '').slice(0, 120),
          }),
        )
      else toast.error(c.name, t('admin:testFail', { code: r.error_code ?? 'unknown' }))
      // 结果已在服务端留痕，刷新列表让"最近测试"列跟上
      invalidate()
    },
    onError: fail,
    onSettled: () => setTestingId(null),
  })

  // 上游余额（§11.33）：结果留痕在服务端，刷新列表让"余额"回填跟上
  const balance = useMutation({
    mutationFn: (c: ChannelRow) => {
      setBalancingId(c.id)
      return apiFetch<ChannelBalance>(`/admin/channels/${c.id}/balance`)
    },
    onSuccess: (r, c) => {
      toast.success(
        c.name,
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

  // 测试全部启用渠道（new-api 同有）：并发 3 路——太多会让一批上游同时看到探测，
  // 太少几十条渠道要等很久；逐条失败不中断，最后汇总成功/失败数
  const testAll = useMutation({
    mutationFn: async () => {
      // 测的是全站所有启用渠道，不受当前页 / 筛选影响：不传 limit 拿整表
      const { data } = await apiFetch<ChannelPage>('/admin/channels?status=1')
      const ids = data.map((c) => c.id)
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
  // 过滤集内的启用数（未筛选时即全站）：页头徽章据此显示
  const enabledCount = channels.data?.enabled ?? 0
  // 表头勾选只管本页；别页勾下的仍留在 picked 里，底部 SelectionBar 的计数会把它们算上
  const allPicked = rows.length > 0 && rows.every((c) => picked.has(c.id))
  const somePicked = rows.some((c) => picked.has(c.id))
  const filtered = query !== '' || providerFilter !== ''
  const applySearch = () => setFilters({ q: draft })
  const clearFilters = () => setFilters({ q: '', provider: '' })

  const togglePick = (id: number) =>
    setPicked((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  // 编辑态优先取列表里的最新行（加 key / 改池后 invalidate 会刷新它）；改了优先级或名字后
  // 这一行可能翻到别页或被搜索词筛掉，此时退回打开抽屉时的快照——find 落空会让抽屉当场
  // 变成一张空白的"新建"表单。
  const editingChannel =
    drawer?.mode === 'edit'
      ? (rows.find((c) => c.id === drawer.channel.id) ?? drawer.channel)
      : undefined

  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:channelsTitle')}
        description={t('admin:channelsDesc')}
        icon={Server}
        meta={
          channels.data && (
            <Badge variant="muted">
              {t('admin:channelsSummary', { total, enabled: enabledCount })}
            </Badge>
          )
        }
        action={
          <>
            <Button
              variant="outline"
              loading={testAll.isPending}
              // 筛选中的"0 启用"不代表全站没有可测的
              disabled={enabledCount === 0 && !filtered}
              onClick={() => testAll.mutate()}
            >
              {!testAll.isPending && <Activity className="h-4 w-4" />}
              {testAll.isPending ? t('admin:testAllRunning') : t('admin:testAll')}
            </Button>
            <Button variant="outline" onClick={() => setDiagnosing(true)}>
              <Stethoscope className="h-4 w-4" />
              {t('admin:diagTitle')}
            </Button>
            <Button onClick={() => setDrawer({ mode: 'create' })}>
              <Plus className="h-4 w-4" />
              {t('admin:createChannel')}
            </Button>
          </>
        }
      />

      <Toolbar
        filters={
          <>
            <ToolbarSearch>
              <SearchInput
                id="ch-search"
                className="min-w-0 flex-1"
                aria-label={t('admin:channelSearchHint')}
                value={draft}
                placeholder={t('admin:channelSearchHint')}
                onChange={setDraft}
                onSubmit={applySearch}
              />
              <Button size="sm" variant="outline" onClick={applySearch}>
                {t('common:search')}
              </Button>
            </ToolbarSearch>
            <Select
              id="ch-provider"
              className="w-40"
              aria-label={t('admin:provider')}
              value={providerFilter}
              onChange={(provider) => setFilters({ provider })}
              placeholder={t('common:all')}
              options={PROVIDERS.map((p) => ({ value: p, label: p }))}
            />
            {filtered && (
              <Button size="sm" variant="ghost" onClick={clearFilters}>
                {t('common:clearFilters')}
              </Button>
            )}
          </>
        }
        selection={
          <Badge variant="muted" className="tabular-nums">
            {t('common:resultCount', { n: total })}
          </Badge>
        }
      />

      {channels.isError ? (
        <ErrorState message={describeError(channels.error)} onRetry={() => void channels.refetch()} />
      ) : channels.isPending ? (
        <TableSkeleton rows={8} cols={9} />
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
        <Table stickyHeader>
          <THead>
            <Tr>
              <Th className="w-10">
                <Checkbox
                  srLabel={t('admin:batchPickAll')}
                  checked={allPicked}
                  indeterminate={somePicked && !allPicked}
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
              </Th>
              <Th>{t('admin:channelName')}</Th>
              <Th>{t('admin:provider')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:healthCol')}</Th>
              <Th className="text-right">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((c) => (
              <Tr key={c.id} selected={picked.has(c.id)}>
                <Td>
                  <Checkbox
                    srLabel={t('admin:batchPick', { name: c.name })}
                    checked={picked.has(c.id)}
                    onChange={() => togglePick(c.id)}
                  />
                </Td>
                <Td>
                  <div className="flex flex-col">
                    {/* 名字与地址同宽截断（hover 见全名）：长名字不该决定整表宽度；ID 并进名字行，省一列 */}
                    <span className="flex max-w-40 min-[1400px]:max-w-48 items-baseline gap-1.5">
                      <span className="shrink-0 text-xs text-muted-foreground tabular-nums">#{c.id}</span>
                      <span className="truncate font-medium" title={c.name}>{c.name}</span>
                    </span>
                    <span className="max-w-40 min-[1400px]:max-w-48 truncate font-mono text-xs text-muted-foreground" title={c.api_base ?? undefined}>
                      {c.api_base ?? '—'}
                    </span>
                    <ChannelAccounts keys={c.keys} />
                  </div>
                </Td>
                {/* 协议列顺带模型数与优先级：两个短数字各占一列太浪费横向空间 */}
                <Td>
                  <div className="flex flex-col items-start gap-1">
                    <span className="inline-flex items-center gap-1">
                      <Badge variant="outline" className="font-mono">
                        {c.provider}
                      </Badge>
                      {/* 供应商控制台直达（new-api #7146）：查上游余额/状态时不必再去搜网址 */}
                      {providerConsoleUrl(c.provider, c.api_base) !== null && (
                        <a
                          href={providerConsoleUrl(c.provider, c.api_base) ?? undefined}
                          target="_blank"
                          rel="noreferrer noopener"
                          className="rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
                          title={t('admin:providerConsole')}
                          aria-label={t('admin:providerConsole')}
                        >
                          <ExternalLink className="h-3.5 w-3.5" />
                        </a>
                      )}
                    </span>
                    <span className="text-xs text-muted-foreground tabular-nums">
                      {t('admin:channelModelsPriority', { n: c.models.length, priority: c.priority })}
                    </span>
                  </div>
                </Td>
                {/* 渠道"启用"≠ 能打：状态列 = 渠道开关 + key 状态机汇总，近 24h 错误率另列 */}
                <Td>
                  <div className="flex flex-wrap items-center gap-1">
                    <KeyStateSummary keys={c.keys} enabled={c.status === 1} />
                    {/* 不在任何池 = 对谁都不可达：渠道开关绿着、key 全可用也打不到，必须在列表就看见 */}
                    {c.status === 1 && (c.pools ?? []).length === 0 && (
                      <Badge variant="destructive" title={t('admin:poolOrphanWarning')}>
                        {t('admin:channelOrphan')}
                      </Badge>
                    )}
                    {/* 显式出口（代理 / 组 / 直连）才标；继承全局默认的不标，免得每行一个徽章 */}
                    {c.egress !== undefined && c.egress.mode !== 'inherit' && (
                      <Badge variant="outline" title={t('admin:egressTitle')}>
                        <Globe className="h-3 w-3" aria-hidden />
                        {describeEgress(t, c.egress, proxies.data, proxyGroups.data)}
                      </Badge>
                    )}
                    {c.status === 1 && <div className="basis-full"><ChannelQuotaCell channelId={c.id} provider={c.provider} /></div>}
                  </div>
                </Td>
                {/* 最近测试与近 24h 同属"能不能打"，合成一列上下排，省出横向空间 */}
                <Td>
                  <div className="flex flex-col items-start gap-1">
                    <LastProbe probe={c.last_test} />
                    <Health24h
                      stat={health.data?.data.find((s) => s.channel_id === c.id)}
                      channel={{ id: c.id, name: c.name, provider: c.provider }}
                    />
                    <ChannelUsage usage={usage.data?.data.find((u) => u.channel_id === c.id)} />
                    <LastBalance balance={c.last_balance} />
                  </div>
                </Td>
                <Td>
                  <div className="flex items-center justify-end gap-0.5">
                    <IconButton
                      icon={c.status === 1 ? PowerOff : Power}
                      label={c.status === 1 ? t('common:disabled') : t('common:enabled')}
                      onClick={() => setStatus.mutate({ id: c.id, status: c.status === 1 ? 2 : 1 })}
                    />
                    <IconButton
                      icon={Activity}
                      label={t('admin:testChannel')}
                      loading={testingId === c.id}
                      onClick={() => test.mutate(c)}
                    />
                    {balanceSupported(c.provider) && (
                      <IconButton
                        icon={Wallet}
                        label={t('admin:balanceQuery')}
                        loading={balancingId === c.id}
                        onClick={() => balance.mutate(c)}
                      />
                    )}
                    <IconButton
                      icon={Pencil}
                      label={t('common:edit')}
                      onClick={() => setDrawer({ mode: 'edit', channel: c })}
                    />
                    <IconButton
                      icon={Copy}
                      label={t('admin:duplicate')}
                      onClick={() => duplicate.mutate(c)}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: c.name }),
                          description: t('common:confirmChannelDelete'),
                          requireText: c.name,
                          onConfirm: () => remove.mutate(c.id),
                        })
                      }
                    />
                  </div>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
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
          onClose={() => setDrawer(null)}
          onDone={invalidate}
        />
      )}
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

/// 订阅渠道绑定的账号（邮箱）：多把 key 时显示首个并标出其余数量，悬停看全部。
function ChannelAccounts({ keys }: { keys?: { account_label?: string; account_plan?: string }[] }) {
  const { t } = useTranslation()
  const labels = [...new Set((keys ?? []).map((k) => k.account_label).filter((v): v is string => Boolean(v)))]
  // 档位按 key 去重；多把 key 档位不同时都列出（Pro + Max 20x）
  const plans = [...new Set((keys ?? []).map((k) => k.account_plan).filter((v): v is string => Boolean(v)))]
  if (labels.length === 0 && plans.length === 0) return null
  return (
    <span className="flex max-w-40 min-[1400px]:max-w-48 items-center gap-1 text-xs text-muted-foreground" title={labels.join('\n')}>
      <UserRound aria-label={t('admin:channelAccount')} className="h-3 w-3 shrink-0" />
      {plans.map((plan) => (
        <Badge key={plan} variant="outline" className="shrink-0 px-1 py-0 text-[10px] leading-4" title={t('admin:channelPlanHint')}>
          {planLabel(plan)}
        </Badge>
      ))}
      {labels.length > 0 && <span className="truncate">{labels[0]}</span>}
      {labels.length > 1 && <span className="shrink-0">+{labels.length - 1}</span>}
    </span>
  )
}

const PLAN_LABELS: Record<string, string> = {
  pro: 'Pro',
  max: 'Max',
  max_5x: 'Max 5x',
  max_20x: 'Max 20x',
  team: 'Team',
  enterprise: 'Enterprise',
}

function planLabel(plan: string): string {
  return PLAN_LABELS[plan] ?? plan
}
