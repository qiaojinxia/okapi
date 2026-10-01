import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { FieldGroup } from '@/components/ui/drawer'
import { cacheRead, cacheWrite } from './types'
import type { TokenDetails } from './types'

/** Shared portal/admin display. Missing observations remain distinct from zero. */
export function TokenBreakdown({ usage: u, recorded }: { usage: TokenDetails; recorded?: boolean }) {
  const { t, i18n } = useTranslation()
  const value = (n: number | null | undefined) => n == null ? t(recorded ? 'logs:unreported' : 'logs:notRecorded') : n.toLocaleString(i18n.language)
  const source = (name: string | null | undefined) => ['upstream', 'estimated', 'local_override'].includes(name ?? '') ? name! : 'unknown'
  const field = (key: string, n: number | null | undefined, origin?: string | null) => <div key={key} className="min-w-0 space-y-1">
    <dt className="text-xs text-muted-foreground">{t(`logs:${key}`)}</dt>
    <dd className="flex flex-wrap items-center gap-2 text-sm tabular-nums">{value(n)}{origin !== undefined && <Badge variant={source(origin) === 'upstream' ? 'success' : 'muted'}>{t(`logs:source_${source(origin)}`)}</Badge>}</dd>
  </div>
  const ttlKnown = u.cache_write_5m_tokens != null && u.cache_write_1h_tokens != null
  const cacheSubsets: [string, number | null | undefined][] = [
    ['cacheWrite5m', ttlKnown ? u.cache_write_5m_tokens : null],
    ['cacheWrite1h', ttlKnown ? u.cache_write_1h_tokens : null],
    ['cacheReadAudio', u.cache_read_modalities?.audio_tokens],
    ['cacheReadImage', u.cache_read_modalities?.image_tokens],
    ['cacheWriteAudio', u.cache_write_modalities?.audio_tokens],
    ['cacheWriteImage', u.cache_write_modalities?.image_tokens],
  ]
  // Explicit zero is an observation; absent fields are not model capabilities.
  const reportedSubsets = cacheSubsets.filter(([, n]) => n != null)
  return <FieldGroup title={t('logs:tokenDetails')} hint={t('logs:tokenHint')}>
    <dl className="grid grid-cols-2 gap-x-5 gap-y-3">
      {field('input', u.prompt_tokens, u.prompt_source ?? null)}
      {field('output', u.completion_tokens, u.completion_source ?? null)}
      {field('cacheRead', cacheRead({ usage: u }))}
      {field('cacheWrite', cacheWrite({ usage: u }))}
      {field('reasoning', u.reasoning_tokens)}
      {field('audioInput', u.audio_prompt_tokens)}
      {field('imageInput', u.image_prompt_tokens)}
      {field('audioOutput', u.audio_completion_tokens)}
      {field('imageOutput', u.image_completion_tokens)}
    </dl>
    {reportedSubsets.length > 0 && <div data-slot="cache-subsets" className="space-y-3 rounded-lg border border-border bg-muted/20 p-3">
      <p className="text-xs font-medium">{t('logs:cacheSubsets')}</p>
      <dl className="grid grid-cols-2 gap-x-5 gap-y-3">
        {reportedSubsets.map(([key, n]) => field(key, n))}
      </dl>
      <p className="text-xs leading-5 text-muted-foreground">{t('logs:cacheSubsetsHint')}</p>
    </div>}
    <div className="space-y-2 rounded-lg bg-muted/30 p-3">
      <p className="text-xs font-medium">{t('logs:usageSource')}</p>
      <dl className="grid grid-cols-2 gap-3">
        {field('upstreamInput', u.upstream_usage?.prompt_tokens)}
        {field('upstreamOutput', u.upstream_usage?.completion_tokens)}
      </dl>
      <p className="text-xs leading-5 text-muted-foreground">{t('logs:usageSourceHint')}</p>
    </div>
    {!recorded && <p className="text-xs text-muted-foreground">{t('logs:historicalHint')}</p>}
  </FieldGroup>
}
