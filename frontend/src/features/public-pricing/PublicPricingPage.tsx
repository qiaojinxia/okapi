import { Link } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { PricingModel } from '@/features/public-pricing/types'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Simulator } from '@/features/public-pricing/Simulator'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

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



export function PublicPricingPage() {
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
          <Select
            id="unit"
            className="w-36"
            value={unit}
            onChange={(v) => setUnit(v as TokenUnit)}
            options={[
              { value: '1M', label: '1M tokens' },
              { value: '1K', label: '1K tokens' },
            ]}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="group">{t('pricing:viewAsGroup')}</Label>
          <Select
            id="group"
            className="w-56"
            value={group}
            onChange={setGroup}
            placeholder={t('pricing:baseGroup')}
            options={groups.map((g) => ({
              value: g.code,
              label: `${g.name ?? g.code} ×${g.ratio ?? '1'}`,
            }))}
          />
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
                <Th>{t('pricing:audioInPrice')}</Th>
                <Th>{t('pricing:audioOutPrice')}</Th>
                <Th>{t('pricing:imageInPrice')}</Th>
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
                // 模态倍率为 1 表示该模型不区分模态（纯文本模型），显示 — 而非重复文本价
                const audioRatio = Number(m.audio_ratio ?? '1')
                const audioIn =
                  audioRatio === 1
                    ? null
                    : unitPriceMicro(m.model_ratio, groupFactor * audioRatio, unit)
                const audioOut =
                  audioRatio === 1
                    ? null
                    : unitPriceMicro(
                        m.model_ratio,
                        groupFactor * audioRatio * Number(m.audio_completion_ratio ?? '1'),
                        unit,
                      )
                const imageRatio = Number(m.image_ratio ?? '1')
                const imageIn =
                  imageRatio === 1
                    ? null
                    : unitPriceMicro(m.model_ratio, groupFactor * imageRatio, unit)
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
                        <Td colSpan={8}>
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
                        <Td>{audioIn === null ? '—' : formatMoney(audioIn, locale)}</Td>
                        <Td>{audioOut === null ? '—' : formatMoney(audioOut, locale)}</Td>
                        <Td>{imageIn === null ? '—' : formatMoney(imageIn, locale)}</Td>
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
