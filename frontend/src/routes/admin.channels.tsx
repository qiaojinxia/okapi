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

      {channels.isError ? (
        <p className="text-sm text-destructive">{describeError(channels.error)}</p>
      ) : (
        <Table>
          <THead>
            <Tr>
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
                <Td>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setStatus.mutate({ id: c.id, status: c.status === 1 ? 2 : 1 })}
                  >
                    {c.status === 1 ? t('common:disabled') : t('common:enabled')}
                  </Button>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}
    </div>
  )
}
