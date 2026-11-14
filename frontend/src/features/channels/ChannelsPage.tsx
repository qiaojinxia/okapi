import {
  Activity,
  Copy,
  Ellipsis,
  Gauge,
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
    { mode: 'create' } | { mode: 'edit'; channel: ChannelRow; focus?: 'limits' } | null
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
        title={t('admin:channelsTitle')}
        description={t('admin:channelsDesc')}
        meta={channels.data && t('admin:channelsSummary', { total, enabled: enabledCount })}
        action={
          <>
            <Button
              variant="outline"
              loading={testAll.isPending}
              // 筛选中的"0 启用"不代表全站没有可测的
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
        // 一行两层、六块各成一列：渠道（是谁）/ 状态（现在能不能打）/ 额度（还剩多少）/ 近 30 天（走了多少量）/
        // 调度（路由怎么看它）/ 操作。此前是四格里各堆四五行，行高一百多、每行还不一样高；现在每一格都是两行文字高，
        // 同一类数字落在同一竖列上，扫一眼就能上下比。定宽布局（table-fixed）：列宽由表头说了算，内容放不下就截断，
        // 不会因为某一行名字长就把别的列挤走；窗口再窄就整表横向滚动。
        <Table stickyHeader className="min-w-[58rem] table-fixed">
          <THead>
            <Tr>
              <Th className="w-11 pr-0 pl-5">
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
              {/* 渠道列不设宽度，吃掉其余各列定宽之后剩下的。三档窗口宽度（表格约 960 / 1100 / 1240 起）各给一组列宽；
                  超宽屏把富余按比例分给状态与额度（条长一点好读），不让渠道列一家独宽 */}
              <Th>{t('admin:channelsTitle')}</Th>
              <Th className="w-[10.5rem] min-[1420px]:w-[13rem] min-[1560px]:w-[13.75rem] min-[1800px]:w-[16%]">{t('common:status')}</Th>
              <Th className="w-[15rem] min-[1420px]:w-[17rem] min-[1560px]:w-[17.5rem] min-[1800px]:w-[24%]">{t('admin:channelColQuota')}</Th>
              <Th numeric className="w-[6.5rem] min-[1420px]:w-[8.5rem] min-[1560px]:w-[8.5rem] min-[1800px]:w-[9rem]">{t('admin:channelColUsage30')}</Th>
              {/* 调度：开关常在；优先级 / 模型数 / 所在池是三级信息，窗口够宽（1560 起）才展开成文字 */}
              <Th className="w-[3.75rem] min-[1560px]:w-[9.25rem] min-[1800px]:w-[11rem]">{t('admin:channelColSchedule')}</Th>
              <Th className="w-[8.25rem] pr-4 text-right">{t('admin:channelColOps')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((c) => {
              const enabled = c.status === 1
              const pools = c.pools ?? []
              return (
              <Tr key={c.id} selected={picked.has(c.id)} data-enabled={enabled || undefined}>
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
                  <ChannelNameCell
                    channel={c}
                    egress={c.egress !== undefined && c.egress.mode !== 'inherit'
                      ? { label: egressLabel(c), title: describeEgress(t, c.egress, proxies.data, proxyGroups.data), mono: c.egress.mode === 'proxy' }
                      : undefined}
                  />
                </Td>
                {/* 第一行回答「现在能不能打」：状态胶囊 + 最近一次测试；第二行是近 24h 的错误率与请求数（点开是健康时间线）。
                    部分 key 不可用时中间多一行原因与恢复时间。凭证到期属于编辑详情，不占列表的扫读空间。 */}
                <Td className="py-3 align-top">
                  <div data-slot="channel-status" className="flex min-w-0 flex-col gap-0.5">
                    <div className="flex h-5 min-w-0 items-center gap-2 overflow-hidden">
                      <KeyStateSummary keys={c.keys} enabled={enabled} />
                      <LastProbe probe={c.last_test} />
                    </div>
                    {/* 不在任何池 = 对谁都不可达：渠道开关绿着、key 全可用也打不到，必须在列表就看见。
                        和「部分 key 不可用」一样占异常才有的那一行 */}
                    {enabled && pools.length === 0 && (
                      <div className="flex leading-5">
                        <Badge variant="destructive" className="min-h-[18px] shrink-0 rounded-[5px] px-1.5 py-0 text-[11px]" title={t('admin:poolOrphanWarning')}>
                          {t('admin:channelOrphan')}
                        </Badge>
                      </div>
                    )}
                    <KeyStateDetail keys={c.keys} enabled={enabled} />
                    <div className="flex h-5 min-w-0 items-center">
                      <Health24h
                        stat={health.data?.data.find((s) => s.channel_id === c.id)}
                        channel={{ id: c.id, name: c.name, provider: c.provider }}
                      />
                    </div>
                  </div>
                </Td>
                {/* 额度：订阅账号是各窗口的用量仪表，设了成本 / Token 上限的渠道是「已用 / 上限」，API 渠道还有上游余额；都没有就是一个破折号 */}
                <Td className="py-3 align-top">
                  <div data-slot="channel-quota" className="flex min-w-0 flex-col gap-1.5">
                    <ChannelQuotaCell
                      channelId={c.id}
                      provider={c.provider}
                      control={c.settings?.account_control}
                      enabled={enabled}
                      fallback={c.last_balance ? null : <span aria-hidden className="leading-5 text-muted-foreground/70">—</span>}
                    />
                    <LastBalance balance={c.last_balance} />
                  </div>
                </Td>
                <Td className="py-3 align-top">
                  <ChannelUsage
                    usage={usage.data?.data.find((u) => u.channel_id === c.id)}
                    settled={usage.data !== undefined}
                  />
                </Td>
                {/* 调度：是否参与调度的开关 + 路由怎么看这条渠道（优先级、服务几个模型、在哪些池）。
                    开关与名称首行同高；文字部分窗口够宽才出现 */}
                <Td className="py-3 align-top">
                  <div data-slot="channel-schedule" className="flex min-w-0 flex-col gap-0.5">
                    <div className="flex h-5 items-center gap-2.5">
                      <Switch
                        hideLabel
                        className="[&>span:last-child]:mt-0"
                        label={t('admin:channelScheduleToggle', { name: shownName(c) })}
                        checked={enabled}
                        disabled={setStatus.isPending}
                        onChange={(on) => setStatus.mutate({ id: c.id, status: on ? 1 : 2 })}
                      />
                      <span className="hidden whitespace-nowrap min-[1560px]:inline">
                        <span className="text-xs text-muted-foreground">{t('admin:priority')}</span>{' '}
                        <span className="text-[13px] font-medium tabular-nums">{c.priority}</span>
                      </span>
                    </div>
                    <span className="hidden truncate text-xs leading-5 text-muted-foreground min-[1560px]:block" title={pools.join(', ') || undefined}>
                      {t('admin:channelModelCount', { n: c.models.length })}
                      {pools.length > 0 && <> · {pools.join(', ')}</>}
                    </span>
                  </div>
                </Td>
                {/* 常用动作（测试 / 编辑）是图标按钮，其余收进「···」菜单；与两行内容垂直居中。
                    图标平时淡一档，悬停整行变深，悬停自己才有底色；名字在悬停提示（便签）与读屏名里。
                    「更多」菜单的触发器不套悬停提示：提示常驻时 Esc 会先关提示，菜单得按两次才关 */}
                <Td className="py-3 pr-4 align-top">
                  <div className="flex h-[42px] items-center justify-end">
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
                          <Button size="icon" variant="ghost" aria-label={t('admin:channelMoreActions')} className={ICON_BUTTON_CLASS}>
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
                        <MenuItem icon={Gauge} onSelect={() => setDrawer({ mode: 'edit', channel: c, focus: 'limits' })}>
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
                              // 对话框里显示的是遮过的名字，要输入的也是它：看到什么就输入什么
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
                </Td>
              </Tr>
              )
            })}
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

/// 上游地址只留主机名（列表里要认的是「哪家站」，完整地址在悬停提示与编辑抽屉里）；写不成地址的原样显示。
function hostOf(base: string): string {
  try {
    return new URL(base).host
  } catch {
    return base
  }
}

const SUBSCRIPTION_PROVIDERS = new Set(['anthropic_max', 'codex'])

/// 渠道格：两行。第一行是名字（这一行唯一的粗体）+ 编号；第二行是「它是什么」——平台 / 档位胶囊，
/// 后面跟账号（与名字不同时）或上游主机名（API 渠道），再后面是显式指定的出口。
///
/// 名字与第二行都按列宽截断（hover 见全文）：长名字不该决定整表宽度。订阅账号的邮箱通常就是渠道名，
/// 只在与名称不同时才补出（多个账号时显示首个并标出其余数量，悬停看全部）。
/// 订阅账号（包括被当作渠道名的）一律遮住中间部分再显示；渠道名不是账号就原样，见 `lib/mask.ts`。
/// 订阅渠道不显示地址：它们都打官方地址，逐行重复一遍只是噪音。
/// 供应商控制台直达（new-api #7146）：查余额 / 状态时不必再去搜网址；平时不抢眼，
/// 悬停整行或键盘聚焦时才现身，触屏设备常显。
function ChannelNameCell({ channel: c, egress }: {
  channel: ChannelRow
  egress?: { label: string; title: string; mono: boolean }
}) {
  const { t } = useTranslation()
  // 档位按 key 去重；多把 key 档位不同时都列出（Pro · Max 20x）
  const plans = [...new Set(c.keys.map((k) => k.account_plan).filter((v): v is string => Boolean(v)))]
  const accounts = [...new Set(c.keys.map((k) => k.account_label).filter((v): v is string => Boolean(v)))]
  const extra = accounts.filter((label) => label !== c.name)
  const consoleUrl = providerConsoleUrl(c.provider, c.api_base)
  const host = !SUBSCRIPTION_PROVIDERS.has(c.provider) && c.api_base ? hostOf(c.api_base) : null
  const enabled = c.status === 1
  return (
    <div className="flex min-w-0 items-center gap-3">
      {/* 停用的渠道整体退后一档：标志褪色、名字变灰，扫一眼就分得出哪些在跑 */}
      <ProviderMark provider={c.provider} size="sm" className={cn('max-[1419px]:h-8 max-[1419px]:w-8', !enabled && 'opacity-50 grayscale')} />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex h-5 min-w-0 items-center gap-1.5">
          <span data-slot="channel-name" className={cn('truncate text-sm font-semibold tracking-[-0.006em]', !enabled && 'text-muted-foreground')} title={shownName(c)}>
            {shownName(c)}
          </span>
          {/* 编号是给对日志用的三级信息，窄窗口先让给名字 */}
          <span className="hidden shrink-0 font-mono text-[11px] font-normal text-muted-foreground/60 tabular-nums min-[1420px]:inline">
            #{c.id}
          </span>
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
        </span>
        <span data-slot="channel-meta" className="flex h-5 min-w-0 items-center gap-2 overflow-hidden text-xs text-muted-foreground">
          <ProviderChip provider={c.provider} plans={plans} compact />
          {/* 标签留给读屏（「账号」「出口」）；可见的只有值，悬停提示挂在值上 */}
          {extra.length > 0 ? (
            <>
              <span className="sr-only">{t('admin:channelAccount')}</span>
              <span data-slot="channel-account" className="min-w-[2.5rem] truncate" title={accounts.map(maskAccount).join('\n')}>
                {maskAccount(extra[0])}
              </span>
              {extra.length > 1 && <span className="-ml-1 shrink-0">+{extra.length - 1}</span>}
            </>
          ) : host !== null && (
            // 窄窗口放不下「主机名 + 出口」两样：出口留着（账号从哪出去更要紧），主机名让位——名字多半已经说明是哪家站
            <span data-slot="channel-host" className={cn('min-w-0 truncate', egress !== undefined && 'max-[1419px]:hidden')} title={c.api_base ?? undefined}>{host}</span>
          )}
          {egress !== undefined && (
            <span data-slot="channel-egress" className="inline-flex min-w-[3.25rem] shrink items-center gap-1">
              <span className="inline-flex shrink-0 text-muted-foreground/70" title={t('admin:channelStatusEgress')}>
                <Globe aria-hidden className="h-3 w-3" strokeWidth={1.8} />
                <span className="sr-only">{t('admin:channelStatusEgress')}</span>
              </span>
              <span className={cn('truncate', egress.mono && 'tabular-nums')} title={egress.title}>{egress.label}</span>
            </span>
          )}
        </span>
      </div>
    </div>
  )
}
