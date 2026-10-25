import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'

/// 可增删的键值行带一个稳定 id 作 React key、增删改都按 id 定位。用下标作 key 时删掉中间一行，
/// React 会把后一行的 DOM 让给前一行的数据：焦点（刚点的 ×、正在输入的框）跟着落到另一行，
/// 继续打字或回车就改错 / 删错行。
export interface KeyValueRow {
  id: number
  name: string
  val: string
}

export function ExtraHeadersEditor({
  value,
  onChange,
}: {
  value: Record<string, string>
  onChange: (next: Record<string, string>) => void
}) {
  const { t } = useTranslation()
  const nextId = useRef(0)
  const [draft, setDraft] = useState<KeyValueRow[]>(() => Object.entries(value).map(([name, val]) => ({ id: nextId.current++, name, val })))
  const commit = (next: KeyValueRow[]) => {
    setDraft(next)
    onChange(Object.fromEntries(next.filter((row) => row.name.trim() !== '').map((row) => [row.name, row.val])))
  }
  const setRow = (id: number, patch: Partial<KeyValueRow>) => commit(draft.map((row) => (row.id === id ? { ...row, ...patch } : row)))
  const removeRow = (id: number) => commit(draft.filter((row) => row.id !== id))
  return (
    <div className="flex flex-col gap-1.5">
      <Label>{t('admin:extraHeaders')}</Label>
      <p className="text-xs text-muted-foreground">{t('admin:extraHeadersHint')}</p>
      {draft.map((row) => (
        <div key={row.id} className="flex gap-2">
          <Input
            value={row.name}
            placeholder="OpenAI-Organization"
            onChange={(e) => setRow(row.id, { name: e.target.value })}
          />
          <Input value={row.val} placeholder="org-…" onChange={(e) => setRow(row.id, { val: e.target.value })} />
          <Button type="button" variant="ghost" onClick={() => removeRow(row.id)}>
            ×
          </Button>
        </div>
      ))}
      <Button type="button" variant="outline" onClick={() => commit([...draft, { id: nextId.current++, name: '', val: '' }])}>
        {t('admin:extraHeadersAdd')}
      </Button>
    </div>
  )
}

export function parseInjectValue(raw: string): unknown {
  const trimmed = raw.trim()
  if (trimmed === '') return ''
  try {
    return JSON.parse(trimmed) as unknown
  } catch {
    return raw
  }
}

export function formatInjectValue(value: unknown): string {
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

export function InjectFieldsEditor({
  value,
  onChange,
}: {
  value: Record<string, unknown>
  onChange: (next: Record<string, unknown>) => void
}) {
  const { t } = useTranslation()
  const nextId = useRef(0)
  const [draft, setDraft] = useState<KeyValueRow[]>(() =>
    Object.entries(value).map(([name, v]) => ({ id: nextId.current++, name, val: formatInjectValue(v) })),
  )
  const commit = (next: KeyValueRow[]) => {
    setDraft(next)
    const obj: Record<string, unknown> = {}
    for (const row of next) {
      if (row.name.trim() === '') continue
      obj[row.name.trim()] = parseInjectValue(row.val)
    }
    onChange(obj)
  }
  const setRow = (id: number, patch: Partial<KeyValueRow>) => commit(draft.map((row) => (row.id === id ? { ...row, ...patch } : row)))
  const removeRow = (id: number) => commit(draft.filter((row) => row.id !== id))
  return (
    <div className="flex flex-col gap-1.5">
      <Label>{t('admin:injectFields')}</Label>
      <p className="text-xs text-muted-foreground">{t('admin:injectFieldsHint')}</p>
      {draft.map((row) => (
        <div key={row.id} className="flex gap-2">
          <Input
            value={row.name}
            placeholder="temperature"
            onChange={(e) => setRow(row.id, { name: e.target.value })}
          />
          <Input
            value={row.val}
            placeholder='0.2 or "forced"'
            onChange={(e) => setRow(row.id, { val: e.target.value })}
          />
          <Button type="button" variant="ghost" onClick={() => removeRow(row.id)}>
            ×
          </Button>
        </div>
      ))}
      <Button type="button" variant="outline" onClick={() => commit([...draft, { id: nextId.current++, name: '', val: '' }])}>
        {t('admin:injectFieldsAdd')}
      </Button>
    </div>
  )
}
