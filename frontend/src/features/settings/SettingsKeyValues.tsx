import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Input, Label } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

export interface SettingRow {
  key: string
  value: unknown
  is_secret: boolean
  configured: boolean
  updated_at: string
}


/// 按当前值推断该用什么控件：布尔给开关，数字给数字框，字符串给文本框，
/// 只有对象/数组才退回 JSON。此前一律要求手写 JSON，连 true 都得带引号规则去猜。
export type ValueKind = 'bool' | 'number' | 'string' | 'json'


export function kindOf(v: unknown, isSecret: boolean): ValueKind {
  if (isSecret) return 'string'
  if (typeof v === 'boolean') return 'bool'
  if (typeof v === 'number') return 'number'
  if (typeof v === 'string') return 'string'
  return 'json'
}


/// 系统设置总览 + 就地编辑。
/// 敏感键（含 secret/key/token/password/webhook/credential）后端只回
/// `configured` 布尔占位，明文永不出接口——故此处只能覆写、不能读回。
export function SettingsCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState<SettingRow | null>(null)
  const [msg, setMsg] = useState<string | null>(null)

  const settings = useQuery({
    queryKey: ['admin', 'settings'],
    queryFn: () => apiFetch<{ data: SettingRow[] }>('/admin/settings'),
  })

  const save = useMutation({
    mutationFn: (arg: { key: string; value: unknown }) =>
      apiFetch('/admin/settings', { method: 'POST', body: arg }),
    onSuccess: () => {
      setMsg(t('common:success'))
      setEditing(null)
      void queryClient.invalidateQueries({ queryKey: ['admin', 'settings'] })
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const rows = settings.data?.data ?? []

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:settingKeyValues')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {settings.isError ? (
          <ErrorState message={describeError(settings.error)} />
        ) : rows.length === 0 ? (
          <EmptyState />
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:settingKey')}</Th>
                <Th>{t('admin:settingValue')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {rows.map((s) => (
                <Tr key={s.key}>
                  <Td className="font-mono text-xs">{s.key}</Td>
                  <Td className="max-w-72 truncate font-mono text-xs">
                    {s.is_secret ? (
                      <Badge variant={s.configured ? 'success' : 'muted'}>
                        {s.configured ? t('admin:settingSet') : t('admin:settingUnset')}
                      </Badge>
                    ) : typeof s.value === 'boolean' ? (
                      <Badge variant={s.value ? 'success' : 'muted'}>
                        {s.value ? t('common:enabled') : t('common:disabled')}
                      </Badge>
                    ) : (
                      JSON.stringify(s.value)
                    )}
                  </Td>
                  <Td>
                    <Button size="sm" variant="outline" onClick={() => setEditing(s)}>
                      {t('common:edit')}
                    </Button>
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}

        {editing !== null && (
          <SettingEditor
            row={editing}
            pending={save.isPending}
            onCancel={() => setEditing(null)}
            onSave={(value) => save.mutate({ key: editing.key, value })}
          />
        )}
        {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
      </CardContent>
    </Card>
  )
}


export function SettingEditor({
  row,
  pending,
  onCancel,
  onSave,
}: {
  row: SettingRow
  pending: boolean
  onCancel: () => void
  onSave: (value: unknown) => void
}) {
  const { t } = useTranslation()
  const kind = kindOf(row.value, row.is_secret)
  // 敏感键无明文可回填，留空强制显式覆写
  const initial = row.is_secret ? '' : typeof row.value === 'string' ? row.value : ''
  const [text, setText] = useState(initial)
  const [num, setNum] = useState(typeof row.value === 'number' ? String(row.value) : '')
  const [bool, setBool] = useState(row.value === true)
  const [json, setJson] = useState(kind === 'json' ? JSON.stringify(row.value, null, 2) : '')
  const [err, setErr] = useState<string | null>(null)

  const submit = () => {
    if (kind === 'bool') return onSave(bool)
    if (kind === 'number') return onSave(Number(num) || 0)
    if (kind === 'string') return onSave(text)
    try {
      onSave(JSON.parse(json) as unknown)
    } catch {
      setErr(t('admin:advancedBadJson'))
    }
  }

  return (
    <div className="flex flex-col gap-2 border-t border-border pt-3">
      <Label htmlFor="set-val">{row.key}</Label>
      {kind === 'bool' && (
        <Switch label={row.key} checked={bool} onChange={setBool} />
      )}
      {kind === 'number' && (
        <Input
          id="set-val"
          className="w-40"
          inputMode="numeric"
          value={num}
          onChange={(e) => setNum(e.target.value)}
        />
      )}
      {kind === 'string' && (
        <Input
          id="set-val"
          value={text}
          placeholder={row.is_secret ? t('admin:settingSecretHint') : undefined}
          onChange={(e) => setText(e.target.value)}
        />
      )}
      {kind === 'json' && (
        <Textarea
          id="set-val"
          rows={6}
          className="font-mono text-xs"
          value={json}
          onChange={(e) => setJson(e.target.value)}
        />
      )}
      {err !== null && <span className="text-xs text-destructive">{err}</span>}
      <div className="flex gap-2">
        <Button size="sm" disabled={pending} onClick={submit}>
          {t('common:save')}
        </Button>
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {t('common:cancel')}
        </Button>
      </div>
    </div>
  )
}
