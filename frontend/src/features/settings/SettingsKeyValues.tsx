import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { Lock, Pencil } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { IconButton } from '@/components/ui/icon-button'
import { Input } from '@/components/ui/input'
import { SearchInput } from '@/components/ui/search-input'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'
import { toast } from '@/components/ui/toast'
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

/// 键名前缀（`epay_key_xxx` → `epay`）：设置项天然按前缀成组，表格按组分段，
/// 找一个键先找它的组比在一百行里逐行扫快得多。
function prefixOf(key: string): string {
  const i = key.indexOf('_')
  return i > 0 ? key.slice(0, i) : key
}

/// 系统设置总览 + 编辑。
///
/// 敏感键（含 secret/key/token/password/webhook/credential）后端只回
/// `configured` 布尔占位，明文永不出接口——故此处只能覆写、不能读回。
///
/// 编辑在抽屉里完成：此前"编辑"表单出现在整张表的**底部**，点第 3 行的编辑要滚到
/// 第 60 行下面去找输入框，改完也看不出改的是哪一行。抽屉标题就是键名，
/// 表格本身不动。
export function SettingsCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState<SettingRow | null>(null)
  const [filter, setFilter] = useState('')

  const settings = useQuery({
    queryKey: ['admin', 'settings'],
    queryFn: () => apiFetch<{ data: SettingRow[] }>('/admin/settings'),
  })

  const save = useMutation({
    mutationFn: (arg: { key: string; value: unknown }) =>
      apiFetch('/admin/settings', { method: 'POST', body: arg }),
    onSuccess: (_r, arg) => {
      toast.success(t('common:saved'), arg.key)
      setEditing(null)
      void queryClient.invalidateQueries({ queryKey: ['admin', 'settings'] })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const all = settings.data?.data ?? []
  const rows = all.filter((s) => s.key.toLowerCase().includes(filter.trim().toLowerCase()))
  // 分组：保持后端返回顺序，仅在前缀变化处插一行组头
  const groups: { prefix: string; items: SettingRow[] }[] = []
  for (const s of rows) {
    const p = prefixOf(s.key)
    const last = groups[groups.length - 1]
    if (last && last.prefix === p) last.items.push(s)
    else groups.push({ prefix: p, items: [s] })
  }

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <SearchInput
          className="w-72"
          value={filter}
          placeholder={t('admin:settingFilterHint')}
          onChange={setFilter}
        />
        <span className="text-xs text-muted-foreground tabular-nums">
          {t('common:resultCount', { n: rows.length })}
        </span>
      </div>

      {settings.isError ? (
        <ErrorState message={describeError(settings.error)} onRetry={() => void settings.refetch()} />
      ) : settings.isPending ? (
        <TableSkeleton rows={10} cols={4} />
      ) : rows.length === 0 ? (
        <EmptyState
          title={filter !== '' ? t('common:noResults') : undefined}
          hint={filter !== '' ? t('common:noResultsHint') : undefined}
        />
      ) : (
        <Table dense>
          <THead>
            <Tr>
              <Th>{t('admin:settingKey')}</Th>
              <Th>{t('admin:settingValue')}</Th>
              <Th>{t('admin:settingUpdatedAt')}</Th>
              <Th className="w-16 text-right">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {groups.map((g) => (
              <GroupRows key={g.prefix} group={g} onEdit={setEditing} />
            ))}
          </TBody>
        </Table>
      )}

      {editing !== null && (
        <SettingEditorDrawer
          key={editing.key}
          row={editing}
          pending={save.isPending}
          onCancel={() => setEditing(null)}
          onSave={(value) => save.mutate({ key: editing.key, value })}
        />
      )}
    </div>
  )
}

function GroupRows({
  group,
  onEdit,
}: {
  group: { prefix: string; items: SettingRow[] }
  onEdit: (row: SettingRow) => void
}) {
  const { t } = useTranslation()
  return (
    <>
      {group.items.length > 1 && (
        <tr className="bg-muted/40">
          <td colSpan={4} className="px-3 py-1.5 text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
            {group.prefix}
            <span className="ml-2 font-normal normal-case tabular-nums">
              {t('common:resultCount', { n: group.items.length })}
            </span>
          </td>
        </tr>
      )}
      {group.items.map((s) => (
        <Tr key={s.key} className="cursor-pointer" onClick={() => onEdit(s)}>
          <Td className="font-mono text-xs">
            <span className="inline-flex items-center gap-1.5">
              {s.is_secret && <Lock className="h-3 w-3 text-muted-foreground" />}
              {s.key}
            </span>
          </Td>
          <Td className="max-w-md">
            <ValueCell row={s} />
          </Td>
          <Td className="whitespace-nowrap text-xs text-muted-foreground">
            {s.updated_at ? dayjs(s.updated_at).format('YYYY-MM-DD HH:mm') : '—'}
          </Td>
          <Td className="text-right">
            <IconButton icon={Pencil} label={t('common:edit')} onClick={() => onEdit(s)} />
          </Td>
        </Tr>
      ))}
    </>
  )
}

function ValueCell({ row }: { row: SettingRow }) {
  const { t } = useTranslation()
  if (row.is_secret) {
    return (
      <Badge dot variant={row.configured ? 'success' : 'muted'}>
        {row.configured ? t('admin:settingSet') : t('admin:settingUnset')}
      </Badge>
    )
  }
  if (typeof row.value === 'boolean') {
    return (
      <Badge dot variant={row.value ? 'success' : 'muted'}>
        {row.value ? t('common:enabled') : t('common:disabled')}
      </Badge>
    )
  }
  if (typeof row.value === 'number') {
    return <span className="font-mono text-xs tabular-nums">{row.value}</span>
  }
  if (typeof row.value === 'string') {
    return (
      <span className="block truncate font-mono text-xs" title={row.value}>
        {row.value === '' ? <span className="text-muted-foreground">{t('admin:settingEmpty')}</span> : row.value}
      </span>
    )
  }
  const json = JSON.stringify(row.value)
  return (
    <span className="block truncate font-mono text-xs text-muted-foreground" title={json}>
      {json}
    </span>
  )
}

/// 单个设置项的编辑抽屉：控件随值类型变化；敏感键无明文可回填，留空强制显式覆写。
function SettingEditorDrawer({
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

  const kindLabel = {
    bool: t('admin:settingKindBool'),
    number: t('admin:settingKindNumber'),
    string: row.is_secret ? t('admin:settingKindSecret') : t('admin:settingKindString'),
    json: t('admin:settingKindJson'),
  }[kind]

  return (
    <Drawer
      open
      onClose={onCancel}
      title={row.key}
      description={t('admin:settingEditDesc', { kind: kindLabel })}
      footer={
        <>
          <Button variant="ghost" onClick={onCancel}>
            {t('common:cancel')}
          </Button>
          <Button loading={pending} onClick={submit}>
            {t('common:save')}
          </Button>
        </>
      }
    >
      <form
        onSubmit={(e) => {
          e.preventDefault()
          submit()
        }}
      >
        <FieldGroup title={t('admin:settingValue')}>
          {kind === 'bool' && (
            <div className="rounded-lg border border-border p-3">
              <Switch label={row.key} checked={bool} onChange={setBool} />
            </div>
          )}
          {kind === 'number' && (
            <Field label={t('admin:settingValue')} htmlFor="set-val">
              <Input
                id="set-val"
                className="w-48 font-mono"
                inputMode="numeric"
                value={num}
                onChange={(e) => setNum(e.target.value)}
              />
            </Field>
          )}
          {kind === 'string' && (
            <Field
              label={t('admin:settingValue')}
              htmlFor="set-val"
              hint={row.is_secret ? t('admin:settingSecretHint') : undefined}
            >
              <Input
                id="set-val"
                type={row.is_secret ? 'password' : 'text'}
                autoComplete="off"
                className="font-mono"
                value={text}
                placeholder={row.is_secret ? t('admin:settingSecretHint') : undefined}
                onChange={(e) => setText(e.target.value)}
              />
            </Field>
          )}
          {kind === 'json' && (
            <Field label={t('admin:settingValue')} htmlFor="set-val" error={err}>
              <Textarea
                id="set-val"
                rows={12}
                value={json}
                onChange={(e) => {
                  setJson(e.target.value)
                  setErr(null)
                }}
              />
            </Field>
          )}
        </FieldGroup>
        {row.updated_at && (
          <p className="pt-2 text-xs text-muted-foreground">
            {t('common:updatedAt', { time: dayjs(row.updated_at).format('YYYY-MM-DD HH:mm') })}
          </p>
        )}
      </form>
    </Drawer>
  )
}
