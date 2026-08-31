import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/admin/channels')({
  component: ChannelsPage,
})

interface ChannelRow {
  id: number
  name: string
  provider: string
  api_base: string | null
  status: number
  priority: number
  models: string[]
}

function ChannelsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [error, setError] = useState<string | null>(null)
  const [editing, setEditing] = useState<number | null>(null)
  const [picked, setPicked] = useState<Set<number>>(new Set())
  const [form, setForm] = useState({
    name: '',
    provider: 'openai',
    api_base: '',
    credential: '',
    models: '',
    priority: '0',
  })
  const [advanced, setAdvanced] = useState('')

  const channels = useQuery({
    queryKey: qk.adminChannels,
    queryFn: () => apiFetch<{ data: ChannelRow[] }>('/admin/channels'),
  })

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminChannels })

  const create = useMutation({
    mutationFn: () =>
      apiFetch('/admin/channels', {
        method: 'POST',
        body: {
          name: form.name,
          provider: form.provider,
          api_base: form.api_base,
          credential: form.credential,
          models: form.models
            .split(',')
            .map((m) => m.trim())
            .filter(Boolean),
          priority: Number(form.priority) || 0,
          settings: advanced.trim() ? (JSON.parse(advanced) as unknown) : undefined,
        },
      }),
    onSuccess: () => {
      setError(null)
      setForm({ name: '', provider: 'openai', api_base: '', credential: '', models: '', priority: '0' })
      setAdvanced('')
      invalidate()
    },
    onError: (err) =>
      setError(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })

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
      setEditing(null)
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

  const [probe, setProbe] = useState<string | null>(null)
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

  // 批量操作只在勾选后可用；空选不发请求（后端亦会 400）
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

  const togglePick = (id: number) =>
    setPicked((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  const fields = [
    ['name', t('admin:channelName')],
    ['provider', t('admin:provider')],
    ['api_base', t('admin:apiBase')],
    ['credential', t('admin:credential')],
    ['models', t('admin:models')],
    ['priority', t('admin:priority')],
  ] as const

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardHeader>
          <CardTitle>{t('admin:createChannel')}</CardTitle>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={field}>{label}</Label>
              <Input
                id={field}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
          <div className="col-span-full flex flex-col gap-1.5">
            <Label htmlFor="advanced">{t('admin:advancedSettings')}</Label>
            <textarea
              id="advanced"
              className="min-h-16 w-full rounded-md border bg-transparent p-2 font-mono text-xs"
              spellCheck={false}
              value={advanced}
              onChange={(e) => setAdvanced(e.target.value)}
              placeholder='{"thinking_to_content":true,"bill_by_response_model":true,"strip_request_fields":["logit_bias"]}'
            />
          </div>
          <div className="col-span-full flex items-center gap-3">
            <Button
              disabled={create.isPending || !form.name || !form.credential || !form.models}
              onClick={() => create.mutate()}
            >
              {t('common:create')}
            </Button>
            {error && <span className="text-xs text-destructive">{error}</span>}
          </div>
        </CardContent>
      </Card>

      {picked.size > 0 && (
        <Card>
          <CardContent className="flex flex-wrap items-center gap-3 py-3">
            <span className="text-sm">{t('admin:batchSelected', { n: picked.size })}</span>
            <Button size="sm" variant="outline" onClick={() => batch.mutate('enable')}>
              {t('common:enabled')}
            </Button>
            <Button size="sm" variant="outline" onClick={() => batch.mutate('disable')}>
              {t('common:disabled')}
            </Button>
            <Button size="sm" variant="destructive" onClick={() => batch.mutate('delete')}>
              {t('common:delete')}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setPicked(new Set())}>
              {t('common:cancel')}
            </Button>
          </CardContent>
        </Card>
      )}

      {probe && <p className="text-xs text-muted-foreground">{probe}</p>}

      {channels.isError ? (
        <p className="text-sm text-destructive">{describeError(channels.error)}</p>
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th />
              <Th>ID</Th>
              <Th>{t('admin:channelName')}</Th>
              <Th>{t('admin:provider')}</Th>
              <Th>{t('admin:apiBase')}</Th>
              <Th>{t('admin:priority')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {(channels.data?.data ?? []).map((c) => (
              <Tr key={c.id}>
                <Td>
                  <input
                    type="checkbox"
                    aria-label={t('admin:batchPick', { name: c.name })}
                    checked={picked.has(c.id)}
                    onChange={() => togglePick(c.id)}
                  />
                </Td>
                <Td>{c.id}</Td>
                <Td>{c.name}</Td>
                <Td>
                  <Badge>{c.provider}</Badge>
                </Td>
                <Td className="max-w-56 truncate font-mono text-xs">{c.api_base ?? '—'}</Td>
                <Td>{c.priority}</Td>
                <Td>
                  <Badge variant={c.status === 1 ? 'success' : 'muted'}>
                    {c.status === 1 ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td className="flex flex-wrap gap-1.5">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setStatus.mutate({ id: c.id, status: c.status === 1 ? 2 : 1 })}
                  >
                    {c.status === 1 ? t('common:disabled') : t('common:enabled')}
                  </Button>
                  <Button size="sm" variant="outline" onClick={() => test.mutate(c.id)}>
                    {t('admin:testChannel')}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setEditing(editing === c.id ? null : c.id)}
                  >
                    {t('common:edit')}
                  </Button>
                  <Button size="sm" variant="outline" onClick={() => duplicate.mutate(c)}>
                    {t('admin:duplicate')}
                  </Button>
                  <Button
                    size="sm"
                    variant="destructive"
                    onClick={() => remove.mutate(c.id)}
                  >
                    {t('common:delete')}
                  </Button>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {editing !== null && (
        <ChannelEditor
          channel={(channels.data?.data ?? []).find((c) => c.id === editing)}
          onDone={invalidate}
          onClose={() => setEditing(null)}
        />
      )}
    </div>
  )
}

/// 渠道编辑面板：配置局部更新 + 上游模型发现 + 凭证轮换。
/// 三者审计语义不同，后端也是三个端点，故 UI 上分区呈现而不合并成一次提交。
function ChannelEditor({
  channel,
  onDone,
  onClose,
}: {
  channel: ChannelRow | undefined
  onDone: () => void
  onClose: () => void
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [form, setForm] = useState({
    name: channel?.name ?? '',
    api_base: channel?.api_base ?? '',
    models: (channel?.models ?? []).join(','),
    priority: String(channel?.priority ?? 0),
  })
  const [settings, setSettings] = useState('')
  const [credential, setCredential] = useState('')

  if (!channel) return null
  const id = channel.id

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${id}`, {
        method: 'PATCH',
        body: {
          name: form.name,
          api_base: form.api_base,
          models: form.models
            .split(',')
            .map((m) => m.trim())
            .filter(Boolean),
          priority: Number(form.priority) || 0,
          settings: settings.trim() ? (JSON.parse(settings) as unknown) : undefined,
        },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      onDone()
    },
    onError: (err) =>
      setMsg(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })

  const rotate = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${id}/credential`, {
        method: 'POST',
        body: { credential },
      }),
    onSuccess: () => {
      setCredential('')
      setMsg(t('admin:credentialRotated'))
      onDone()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  // 可见性矩阵（§6.3）：哪些定价分组能看到本渠道。空集 = 不限（全部可见）
  const groups = useQuery({
    queryKey: qk.adminGroups,
    queryFn: () => apiFetch<{ data: { group_code: string }[] }>('/admin/groups'),
  })
  const [visible, setVisible] = useState<Set<string>>(new Set())
  const saveGroups = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${id}/groups`, {
        method: 'POST',
        body: { groups: [...visible] },
      }),
    onSuccess: () => {
      setMsg(t('admin:visibilitySaved'))
      onDone()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  // 模型发现：手动触发，不随面板打开自动外呼上游
  const discovered = useQuery({
    queryKey: qk.channelModels(id),
    queryFn: () => apiFetch<{ data: string[] }>(`/admin/channels/${id}/fetch-models`),
    enabled: false,
  })

  const fields = [
    ['name', t('admin:channelName')],
    ['api_base', t('admin:apiBase')],
    ['models', t('admin:models')],
    ['priority', t('admin:priority')],
  ] as const

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:editChannel', { name: channel.name })}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={`edit-${field}`}>{label}</Label>
              <Input
                id={`edit-${field}`}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
        </div>

        <div className="flex flex-col gap-1.5">
          <Label htmlFor="edit-settings">{t('admin:advancedSettings')}</Label>
          <textarea
            id="edit-settings"
            className="min-h-16 w-full rounded-md border bg-transparent p-2 font-mono text-xs"
            spellCheck={false}
            value={settings}
            onChange={(e) => setSettings(e.target.value)}
            placeholder='{"thinking_to_content":true}'
          />
        </div>

        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="edit-credential">{t('admin:rotateCredential')}</Label>
            <Input
              id="edit-credential"
              value={credential}
              onChange={(e) => setCredential(e.target.value)}
              placeholder="sk-..."
            />
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={!credential.trim() || rotate.isPending}
            onClick={() => rotate.mutate()}
          >
            {t('admin:rotate')}
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => void queryClient.fetchQuery({
              queryKey: qk.channelModels(id),
              queryFn: () => apiFetch<{ data: string[] }>(`/admin/channels/${id}/fetch-models`),
            })}
          >
            {t('admin:fetchModels')}
          </Button>
        </div>

        {discovered.data && (
          <p className="text-xs text-muted-foreground">
            {t('admin:discovered', { n: discovered.data.data.length })}
            <button
              type="button"
              className="ml-2 underline"
              onClick={() => setForm((f) => ({ ...f, models: discovered.data.data.join(',') }))}
            >
              {t('admin:applyDiscovered')}
            </button>
          </p>
        )}

        <div className="flex flex-col gap-1.5 border-t border-border pt-3">
          <Label>{t('admin:visibility')}</Label>
          <p className="text-xs text-muted-foreground">{t('admin:visibilityHint')}</p>
          <div className="flex flex-wrap gap-3">
            {(groups.data?.data ?? []).map((g) => (
              <label key={g.group_code} className="flex items-center gap-2 font-mono text-xs">
                <input
                  type="checkbox"
                  checked={visible.has(g.group_code)}
                  onChange={() =>
                    setVisible((prev) => {
                      const next = new Set(prev)
                      if (next.has(g.group_code)) next.delete(g.group_code)
                      else next.add(g.group_code)
                      return next
                    })
                  }
                />
                {g.group_code}
              </label>
            ))}
            <Button size="sm" variant="outline" onClick={() => saveGroups.mutate()}>
              {t('admin:saveVisibility')}
            </Button>
          </div>
        </div>

        <div className="flex items-center gap-3">
          <Button disabled={save.isPending} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          {msg && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>
      </CardContent>
    </Card>
  )
}
