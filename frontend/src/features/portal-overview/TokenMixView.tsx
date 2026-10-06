import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import type { BreakdownRow, BreakdownTotal } from '@/features/portal-overview/types'
import { sumByModel } from '@/features/portal-overview/types'
import { formatBp, formatCount } from '@/lib/money'
import { cacheAmount, cacheHit } from './cache-metrics'
import { CacheTokenValue } from './CacheTokenValue'

export interface Segment {
  key: 'input' | 'cached' | 'write' | 'output' | 'reasoning'
  value: number
  className: string
}

/// 把 OpenAI 口径的四个 usage 字段拆成互斥四段：
/// cached ⊂ prompt、reasoning ⊂ completion，直接画会把缓存和推理各算两遍。
export function segments(t: {
  prompt_tokens: number
  cached_tokens: number
  cache_write_tokens?: number | null
  recorded_cache_write_tokens?: number | null
  completion_tokens: number
  reasoning_tokens: number
}): Segment[] {
  const cached = Math.min(t.cached_tokens, t.prompt_tokens)
  const reasoning = Math.min(t.reasoning_tokens, t.completion_tokens)
  const writes = Math.min(t.recorded_cache_write_tokens ?? t.cache_write_tokens ?? 0, t.prompt_tokens - cached)
  return [
    { key: 'input', value: t.prompt_tokens - cached - writes, className: 'bg-primary' },
    { key: 'cached', value: cached, className: 'bg-success' },
    { key: 'write', value: writes, className: 'bg-chart-5' },
    { key: 'output', value: t.completion_tokens - reasoning, className: 'bg-warning' },
    { key: 'reasoning', value: reasoning, className: 'bg-muted-foreground' },
  ]
}

/// 按已记录的用量拆分；采集不完整时不把缺失值解释成零命中。
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
  const hit = cacheHit(total)
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
              <span className="font-medium">{s.key === 'write' || s.key === 'cached'
                ? <CacheTokenValue value={cacheAmount(total, s.key === 'cached' ? 'read' : 'write')} /> : formatCount(s.value, locale)}</span>
              <span className="text-muted-foreground">
                {(s.key === 'write' || s.key === 'cached') && cacheAmount(total, s.key === 'cached' ? 'read' : 'write').tokens == null ? '' : formatBp(sum > 0 ? Math.round((s.value * 10_000) / sum) : 0, locale)}
              </span>
            </span>
          ))}
        </div>
        <p className="text-xs text-muted-foreground">
          {t(hit.partial ? 'portal:tokMixMeasuredHint' : 'portal:tokMixHint', { hit: hit.bp == null ? '—' : formatBp(hit.bp, locale) })}
        </p>
        {hit.partial && <p className="text-xs text-muted-foreground">{t('portal:cacheHitMeasured', { v: formatBp(hit.bp, locale), n: hit.samples, total: total.requests })}</p>}
        {total.cache_write_tokens == null && <p className="rounded-lg bg-muted/60 px-3 py-2 text-xs text-muted-foreground">{t('charts:missingCacheWrite')}</p>}
        {total.cache_hit_bp == null && <p className="rounded-lg bg-muted/60 px-3 py-2 text-xs text-muted-foreground">{t('portal:cacheIncomplete')}</p>}
        <p className="text-xs text-muted-foreground">{t('portal:cacheCoverage', { read: total.cache_read_known_requests ?? 0, write: total.cache_write_known_requests ?? 0, total: total.requests })}</p>

        <Table stickyHeader stickyFirstColumn aria-label={t('portal:viewTokens')} wrapperClassName="max-h-[max(12rem,calc(100dvh-30rem))]">
          <THead>
            <Tr>
              <Th>{t('pricing:model')}</Th>
              <Th numeric>{t('portal:tokInput')}</Th>
              <Th numeric>{t('portal:tokCached')}</Th>
              <Th numeric>{t('charts:cacheWrite')}</Th>
              <Th numeric>{t('portal:tokOutput')}</Th>
              <Th numeric>{t('portal:tokReasoning')}</Th>
              <Th numeric>{t('portal:cacheHitShort')}</Th>
            </Tr>
          </THead>
          <TBody>
            {models.map((m) => {
              const s = segments(m)
              const modelHit = cacheHit(m)
              return (
                <Tr key={m.model}>
                  <Td className="w-px font-mono text-xs"><span className="block w-28 break-all sm:w-44">{m.model}</span></Td>
                  {s.map((x) => (
                    <Td key={x.key} numeric className="whitespace-nowrap text-xs">
                      {x.key === 'write' || x.key === 'cached'
                        ? <CacheTokenValue value={cacheAmount(m, x.key === 'cached' ? 'read' : 'write')} /> : formatCount(x.value, locale)}
                    </Td>
                  ))}
                  <Td numeric className="text-xs">{modelHit.bp == null ? '—' : modelHit.partial
                    ? t('portal:cacheHitMeasured', { v: formatBp(modelHit.bp, locale), n: modelHit.samples, total: m.requests })
                    : formatBp(modelHit.bp, locale)}</Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      </CardContent>
    </Card>
  )
}
