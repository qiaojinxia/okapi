import {
  Activity,
  Copy,
  Ellipsis,
  ExternalLink,
  Globe,
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
import { ICON_BUTTON_CLASS, IconButton } from '@/components/ui/icon-button'
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
  StatusLine,
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
import { CHANNEL_STATUS_FILTERS, CHANNEL_STATUS_VALUE, PROVIDERS, balanceSupported, providerConsoleUrl } from '@/features/channels/types'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { SearchInput } from '@/components/ui/search-input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { SelectionBar } from '@/components/ui/selection-bar'
import { TableSkeleton } from '@/components/ui/skeleton'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { useDraft } from '@/hooks/use-draft'
import { usePagination } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
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
  const statusFilter = search.status ?? ''
  const [draft, setDraft] = useDraft(query)
  const [drawer, setDrawer] = useState<
    { mode: 'create' } | { mode: 'edit'; channel: ChannelRow } | null
  >(null)
  const [diagnosing, setDiagnosing] = useState(false)
  const [trying, setTrying] = useState<ChannelRow | null>(null)
  const [batchEgress, setBatchEgress] = useState(false)
  // 行内出口只显示图标与目标名称；目录整表缓存，列表翻页不重拉。
  const proxies = useQuery(proxyOptions())
  const proxyGroups = useQuery(proxyGroupOptions())
  const [testingId, setTestingId] = useState<number | null>(null)
  const [balancingId, setBalancingId] = useState<number | null>(null)
  const { confirm, dialog } = useConfirm()
  const pager = usePagination()
  // 过滤器与页码同一次导航更新：只按"新条件 + 第一页"请求一次
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

  // 近 24h 健康：一次整表查询按 channel_id 分发到各行（CH 未启用则各行显示 —）
  const health = useChannelHealth24h()
  const usage = useChannelUsage()
  // 过滤与切片都在服务端：几百条渠道每条带 keys / pools，整表拉回来再筛既慢又占内存。
  // 列表接口不传 limit 才回全量——那是给"测活全部"和模型页的渠道计数用的。
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
      // 单条测活直接验第一个模型：只探 /models 只能说明"凭证认、网络通"，
      // 聚合型上游按套餐授权模型，那种 ok 会对一个实际 403 的模型报成功
      return probeOne(c.id, c.models[0])
    },
    onSuccess: (r, c) => {
      if (r.ok) toast.success(shownName(c), t('admin:testOk', { ms: r.latency_ms ?? 0 }))
      // 2xx 却是网页：地址缺版本段（/v1）或指到了站点首页，网关已不再把它当成功
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
        className="[&_h1]:text-[22px] [&_h1]:leading-7 [&_h1]:tracking-[-0.025em] [&_p]:max-w-2xl [&_p]:text-[13px]"
        title={t('admin:channelsTitle')}
        description={t('admin:channelsDesc')}
        meta={channels.data && t('admin:channelsSummary', { total, enabled: enabledCount })}
        action={
          <>
            <Button
              variant="outline"
              className="px-3.5"
              loading={testAll.isPending}
              // 筛选中的"0 启用"不代表全站没有可测的
              disabled={enabledCount === 0 && !filtered}
              onClick={() => testAll.mutate()}
            >
              {testAll.isPending ? t('admin:testAllRunning') : t('admin:testAll')}
            </Button>
            <Button variant="outline" className="px-3.5" onClick={() => setDiagnosing(true)}>
              {t('admin:diagTitle')}
            </Button>
            <Button className="gap-1.5 rounded-lg px-4 shadow-[0_5px_14px_-8px_var(--primary)]" onClick={() => setDrawer({ mode: 'create' })}>
              <span aria-hidden>+</span>
              {t('admin:createChannel')}
            </Button>
          </>
        }
      />

      {/* 搜索回车提交；状态是互斥短选项，用分段控件 */}
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
      />

      {channels.isError ? (
        <ErrorState message={describeError(channels.error)} onRetry={() => void channels.refetch()} />
      ) : channels.isPending ? (
        <TableSkeleton rows={8} cols={5} />
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
        <Table stickyHeader wrapperClassName="rounded-[18px] border-border/70 shadow-[0_1px_2px_oklch(0_0_0/0.035),0_18px_40px_-30px_oklch(0_0_0/0.26)]">
          <THead className="bg-muted/30 [&_th]:h-10 [&_th]:bg-muted/30 [&_th]:text-[11px] [&_th]:font-semibold [&_th]:tracking-[0.045em] [&_th]:text-muted-foreground/85">
            <Tr>
              <Th className="w-11 min-w-11 pr-0 pl-5">
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
              {/* 窄屏（表格 < 约 1100px）四列按比例分，谁都不富余；宽屏时状态列定宽（够放「胶囊 + 延迟 + 时间」），
                  用量列吃掉剩下的——额度条是唯一能拉长的内容，长一点比列与列之间留出一大块空白好看。 */}
              <Th className="w-[28%] min-w-60 min-[1420px]:w-[27%]">{t('admin:channelsTitle')}</Th>
              <Th className="w-[29%] min-w-56 min-[1420px]:w-[17.5rem]">{t('common:status')}</Th>
              <Th className="w-[30%] min-w-64 min-[1420px]:w-auto">{t('admin:channelColUsage')}</Th>
              <Th className="w-[9%] min-w-36 pr-5 text-right">{t('admin:channelColActions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((c) => (
              <Tr key={c.id} selected={picked.has(c.id)} className="group/row hover:bg-accent/30 data-[selected]:shadow-[inset_2px_0_0_var(--primary)]">
                <Td className="py-3 pr-0 pl-5 align-top">
                  {/* 与名称首行同高居中：直接放行内框，勾选框会落在基线上、比名字高出两三像素 */}
                  <div className="flex h-5 items-center">
                    <Checkbox
                      srLabel={t('admin:batchPick', { name: shownName(c) })}
                      checked={picked.has(c.id)}
                      onChange={() => togglePick(c.id)}
                    />
                  </div>
                </Td>
                <Td className="py-3 align-top">
                  <ChannelNameCell channel={c} />
                </Td>
                {/* 首行回答“现在能不能打”；下面用图标短行补充近 24h 与显式出口。
                    凭证到期属于编辑详情，不占列表的扫读空间。 */}
                <Td className="py-3 align-top">
                  <dl className="grid grid-cols-[auto_minmax(0,1fr)] items-baseline gap-x-2.5 gap-y-0.5 text-foreground/90">
                    <dt>
                      <KeyStateSummary keys={c.keys} enabled={c.status === 1} />
                    </dt>
                    <dd className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
                      <LastProbe probe={c.last_test} />
                      {/* 不在任何池 = 对谁都不可达：渠道开关绿着、key 全可用也打不到，必须在列表就看见 */}
                      {c.status === 1 && (c.pools ?? []).length === 0 && (
                        <Badge variant="destructive" className="min-h-5 rounded-md px-1.5 py-0 text-[11px]" title={t('admin:poolOrphanWarning')}>
                          {t('admin:channelOrphan')}
                        </Badge>
                      )}
                    </dd>
                    {/* 部分 key 不可用时才有：各态计数 + 恢复时间，自成一行 */}
                    <KeyStateDetail keys={c.keys} enabled={c.status === 1} />
                    <StatusLine label={t('admin:channelStatus24h')} icon={Activity}>
                      <Health24h
                        stat={health.data?.data.find((s) => s.channel_id === c.id)}
                        channel={{ id: c.id, name: c.name, provider: c.provider }}
                      />
                    </StatusLine>
                    {c.egress !== undefined && c.egress.mode !== 'inherit' && (
                      <StatusLine label={t('admin:channelStatusEgress')} icon={Globe} title={describeEgress(t, c.egress, proxies.data, proxyGroups.data)}>
                        <span className={cn((c.egress.mode === 'proxy' || c.egress.mode === 'group') && 'text-xs text-muted-foreground', c.egress.mode === 'proxy' && 'font-mono')}>
                          {egressLabel(c)}
                        </span>
                      </StatusLine>
                    )}
                  </dl>
                </Td>
                {/* 用量窗口：近 30 天用量一行 + 订阅额度窗口 + 上游余额，回答「还剩多少」 */}
                <Td className="py-3 align-top">
                  <div className="flex w-full max-w-[38rem] flex-col items-start gap-2.5">
                    <ChannelUsage usage={usage.data?.data.find((u) => u.channel_id === c.id)} />
                    {c.status === 1 && <ChannelQuotaCell channelId={c.id} provider={c.provider} />}
                    <LastBalance balance={c.last_balance} />
                  </div>
                </Td>
                {/* 调度开关在上，常用动作（测试 / 编辑）在下，其余动作收进「···」菜单 */}
                <Td className="py-3 pr-5 align-top">
                  <div className="flex flex-col items-end gap-3">
                    <Switch
                      hideLabel
                      label={t('admin:channelScheduleToggle', { name: shownName(c) })}
                      checked={c.status === 1}
                      disabled={setStatus.isPending}
                      onChange={(on) => setStatus.mutate({ id: c.id, status: on ? 1 : 2 })}
                    />
                    {/* 图标动作：平时淡一档，悬停整行变深，悬停自己才有底色；名字在悬停提示（便签）与读屏名里。
                        「更多」菜单的触发器不套悬停提示：提示常驻时 Esc 会先关提示，菜单得按两次才关 */}
                    <div className="flex items-center gap-0.5 rounded-lg border border-border/60 bg-background/45 p-0.5 shadow-xs">
                      <IconButton
                        icon={Activity}
                        label={t('admin:testChannel')}
                        loading={testingId === c.id}
                        className={ROW_ICON}
                        onClick={() => test.mutate(c)}
                      />
                      <IconButton
                        icon={Pencil}
                        label={t('common:edit')}
                        className={ROW_ICON}
                        onClick={() => setDrawer({ mode: 'edit', channel: c })}
                      />
                      <Menu
                        strategy="fixed"
                        className="min-w-44"
                        trigger={
                          <Button size="icon" variant="ghost" aria-label={t('admin:channelMoreActions')} className={cn(ICON_BUTTON_CLASS, ROW_ICON)}>
                            <Ellipsis />
                          </Button>
                        }
                      >
                        <MenuItem icon={MessageSquareText} onSelect={() => setTrying(c)}>
                          {t('admin:channelTryAction')}
                        </MenuItem>
                        {balanceSupported(c.provider) && (
                          <MenuItem icon={Wallet} disabled={balancingId === c.id} onSelect={() => balance.mutate(c)}>
                            {t('admin:balanceQuery')}
                          </MenuItem>
                        )}
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
                              // 对话框里显示的是遮过的名字，要输入的也是它：看到什么就输入什么
                              requireText: shownName(c),
                              onConfirm: () => remove.mutate(c.id),
                            })
                          }
                        >
                          {t('common:delete')}
                        </MenuItem>
                      </Menu>
                    </div>
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

/// 行内图标动作的配色：平时比普通图标淡一档，悬停所在行时变深，悬停到自己才有底色。
const ROW_ICON =
  'rounded-lg text-muted-foreground/75 group-hover/row:text-foreground/80 hover:bg-accent hover:text-foreground'

/// 渠道格：ID + 名称、地址、平台 / 档位胶囊 + 模型数与优先级。
///
/// 名字与地址同宽截断（hover 见全名）：长名字不该决定整表宽度。订阅账号的邮箱通常就是渠道名，
/// 只在与名称不同时另起一行补出（多个账号时显示首个并标出其余数量，悬停看全部）。
/// 订阅账号（包括被当作渠道名的）一律遮住中间部分再显示；渠道名不是账号就原样，见 `lib/mask.ts`。
/// 供应商控制台直达（new-api #7146）：查余额 / 状态时不必再去搜网址；平时不抢眼，
/// 悬停整行或键盘聚焦时才现身，触屏设备常显。
function ChannelNameCell({ channel: c }: { channel: ChannelRow }) {
  const { t } = useTranslation()
  // 档位按 key 去重；多把 key 档位不同时都列出（Pro · Max 20x）
  const plans = [...new Set(c.keys.map((k) => k.account_plan).filter((v): v is string => Boolean(v)))]
  const accounts = [...new Set(c.keys.map((k) => k.account_label).filter((v): v is string => Boolean(v)))]
  const extra = accounts.filter((label) => label !== c.name)
  const consoleUrl = providerConsoleUrl(c.provider, c.api_base)
  return (
    <div className="flex w-0 min-w-full items-start gap-3">
      <ProviderMark provider={c.provider} className="mt-0.5" />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <span className="flex min-w-0 items-center gap-2 leading-5">
          <span className="truncate text-[15px] font-semibold tracking-[-0.012em]" title={shownName(c)}>
            {shownName(c)}
          </span>
          <span className="shrink-0 font-mono text-[11px] font-normal tracking-normal text-muted-foreground/60 tabular-nums">
            #{c.id}
          </span>
          {consoleUrl !== null && (
            <a
              href={consoleUrl}
              target="_blank"
              rel="noreferrer noopener"
              className="ml-auto -mr-1 shrink-0 rounded-md p-1 text-muted-foreground opacity-0 transition-opacity group-hover/row:opacity-100 hover:bg-muted hover:text-foreground focus-visible:opacity-100 [@media(hover:none)]:opacity-100"
              title={t('admin:providerConsole')}
              aria-label={t('admin:providerConsole')}
            >
              <ExternalLink className="h-3.5 w-3.5" />
            </a>
          )}
        </span>
        <span className="truncate font-mono text-xs leading-4 text-muted-foreground/75" title={c.api_base ?? undefined}>
          {c.api_base ?? '—'}
        </span>
        {extra.length > 0 && (
          <span className="flex min-w-0 items-baseline gap-1.5 text-xs leading-4 text-muted-foreground" title={accounts.map(maskAccount).join('\n')}>
            <span className="shrink-0 text-muted-foreground/75">{t('admin:channelAccount')}</span>
            <span className="truncate">{maskAccount(extra[0])}</span>
            {extra.length > 1 && <span className="shrink-0">+{extra.length - 1}</span>}
          </span>
        )}
        <span className="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-1">
          <ProviderChip provider={c.provider} plans={plans} />
          <span className="whitespace-nowrap text-xs text-muted-foreground/85 tabular-nums">
            {t('admin:channelModelsPriority', { n: c.models.length, priority: c.priority })}
          </span>
        </span>
      </div>
    </div>
  )
}
