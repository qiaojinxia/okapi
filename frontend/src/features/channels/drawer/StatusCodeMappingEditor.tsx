import { ArrowRight, Plus, Trash2 } from 'lucide-react'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'

interface StatusRow {
  id: number
  from: string
  to: string
}

function validErrorStatus(raw: string): number | null {
  const trimmed = raw.trim()
  if (!/^\d{3}$/.test(trimmed)) return null
  const n = Number(trimmed)
  return n >= 400 && n <= 599 ? n : null
}

/// 渠道级上游错误状态码归一化映射（`settings.status_code_mapping`）。
/// 仅重写返回给客户端的 HTTP 状态码，不改动内部重试/冷却状态机与请求日志记录的真实上游状态码。
export function StatusCodeMappingEditor({
  value,
  onChange,
}: {
  value: Record<string, number>
  onChange: (next: Record<string, number>) => void
}) {
  const { t } = useTranslation()
  const seq = useRef(0)
  const [rows, setRows] = useState<StatusRow[]>(() =>
    Object.entries(value).map(([from, to]) => ({ id: seq.current++, from, to: String(to) })),
  )

  const commit = (next: StatusRow[]) => {
    setRows(next)
    const obj: Record<string, number> = {}
    for (const row of next) {
      const fromCode = validErrorStatus(row.from)
      const toCode = validErrorStatus(row.to)
      if (fromCode !== null && toCode !== null) {
        obj[String(fromCode)] = toCode
      }
    }
    onChange(obj)
  }

  const updateRow = (id: number, patch: Partial<StatusRow>) =>
    commit(rows.map((r) => (r.id === id ? { ...r, ...patch } : r)))

  const removeRow = (id: number) => commit(rows.filter((r) => r.id !== id))

  const addRow = (from = '', to = '') => commit([...rows, { id: seq.current++, from, to }])

  return (
    <div data-slot="status-code-mapping-editor" className="flex flex-col gap-3 border-t border-border/60 pt-4">
      <div className="flex flex-col gap-1">
        <Label className="text-[13px] text-foreground">{t('admin:statusCodeMapping')}</Label>
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:statusCodeMappingHint')}</p>
      </div>

      {rows.map((row) => {
        const fromBad = row.from.trim() !== '' && validErrorStatus(row.from) === null
        const toBad = row.to.trim() !== '' && validErrorStatus(row.to) === null
        return (
          <div
            key={row.id}
            data-slot="status-code-mapping-row"
            className="flex items-center gap-2 rounded-lg border border-border/65 bg-card/75 p-2"
          >
            <Input
              className="h-8 w-28 font-mono text-xs"
              inputMode="numeric"
              value={row.from}
              aria-invalid={fromBad}
              placeholder="503"
              aria-label={t('admin:statusCodeMappingFrom')}
              onChange={(e) => updateRow(row.id, { from: e.target.value })}
            />
            <ArrowRight aria-hidden className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
            <Input
              className="h-8 w-28 font-mono text-xs"
              inputMode="numeric"
              value={row.to}
              aria-invalid={toBad}
              placeholder="429"
              aria-label={t('admin:statusCodeMappingTo')}
              onChange={(e) => updateRow(row.id, { to: e.target.value })}
            />
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="ml-auto h-8 w-8 shrink-0 text-muted-foreground hover:text-destructive"
              aria-label={t('common:delete')}
              onClick={() => removeRow(row.id)}
            >
              <Trash2 className="h-3.5 w-3.5" />
            </Button>
          </div>
        )
      })}

      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" size="sm" variant="outline" onClick={() => addRow()}>
          <Plus className="h-3.5 w-3.5" />
          {t('admin:statusCodeMappingAdd')}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="text-xs text-muted-foreground"
          onClick={() => addRow('503', '429')}
        >
          + 503 → 429
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="text-xs text-muted-foreground"
          onClick={() => addRow('500', '502')}
        >
          + 500 → 502
        </Button>
      </div>
    </div>
  )
}
