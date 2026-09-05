import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import type { BreakdownRow, BreakdownTotal } from '@/features/portal-overview/types'
import { sumByModel } from '@/features/portal-overview/types'
import { formatBp, formatCount } from '@/lib/money'

interface Segment {
  key: 'input' | 'cached' | 'write' | 'output' | 'reasoning'
  value: number
  className: string
}

/// 把 OpenAI 口径的四个 usage 字段拆成互斥四段：
/// cached ⊂ prompt、reasoning ⊂ completion，直接画会把缓存和推理各算两遍。
function segments(t: {
  prompt_tokens: number
  cached_tokens: number
  cache_write_tokens?: number | null
  completion_tokens: number
  reasoning_tokens: number
}): Segment[] {
  const cached = Math.min(t.cached_tokens, t.prompt_tokens)
  const reasoning = Math.min(t.reasoning_tokens, t.completion_tokens)
  const writes = Math.min(t.cache_write_tokens ?? 0, t.prompt_tokens - cached)
  return [
    { key: 'input', value: t.prompt_tokens - cached - writes, className: 'bg-primary' },
    { key: 'cached', value: cached, className: 'bg-success' },
    { key: 'write', value: writes, className: 'bg-chart-5' },
    { key: 'output', value: t.completion_tokens - reasoning, className: 'bg-warning' },
    { key: 'reasoning', value: reasoning, className: 'bg-muted-foreground' },
  ]
}

/// Token 构成（Sub2API 强项的吸收：input / cache read / output / reasoning 四段）。
///
/// 对编码智能体用户这是账单里最该看懂的一张图：缓存命中的那一段按 cache_ratio
/// （常为 0.1×）计价，命中率掉下来账单立刻翻倍——比总量涨跌更能解释"为什么这周贵"。
export function TokenMixView({
  rows,
  total,
}: {
  rows: BreakdownRow[]
  total: BreakdownTotal | null
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  if (total === null || total.tokens === 0) {
    return (
      <Card>
        <CardContent>
          <EmptyState hint={t('portal:emptyUsageHint')} />
        </CardContent>
      </Card>
    )
  }
  const segs = segments(total)
  const sum = segs.reduce((s, x) => s + x.value, 0)
  const label: Record<Segment['key'], string> = {
    input: t('portal:tokInput'),
    cached: t('portal:tokCached'),
    write: t('charts:cacheWrite'),
    output: t('portal:tokOutput'),
    reasoning: t('portal:tokReasoning'),
  }
  const models = [...sumByModel(rows).values()].sort((a, b) => b.prompt_tokens + b.completion_tokens - (a.prompt_tokens + a.completion_tokens))

  return (
    <Card>
      <CardContent className="flex flex-col gap-4 pt-4">
        {/* 一根横向堆叠条：四段占比一眼可读；段太窄（<1%）不画文字只留色块 */}
        <div className="flex h-4 w-full overflow-hidden rounded bg-muted">
          {segs
            .filter((s) => s.value > 0)
            .map((s) => (
              <div
                key={s.key}
                className={s.className}
                style={{ width: `${(s.value / sum) * 100}%` }}
                title={`${label[s.key]} ${formatCount(s.value, locale)}`}
              />
            ))}
        </div>
        <div className="flex flex-wrap gap-x-6 gap-y-2 text-xs">
          {segs.map((s) => (
            <span key={s.key} className="inline-flex items-center gap-1.5">
              <span className={`inline-block h-2.5 w-2.5 rounded-sm ${s.className}`} />
              <span className="text-muted-foreground">{label[s.key]}</span>
              <span className="font-medium">{s.key === 'write' && total.cache_write_tokens == null ? '—' : formatCount(s.value, locale)}</span>
              <span className="text-muted-foreground">
                {s.key === 'write' && total.cache_write_tokens == null ? '' : formatBp(sum > 0 ? Math.round((s.value * 10_000) / sum) : 0, locale)}
              </span>
            </span>
          ))}
        </div>
        <p className="text-xs text-muted-foreground">
          {t('portal:tokMixHint', { hit: formatBp(total.cache_hit_bp, locale) })}
        </p>
        {total.cache_write_tokens == null && <p className="rounded-lg bg-muted/60 px-3 py-2 text-xs text-muted-foreground">{t('charts:missingCacheWrite')}</p>}

        <Table>
          <THead>
            <Tr>
              <Th>{t('pricing:model')}</Th>
              <Th>{t('portal:tokInput')}</Th>
              <Th>{t('portal:tokCached')}</Th>
              <Th>{t('charts:cacheWrite')}</Th>
              <Th>{t('portal:tokOutput')}</Th>
              <Th>{t('portal:tokReasoning')}</Th>
              <Th>{t('portal:cacheHitShort')}</Th>
            </Tr>
          </THead>
          <TBody>
            {models.map((m) => {
              const s = segments(m)
              const hitBp =
                m.prompt_tokens > 0 ? Math.round((m.cached_tokens * 10_000) / m.prompt_tokens) : 0
              return (
                <Tr key={m.model}>
                  <Td className="font-mono text-xs">{m.model}</Td>
                  {s.map((x) => (
                    <Td key={x.key} className="text-xs">
                      {x.key === 'write' && m.cache_write_tokens == null ? '—' : formatCount(x.value, locale)}
                    </Td>
                  ))}
                  <Td className="text-xs">{formatBp(hitBp, locale)}</Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      </CardContent>
    </Card>
  )
}
