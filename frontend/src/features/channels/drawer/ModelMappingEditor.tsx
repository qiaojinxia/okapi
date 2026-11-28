import { ArrowRight, Plus, Trash2 } from 'lucide-react'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { OptionalSection } from '@/components/ui/optional-section'

interface MappingRow {
  id: number
  pattern: string
  target: string
}

/// 客户端实时预览解析（与后端 `okapi_store::model_mapping::resolve` 同一优先级：精确 > 通配 `*` > 正则 `re:`）。
function previewResolve(mapping: Record<string, string>, model: string): string {
  const input = model.trim()
  if (input === '') return ''
  if (mapping[input] && mapping[input].trim() !== '') return mapping[input].trim()

  const wildcards = Object.entries(mapping)
    .filter(([k, v]) => !k.startsWith('re:') && k.includes('*') && v.trim() !== '')
    .sort(([a], [b]) => {
      const litA = a.replace(/\*/g, '').length
      const litB = b.replace(/\*/g, '').length
      return litB - litA || a.localeCompare(b)
    })
  for (const [pattern, rawTarget] of wildcards) {
    const target = rawTarget.trim()
    const escaped = pattern.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '(.*)')
    try {
      const match = new RegExp(`^${escaped}$`).exec(input)
      if (!match) continue
      if (target.includes('*')) {
        let idx = 1
        return target.replace(/\*/g, () => match[idx++] ?? '')
      }
      if (target.includes('$')) {
        return target.replace(/\$(\d+)/g, (_, n: string) => match[Number(n)] ?? '')
      }
      return target
    } catch {
      continue
    }
  }

  const regexes = Object.entries(mapping)
    .filter(([k, v]) => k.startsWith('re:') && k.length > 3 && v.trim() !== '')
    .sort(([a], [b]) => b.length - a.length || a.localeCompare(b))
  for (const [key, rawTarget] of regexes) {
    try {
      const re = new RegExp(key.slice(3))
      if (re.test(input)) return input.replace(re, rawTarget.trim())
    } catch {
      continue
    }
  }
  return input
}

/// 渠道模型映射编辑器：支持精确映射、通配符 `*`（如 `claude-* → us.anthropic.claude-*-v1:0`）与正则 `re:`，附带试算预览。
export function ModelMappingEditor({
  value,
  onChange,
}: {
  value: Record<string, string>
  onChange: (next: Record<string, string>) => void
}) {
  const { t } = useTranslation()
  const seq = useRef(0)
  const [rows, setRows] = useState<MappingRow[]>(() =>
    Object.entries(value).map(([pattern, target]) => ({ id: seq.current++, pattern, target })),
  )
  const [sample, setSample] = useState('')

  const commit = (next: MappingRow[]) => {
    setRows(next)
    const obj: Record<string, string> = {}
    for (const row of next) {
      const k = row.pattern.trim()
      const v = row.target.trim()
      if (k !== '' && v !== '') obj[k] = v
    }
    onChange(obj)
  }

  const updateRow = (id: number, patch: Partial<MappingRow>) =>
    commit(rows.map((r) => (r.id === id ? { ...r, ...patch } : r)))

  const removeRow = (id: number) => commit(rows.filter((r) => r.id !== id))

  const addRow = (pattern = '', target = '') =>
    commit([...rows, { id: seq.current++, pattern, target }])

  const activeCount = Object.keys(value).length
  const preview = sample.trim() !== '' ? previewResolve(value, sample) : ''

  return (
    <OptionalSection
      id="channel-model-mapping"
      title={t('admin:modelMappingTitle')}
      hint={t('admin:modelMappingHint')}
      defaultOpen={activeCount > 0}
      summary={
        activeCount > 0
          ? t('admin:modelMappingCount', { n: activeCount })
          : t('admin:channelOptionsDefault')
      }
    >
      <div data-slot="model-mapping-editor" className="flex flex-col gap-3">
        {rows.map((row) => (
          <div
            key={row.id}
            data-slot="model-mapping-row"
            className="flex flex-wrap items-center gap-2 rounded-lg border border-border/65 bg-card/75 p-2 sm:flex-nowrap"
          >
            <Input
              className="h-8 min-w-36 flex-1 font-mono text-xs"
              value={row.pattern}
              placeholder="claude-* / re:^gpt-(.+)$"
              aria-label={t('admin:modelMappingSource')}
              onChange={(e) => updateRow(row.id, { pattern: e.target.value })}
            />
            <ArrowRight aria-hidden className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
            <Input
              className="h-8 min-w-36 flex-1 font-mono text-xs"
              value={row.target}
              placeholder="us.anthropic.claude-*-v1:0"
              aria-label={t('admin:modelMappingTarget')}
              onChange={(e) => updateRow(row.id, { target: e.target.value })}
            />
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="h-8 w-8 shrink-0 text-muted-foreground hover:text-destructive"
              aria-label={t('common:delete')}
              onClick={() => removeRow(row.id)}
            >
              <Trash2 className="h-3.5 w-3.5" />
            </Button>
          </div>
        ))}

        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" size="sm" variant="outline" onClick={() => addRow()}>
            <Plus className="h-3.5 w-3.5" />
            {t('admin:modelMappingAdd')}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="text-xs text-muted-foreground"
            onClick={() => addRow('claude-*', 'us.anthropic.claude-*-v1:0')}
          >
            + {t('admin:modelMappingPresetWildcard')}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="text-xs text-muted-foreground"
            onClick={() => addRow('anthropic/*', '*')}
          >
            + {t('admin:modelMappingPresetStripPrefix')}
          </Button>
        </div>

        {activeCount > 0 && (
          <div className="flex flex-col gap-1.5 rounded-xl border border-border/60 bg-muted/25 p-3">
            <Label htmlFor="model-mapping-preview" className="text-xs text-muted-foreground">
              {t('admin:modelMappingPreviewLabel')}
            </Label>
            <div className="flex flex-wrap items-center gap-2">
              <Input
                id="model-mapping-preview"
                className="h-8 min-w-40 flex-1 font-mono text-xs"
                value={sample}
                placeholder="claude-sonnet-4"
                onChange={(e) => setSample(e.target.value)}
              />
              {preview !== '' && (
                <code
                  data-slot="model-mapping-preview-result"
                  className="inline-flex items-center gap-1.5 rounded-lg border border-primary/25 bg-primary/[0.06] px-2.5 py-1 font-mono text-xs text-foreground"
                >
                  <ArrowRight aria-hidden className="h-3 w-3 text-primary" />
                  {preview}
                </code>
              )}
            </div>
          </div>
        )}
      </div>
    </OptionalSection>
  )
}
