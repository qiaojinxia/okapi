import { Activity, Copy, Pencil, Plus, Power, PowerOff, Trash2 } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ChannelDrawer } from '@/features/channels/ChannelDrawer'
import { Checkbox } from '@/components/ui/checkbox'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { Input, Label } from '@/components/ui/input'
import { PROVIDERS } from '@/features/channels/types'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { Select } from '@/components/ui/select'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

/// 渠道列表页。
///
/// 只负责"找到渠道并对其批量处理"：新建与编辑都在抽屉里完成，页面本身不放表单。
/// 此前建表单、列表、展开式编辑器堆在同一屏，用户要滚过整片表单才看到数据。
export function ChannelsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [error, setError] = useState<string | null>(null)
  const [probe, setProbe] = useState<string | null>(null)
  const [picked, setPicked] = useState<Set<number>>(new Set())
  const [search, setSearch] = useState('')
  const [providerFilter, setProviderFilter] = useState('')
  const [drawer, setDrawer] = useState<{ mode: 'create' } | { mode: 'edit'; id: number } | null>(
    null,
  )
  const { confirm, dialog } = useConfirm()

  const channels = useQuery({
    queryKey: qk.adminChannels,
    queryFn: () => apiFetch<{ data: ChannelRow[] }>('/admin/channels'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminChannels })

  const setStatus = useMutation({
    mutationFn: (arg: { id: number; status: number }) =>
      apiFetch(`/admin/channels/${arg.id}/status`, {
        method: 'POST',
        body: { status: arg.status },
      }),
    onSuccess: invalidate,
    onError: (err) => setError(describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (id: number) => apiFetch(`/admin/channels/${id}`, { method: 'DELETE' }),
    onSuccess: () => {
      setDrawer(null)
      invalidate()
    },
    onError: (err) => setError(describeError(err)),
  })

  const duplicate = useMutation({
    mutationFn: (c: ChannelRow) =>
      apiFetch(`/admin/channels/${c.id}/duplicate`, {
        method: 'POST',
        body: { name: `${c.name}-copy` },
      }),
    onSuccess: invalidate,
    onError: (err) => setError(describeError(err)),
  })

  const test = useMutation({
    mutationFn: (id: number) =>
      apiFetch<{ ok: boolean; latency_ms?: number; error_code?: string }>(
        `/admin/channels/${id}/test`,
        { method: 'POST', body: {} },
      ),
    onSuccess: (r) =>
      setProbe(
        r.ok
          ? t('admin:testOk', { ms: r.latency_ms ?? 0 })
          : t('admin:testFail', { code: r.error_code ?? 'unknown' }),
      ),
    onError: (err) => setProbe(describeError(err)),
  })

  const batch = useMutation({
    mutationFn: (action: 'enable' | 'disable' | 'delete') =>
      apiFetch<{ affected: number }>('/admin/channels/batch', {
        method: 'POST',
        body: { ids: [...picked], action },
      }),
    onSuccess: () => {
      setPicked(new Set())
      invalidate()
    },
    onError: (err) => setError(describeError(err)),
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
        action={
          <Button onClick={() => setDrawer({ mode: 'create' })}>
            <Plus className="h-4 w-4" />
            {t('admin:createChannel')}
          </Button>
        }
      />

      <Toolbar
        filters={
          <>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="ch-search">{t('common:search')}</Label>
              <Input
                id="ch-search"
                className="w-56"
                value={search}
                placeholder={t('admin:channelSearchHint')}
                onChange={(e) => setSearch(e.target.value)}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="ch-provider">{t('admin:provider')}</Label>
              <Select
                id="ch-provider"
                className="w-40"
                value={providerFilter}
                onChange={setProviderFilter}
                placeholder={t('common:all')}
                options={PROVIDERS.map((p) => ({ value: p, label: p }))}
              />
            </div>
          </>
        }
        selection={
          picked.size > 0 ? (
            <>
              <span className="text-sm">{t('admin:batchSelected', { n: picked.size })}</span>
              <Button size="sm" variant="outline" onClick={() => batch.mutate('enable')}>
                {t('common:enabled')}
              </Button>
              <Button size="sm" variant="outline" onClick={() => batch.mutate('disable')}>
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
                {t('common:delete')}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setPicked(new Set())}>
                {t('common:cancel')}
              </Button>
            </>
          ) : undefined
        }
      />

      {error !== null && <ErrorState message={error} />}
      {probe !== null && <p className="text-xs text-muted-foreground">{probe}</p>}

      {channels.isError ? (
        <ErrorState message={describeError(channels.error)} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:channelsEmptyHint')} />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>
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
              <Th>{t('admin:models')}</Th>
              <Th>{t('admin:priority')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((c) => (
              <Tr key={c.id}>
                <Td>
                  <Checkbox
                    srLabel={t('admin:batchPick', { name: c.name })}
                    checked={picked.has(c.id)}
                    onChange={() => togglePick(c.id)}
                  />
                </Td>
                <Td>{c.id}</Td>
                <Td>
                  <div className="flex flex-col">
                    <span>{c.name}</span>
                    <span className="max-w-56 truncate font-mono text-xs text-muted-foreground">
                      {c.api_base ?? '—'}
                    </span>
                  </div>
                </Td>
                <Td>
                  <Badge>{c.provider}</Badge>
                </Td>
                <Td className="text-xs text-muted-foreground">
                  {t('admin:modelCount', { n: c.models.length })}
                </Td>
                <Td>{c.priority}</Td>
                <Td>
                  <Badge variant={c.status === 1 ? 'success' : 'muted'}>
                    {c.status === 1 ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td>
                  <div className="flex items-center gap-0.5">
                    <IconButton
                      icon={c.status === 1 ? PowerOff : Power}
                      label={c.status === 1 ? t('common:disabled') : t('common:enabled')}
                      onClick={() => setStatus.mutate({ id: c.id, status: c.status === 1 ? 2 : 1 })}
                    />
                    <IconButton
                      icon={Activity}
                      label={t('admin:testChannel')}
                      onClick={() => test.mutate(c.id)}
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

      {dialog}
      {drawer !== null && (
        <ChannelDrawer
          channel={editingChannel}
          onClose={() => setDrawer(null)}
          onDone={invalidate}
        />
      )}
    </div>
  )
}
