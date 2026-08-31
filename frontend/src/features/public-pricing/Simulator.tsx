import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { PricingModel } from '@/features/public-pricing/types'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { formatMoney, simulateChargeMicro } from '@/lib/money'

/// 定价模拟器（展示层估算，权威语义在后端计费引擎与账单快照）。
export function Simulator({ models }: { models: PricingModel[] }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const ratioModels = models.filter((m) => m.mode !== 'per_call')
  const [model, setModel] = useState('')
  const [form, setForm] = useState({
    prompt: '1000',
    cached: '0',
    cacheWrite: '0',
    completion: '500',
    group: '1',
  })
  const selected = ratioModels.find((m) => m.model === model) ?? ratioModels[0]

  const estimate = selected
    ? simulateChargeMicro({
        modelRatio: Number(selected.model_ratio ?? '1'),
        completionRatio: Number(selected.completion_ratio ?? '1'),
        cacheRatio: Number(selected.cache_ratio ?? '1'),
        cacheWriteRatio: Number(selected.cache_write_ratio ?? '1'),
        groupRatio: Number(form.group) || 1,
        promptTokens: Number(form.prompt) || 0,
        cachedTokens: Number(form.cached) || 0,
        cacheWriteTokens: Number(form.cacheWrite) || 0,
        completionTokens: Number(form.completion) || 0,
      })
    : 0

  const fields = [
    ['prompt', t('pricing:simPrompt')],
    ['cached', t('pricing:simCached')],
    ['cacheWrite', t('pricing:simCacheWrite')],
    ['completion', t('pricing:simCompletion')],
    ['group', t('pricing:simGroup')],
  ] as const

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('pricing:simulator')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="sim-model">{t('pricing:model')}</Label>
            <Select
              id="sim-model"
              value={selected?.model ?? ''}
              onChange={setModel}
              options={ratioModels.map((m) => ({ value: m.model, label: m.model }))}
            />
          </div>
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={`sim-${field}`}>{label}</Label>
              <Input
                id={`sim-${field}`}
                inputMode="numeric"
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
        </div>
        <div className="flex items-baseline gap-3">
          <span className="text-sm text-muted-foreground">{t('pricing:simResult')}</span>
          <span className="text-xl font-bold">{formatMoney(estimate, locale)}</span>
          <span className="text-xs text-muted-foreground">{t('pricing:simNote')}</span>
        </div>
      </CardContent>
    </Card>
  )
}
