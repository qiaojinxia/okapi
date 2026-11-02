import { Plus } from 'lucide-react'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'

/// 可增删的键值行带一个稳定 id 作 React key、增删改都按 id 定位。用下标作 key 时删掉中间一行，
/// React 会把后一行的 DOM 让给前一行的数据：焦点（刚点的 ×、正在输入的框）跟着落到另一行，
/// 继续打字或回车就改错 / 删错行。
interface KeyValueRow {
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
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-1">
        <Label className="text-[13px] text-foreground">{t('admin:extraHeaders')}</Label>
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:extraHeadersHint')}</p>
      </div>
      {draft.map((row) => (
        <div key={row.id} className="flex items-center gap-2 rounded-lg border border-border/60 bg-card/70 p-1.5">
          <Input
            className="h-8 border-transparent bg-transparent shadow-none"
            value={row.name}
            placeholder="OpenAI-Organization"
            onChange={(e) => setRow(row.id, { name: e.target.value })}
          />
          <Input className="h-8 border-transparent bg-transparent shadow-none" value={row.val} placeholder="org-…" onChange={(e) => setRow(row.id, { val: e.target.value })} />
          <Button type="button" size="icon" variant="ghost" className="h-8 w-8 shrink-0 text-muted-foreground hover:text-destructive" onClick={() => removeRow(row.id)}>
            ×
          </Button>
        </div>
      ))}
      <Button type="button" size="sm" variant="outline" className="self-start" onClick={() => commit([...draft, { id: nextId.current++, name: '', val: '' }])}>
        <Plus className="h-3.5 w-3.5" />
        {t('admin:extraHeadersAdd')}
      </Button>
    </div>
  )
}

function parseInjectValue(raw: string): unknown {
  const trimmed = raw.trim()
  if (trimmed === '') return ''
  try {
    return JSON.parse(trimmed) as unknown
  } catch {
    return raw
  }
}

function formatInjectValue(value: unknown): string {
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
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-1">
        <Label className="text-[13px] text-foreground">{t('admin:injectFields')}</Label>
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:injectFieldsHint')}</p>
      </div>
      {draft.map((row) => (
        <div key={row.id} className="flex items-center gap-2 rounded-lg border border-border/60 bg-card/70 p-1.5">
          <Input
            className="h-8 border-transparent bg-transparent shadow-none"
            value={row.name}
            placeholder="temperature"
            onChange={(e) => setRow(row.id, { name: e.target.value })}
          />
          <Input
            className="h-8 border-transparent bg-transparent shadow-none"
            value={row.val}
            placeholder='0.2 or "forced"'
            onChange={(e) => setRow(row.id, { val: e.target.value })}
          />
          <Button type="button" size="icon" variant="ghost" className="h-8 w-8 shrink-0 text-muted-foreground hover:text-destructive" onClick={() => removeRow(row.id)}>
            ×
          </Button>
        </div>
      ))}
      <Button type="button" size="sm" variant="outline" className="self-start" onClick={() => commit([...draft, { id: nextId.current++, name: '', val: '' }])}>
        <Plus className="h-3.5 w-3.5" />
        {t('admin:injectFieldsAdd')}
      </Button>
    </div>
  )
}

/// 渠道抽屉：新建与编辑共用，但形态不同。
///
/// - 新建只问三件必答事（接入 / 凭证 / 模型），行为开关与调度参数走缺省——
///   建渠道时用户根本还不知道要不要 thinking 转正文，问了也是瞎选；
/// - 编辑按"接入 / 模型 / 调度 / 行为"分页签：此前六段纵排滚起来找不到北。
///   表单状态提升在抽屉层，切页签不丢改动；底部"保存"提交全部页签的字段。
///
/// 凭证轮换、模型发现、per-key 参数、可见性各自是独立端点（审计语义不同），
/// 故它们在所属页签内单独提交，不并进主"保存"。
