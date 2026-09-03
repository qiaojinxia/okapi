import {
  Activity,
  Copy,
  ExternalLink,
  Pencil,
  Plus,
  Power,
  PowerOff,
  Server,
  Stethoscope,
  Trash2,
} from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ChannelDrawer } from '@/features/channels/ChannelDrawer'
import {
  Health24h,
  KeyStateSummary,
  LastProbe,
  useChannelHealth24h,
} from '@/features/channels/ChannelHealthCell'
import { RouteDiagnosisDrawer } from '@/features/channels/RouteDiagnosis'
import { Checkbox } from '@/components/ui/checkbox'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { PROVIDERS, providerConsoleUrl } from '@/features/channels/types'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { SearchInput } from '@/components/ui/search-input'
import { Select } from '@/components/ui/select'
import { SelectionBar } from '@/components/ui/selection-bar'
import { TableSkeleton } from '@/components/ui/skeleton'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

/// 渠道列表页。
///
/// 只负责"找到渠道并对其批量处理"：新建与编辑都在抽屉里完成，页面本身不放表单。
/// 此前建表单、列表、展开式编辑器堆在同一屏，用户要滚过整片表单才看到数据。
///
/// 操作反馈全部走 toast：测活结果、批量结果、失败原因此前都是工具栏下一行 12px 灰字，
/// 测活成功与失败长得一样，看完也不会消失。
export function ChannelsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [picked, setPicked] = useState<Set<number>>(new Set())
  const [search, setSearch] = useState('')
  const [providerFilter, setProviderFilter] = useState('')
  const [drawer, setDrawer] = useState<{ mode: 'create' } | { mode: 'edit'; id: number } | null>(
    null,
  )
  const [diagnosing, setDiagnosing] = useState(false)
  const [testingId, setTestingId] = useState<number | null>(null)
  const { confirm, dialog } = useConfirm()

  // 近 24h 健康：一次整表查询按 channel_id 分发到各行（CH 未启用则各行显示 —）
  const health = useChannelHealth24h()
  const channels = useQuery({
    queryKey: qk.adminChannels,
    queryFn: () => apiFetch<{ data: ChannelRow[] }>('/admin/channels'),
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

  const probeOne = (id: number) =>
    apiFetch<{ ok: boolean; latency_ms?: number; error_code?: string }>(
      `/admin/channels/${id}/test`,
      { method: 'POST', body: {} },
    )
  const test = useMutation({
    mutationFn: (c: ChannelRow) => {
      setTestingId(c.id)
      return probeOne(c.id)
    },
    onSuccess: (r, c) => {
      if (r.ok) toast.success(c.name, t('admin:testOk', { ms: r.latency_ms ?? 0 }))
      else toast.error(c.name, t('admin:testFail', { code: r.error_code ?? 'unknown' }))
      // 结果已在服务端留痕，刷新列表让"最近测试"列跟上
      invalidate()
    },
    onError: fail,
    onSettled: () => setTestingId(null),
  })

  // 测试全部启用渠道（new-api 同有）：并发 3 路——太多会让一批上游同时看到探测，
  // 太少几十条渠道要等很久；逐条失败不中断，最后汇总成功/失败数
  const testAll = useMutation({
    mutationFn: async (ids: number[]) => {
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

  const all = channels.data?.data ?? []
  // 过滤在前端做：渠道数量是运营规模（几十到几百），拉全量再筛比每次改条件都往
  // 后端跑一趟更顺手，也省掉一套分页状态。
  const rows = all.filter((c) => {
    const kw = search.trim().toLowerCase()
    const hitKw =
      kw === '' ||
      c.name.toLowerCase().includes(kw) ||
      (c.api_base ?? '').toLowerCase().includes(kw)
    return hitKw && (providerFilter === '' || c.provider === providerFilter)
  })
  const allPicked = rows.length > 0 && rows.every((c) => picked.has(c.id))
  const somePicked = rows.some((c) => picked.has(c.id))
  const filtered = search.trim() !== '' || providerFilter !== ''
  const enabledCount = all.filter((c) => c.status === 1).length

  const togglePick = (id: number) =>
    setPicked((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  const editingChannel =
    drawer?.mode === 'edit' ? all.find((c) => c.id === drawer.id) : undefined

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:channelsTitle')}
        description={t('admin:channelsDesc')}
        icon={Server}
        meta={
          channels.data && (
            <Badge variant="muted">
              {t('admin:channelsSummary', { total: all.length, enabled: enabledCount })}
            </Badge>
          )
        }
        action={
          <>
            <Button
              variant="outline"
              loading={testAll.isPending}
              disabled={enabledCount === 0}
              onClick={() => testAll.mutate(all.filter((c) => c.status === 1).map((c) => c.id))}
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
            <SearchInput
              id="ch-search"
              className="w-64"
              value={search}
              placeholder={t('admin:channelSearchHint')}
              onChange={setSearch}
            />
            <Select
              id="ch-provider"
              className="w-40"
              aria-label={t('admin:provider')}
              value={providerFilter}
              onChange={setProviderFilter}
              placeholder={t('common:all')}
              options={PROVIDERS.map((p) => ({ value: p, label: p }))}
            />
            {filtered && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  setSearch('')
                  setProviderFilter('')
                }}
              >
                {t('common:clearFilters')}
              </Button>
            )}
          </>
        }
        selection={
          <span className="text-xs text-muted-foreground tabular-nums">
            {t('common:resultCount', { n: rows.length })}
          </span>
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
              <Button
                variant="outline"
                onClick={() => {
                  setSearch('')
                  setProviderFilter('')
                }}
              >
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
        <Table>
          <THead>
            <Tr>
              <Th className="w-10">
                <Checkbox
                  srLabel={t('admin:batchPickAll')}
                  checked={allPicked}
                  indeterminate={somePicked && !allPicked}
                  onChange={(on) => setPicked(on ? new Set(rows.map((c) => c.id)) : new Set())}
                />
              </Th>
              <Th>ID</Th>
              <Th>{t('admin:channelName')}</Th>
              <Th>{t('admin:provider')}</Th>
              <Th numeric>{t('admin:models')}</Th>
              <Th numeric>{t('admin:priority')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:probeCol')}</Th>
              <Th>{t('admin:health24hCol')}</Th>
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
                <Td className="text-xs text-muted-foreground tabular-nums">{c.id}</Td>
                <Td>
                  <div className="flex flex-col">
                    {/* 名字与地址同宽截断（hover 见全名）：长名字不该决定整表宽度 */}
                    <span className="max-w-56 truncate font-medium" title={c.name}>
                      {c.name}
                    </span>
                    <span className="max-w-56 truncate font-mono text-xs text-muted-foreground">
                      {c.api_base ?? '—'}
                    </span>
                  </div>
                </Td>
                <Td>
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
                </Td>
                <Td numeric className="text-xs text-muted-foreground">
                  {t('admin:modelCount', { n: c.models.length })}
                </Td>
                <Td numeric>{c.priority}</Td>
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
                  </div>
                </Td>
                <Td>
                  <LastProbe probe={c.last_test} />
                </Td>
                <Td>
                  <Health24h
                    stat={health.data?.data.find((s) => s.channel_id === c.id)}
                    channel={{ id: c.id, name: c.name, provider: c.provider }}
                  />
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
                    <IconButton
                      icon={Pencil}
                      label={t('common:edit')}
                      onClick={() => setDrawer({ mode: 'edit', id: c.id })}
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

      <SelectionBar count={picked.size} onClear={() => setPicked(new Set())}>
        <Button size="sm" variant="outline" loading={batch.isPending} onClick={() => batch.mutate('enable')}>
          <Power className="h-3.5 w-3.5" />
          {t('common:enabled')}
        </Button>
        <Button size="sm" variant="outline" loading={batch.isPending} onClick={() => batch.mutate('disable')}>
          <PowerOff className="h-3.5 w-3.5" />
          {t('common:disabled')}
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
    </div>
  )
}
