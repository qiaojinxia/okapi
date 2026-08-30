import { useQuery } from '@tanstack/react-query'
import { Link, createFileRoute } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney, simulateChargeMicro } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { useState } from 'react'

export const Route = createFileRoute('/pricing')({
  component: PricingPage,
})

interface PricingModel {
  model: string
  display_name: string | null
  vendor: string | null
  mode: string
  model_ratio: string | null
  completion_ratio: string | null
  cache_ratio: string | null
  cache_write_ratio: string | null
  per_call_price_micro: number | null
}

type TokenUnit = '1K' | '1M'

interface PricingGroup {
  code: string
  name: string | null
  ratio: string | null
}

/// 倍率 → 每单位 tokens 美元价（基准 $2/1M per 倍率 1.0，DESIGN §3；与 new-api
/// `model_ratio × 2 × groupRatio` 同口径）。
function unitPriceMicro(
  ratio: string | null,
  factor = 1,
  unit: TokenUnit = '1M',
): number | null {
  if (ratio === null) return null
  const r = Number(ratio)
  if (Number.isNaN(r)) return null
  const perMillion = r * factor * 2_000_000
  return unit === '1M' ? perMillion : perMillion / 1000
}

function PricingPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [unit, setUnit] = useState<TokenUnit>('1M')
  const [group, setGroup] = useState('')
  const pricing = useQuery({
    queryKey: qk.publicPricing,
    queryFn: () =>
      apiFetch<{ models: PricingModel[]; groups: PricingGroup[] }>('/api/pricing'),
  })
  const groups = pricing.data?.groups ?? []
  // 选中分组的倍率一并折进展示价：用户看到的就是自己那档的实价
  const groupFactor = Number(groups.find((g) => g.code === group)?.ratio ?? '1') || 1

  return (
    <div className="mx-auto flex max-w-4xl flex-col gap-4 p-6">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-bold text-primary">{t('pricing:title')}</h1>
        <Link to="/" className="text-sm text-muted-foreground hover:text-foreground">
          {t('common:login')}
        </Link>
      </div>
      <p className="text-sm text-muted-foreground">{t('pricing:baseNote')}</p>

      <div className="flex flex-wrap items-end gap-3">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="unit">{t('pricing:tokenUnit')}</Label>
          <select
            id="unit"
            className="h-9 rounded-md border border-input bg-card px-2 text-sm"
            value={unit}
            onChange={(e) => setUnit(e.target.value as TokenUnit)}
          >
            <option value="1M">1M tokens</option>
            <option value="1K">1K tokens</option>
          </select>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="group">{t('pricing:viewAsGroup')}</Label>
          <select
            id="group"
            className="h-9 rounded-md border border-input bg-card px-2 text-sm"
            value={group}
            onChange={(e) => setGroup(e.target.value)}
          >
            <option value="">{t('pricing:baseGroup')}</option>
            {groups.map((g) => (
              <option key={g.code} value={g.code}>
                {g.name ?? g.code} ×{g.ratio ?? '1'}
              </option>
            ))}
          </select>
        </div>
      </div>

      {pricing.isError ? (
        <p className="text-sm text-destructive">{describeError(pricing.error)}</p>
      ) : (
        <>
          <Table>
            <THead>
              <Tr>
                <Th>{t('pricing:model')}</Th>
                <Th>{t('pricing:ratio')}</Th>
                <Th>{t('pricing:promptPrice')}</Th>
                <Th>{t('pricing:completionPrice')}</Th>
                <Th>{t('pricing:cachedPrice')}</Th>
                <Th>{t('pricing:cacheWritePrice')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(pricing.data?.models ?? []).map((m) => {
                const prompt = unitPriceMicro(m.model_ratio, groupFactor, unit)
                const completion = unitPriceMicro(
                  m.model_ratio,
                  groupFactor * Number(m.completion_ratio ?? '1'),
                  unit,
                )
                const cached = unitPriceMicro(
                  m.model_ratio,
                  groupFactor * Number(m.cache_ratio ?? '1'),
                  unit,
                )
                // 写入倍率为 1 时该模型不区分缓存写入（OpenAI 隐式缓存），显示 —
                const cacheWriteRatio = Number(m.cache_write_ratio ?? '1')
                const cacheWrite =
                  cacheWriteRatio === 1
                    ? null
                    : unitPriceMicro(m.model_ratio, groupFactor * cacheWriteRatio, unit)
                return (
                  <Tr key={m.model}>
                    <Td>
                      <span className="font-mono text-xs">{m.model}</span>
                      {m.vendor && (
                        <Badge variant="muted" className="ml-2">
                          {m.vendor}
                        </Badge>
                      )}
                    </Td>
                    {m.mode === 'per_call' ? (
                      <>
                        <Td colSpan={5}>
                          {t('pricing:perCall', {
                            price: formatMoney(m.per_call_price_micro ?? 0, locale),
                          })}
                        </Td>
                      </>
                    ) : (
                      <>
                        <Td>{m.model_ratio ?? '—'}</Td>
                        <Td>{prompt === null ? '—' : formatMoney(prompt, locale)}</Td>
                        <Td>{completion === null ? '—' : formatMoney(completion, locale)}</Td>
                        <Td>{cached === null ? '—' : formatMoney(cached, locale)}</Td>
                        <Td>
                          {cacheWrite === null ? '—' : formatMoney(cacheWrite, locale)}
                        </Td>
                      </>
                    )}
                  </Tr>
                )
              })}
            </TBody>
          </Table>

          <Card>
            <CardHeader>
              <CardTitle>{t('pricing:groups')}</CardTitle>
            </CardHeader>
            <CardContent className="flex flex-wrap gap-2">
              {groups.map((g) => (
                <Badge key={g.code}>
                  {g.name ?? g.code} ×{g.ratio ?? '1'}
                </Badge>
              ))}
            </CardContent>
          </Card>

          <Simulator models={pricing.data?.models ?? []} />
        </>
      )}
    </div>
  )
}

/// 定价模拟器（展示层估算，权威语义在后端计费引擎与账单快照）。
function Simulator({ models }: { models: PricingModel[] }) {
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
            <select
              id="sim-model"
              className="h-9 rounded-md border border-input bg-card px-2 text-sm"
              value={selected?.model ?? ''}
              onChange={(e) => setModel(e.target.value)}
            >
              {ratioModels.map((m) => (
                <option key={m.model} value={m.model}>
                  {m.model}
                </option>
              ))}
            </select>
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
