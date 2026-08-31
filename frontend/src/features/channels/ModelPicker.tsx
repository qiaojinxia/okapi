import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import { ErrorState } from '@/components/ui/state'
import { Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

export interface PickerModel {
  model_name: string
  vendor: string | null
  pricing_mode: string | null
}


/// 模型选择器：从**已配定价**的模型里勾选，避免手输拼错——模型名拼错的后果是
/// 请求直接 404 且不易排查。
///
/// 按供应商分组展示（vendor 由后端按模型名前缀自动归类）；未定价模型标红，
/// 因为建了渠道却没定价，请求同样会被拒。
export function ModelPicker({
  value,
  onChange,
}: {
  value: string[]
  onChange: (models: string[]) => void
}) {
  const { t } = useTranslation()
  const models = useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: PickerModel[] }>('/admin/models'),
  })

  const picked = new Set(value)
  const toggle = (name: string) => {
    const next = new Set(picked)
    if (next.has(name)) next.delete(name)
    else next.add(name)
    onChange([...next])
  }

  const groups = new Map<string, PickerModel[]>()
  for (const m of models.data?.data ?? []) {
    const key = m.vendor ?? ''
    const list = groups.get(key) ?? []
    list.push(m)
    groups.set(key, list)
  }
  // 未归类的排到末尾
  const ordered = [...groups.entries()].sort(([a], [b]) => {
    if (a === '') return 1
    if (b === '') return -1
    return a.localeCompare(b)
  })

  if (models.isError) {
    return <ErrorState message={describeError(models.error)} />
  }
  return (
    <div className="flex flex-col gap-2">
      <Label>{t('admin:pickModels')}</Label>
      <div className="flex max-h-56 flex-col gap-2 overflow-y-auto rounded-md border border-border p-2">
        {ordered.length === 0 ? (
          <span className="text-xs text-muted-foreground">{t('admin:pickModelsEmpty')}</span>
        ) : (
          ordered.map(([vendor, list]) => (
            <div key={vendor || 'other'} className="flex flex-col gap-1">
              <span className="text-xs font-medium text-muted-foreground">
                {vendor || t('admin:vendorOther')}
              </span>
              <div className="flex flex-wrap gap-x-4 gap-y-1">
                {list.map((m) => (
                  <span key={m.model_name} className="inline-flex items-center gap-1.5">
                    <Checkbox
                      label={m.model_name}
                      checked={picked.has(m.model_name)}
                      onChange={() => toggle(m.model_name)}
                      className="font-mono text-xs"
                    />
                    {m.pricing_mode === null && (
                      <Badge variant="destructive">{t('admin:unpriced')}</Badge>
                    )}
                  </span>
                ))}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  )
}
