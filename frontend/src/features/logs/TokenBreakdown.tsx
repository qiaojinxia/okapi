import { Layers } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { DetailSection, InfoGrid, InfoItem } from './detail-ui'
import type { InfoDot } from './detail-ui'
import { cacheRead, cacheWrite } from './types'
import type { TokenDetails } from './types'

/** Shared portal/admin display. Missing observations remain distinct from zero. */
export function TokenBreakdown({ usage: u, recorded }: { usage: TokenDetails; recorded?: boolean }) {
  const { t, i18n } = useTranslation()
  const value = (n: number | null | undefined) => n == null ? '—' : n.toLocaleString(i18n.language)
  const source = (name: string | null | undefined) => ['upstream', 'estimated', 'local_override'].includes(name ?? '') ? name! : 'unknown'
  // 来源徽章只在"需要留意"时醒目：本地估算 / 本地覆盖（计费数与上游实报不一致）用警示色；
  // 上游实报保持安静；来源未记录（老数据）不在每个字段上重复，统一在底部一行里交代。
  const tone = (name: string) => name === 'estimated' || name === 'local_override' ? 'warning' : 'muted'
  const legend: Record<string, InfoDot> = { input: 'info', cacheRead: 'success', output: 'primary' }
  const field = (key: string, n: number | null | undefined, origin?: string | null) => <InfoItem key={key} label={t(`logs:${key}`)} dot={legend[key]}>
    {value(n)}{origin !== undefined && source(origin) !== 'unknown' && <Badge className="ml-2 align-middle" variant={tone(source(origin))}>{t(`logs:source_${source(origin)}`)}</Badge>}
  </InfoItem>
  const sources = [...new Set([source(u.prompt_source), source(u.completion_source)])]
  const hasUpstream = u.upstream_usage?.prompt_tokens != null || u.upstream_usage?.completion_tokens != null
  // 输入拆成"未命中缓存 / 命中缓存"两段，再接输出：一眼看出缓存省了多少、输出占多大。
  const read = cacheRead({ usage: u })
  const cached = read != null && read >= 0 && read <= u.prompt_tokens ? read : 0
  const parts = [
    { key: 'input', n: u.prompt_tokens - cached, color: 'bg-info' },
    { key: 'cacheRead', n: cached, color: 'bg-success' },
    { key: 'output', n: u.completion_tokens, color: 'bg-primary' },
  ].filter((part) => Number.isFinite(part.n) && part.n > 0)
  const sum = parts.reduce((total, part) => total + part.n, 0)
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
  if (u.input_unit === 'characters') return <DetailSection icon={Layers} title={t('logs:tokenDetails')} hint={t('logs:characterHint')}>
    <InfoGrid cols={2}>{field('inputCharacters', u.input_characters)}</InfoGrid>
  </DetailSection>
  return <DetailSection icon={Layers} title={t('logs:tokenDetails')} hint={t('logs:tokenHint')}>
    {sum > 0 && <div data-slot="token-bar" aria-hidden className="flex h-2.5 overflow-hidden rounded-full bg-muted">
      {parts.map((part) => <span key={part.key} className={`${part.color} h-full first:rounded-l-full last:rounded-r-full`} style={{ width: `${part.n / sum * 100}%` }} />)}
    </div>}
    <InfoGrid cols={3}>
      {field('input', u.prompt_tokens, u.prompt_source ?? null)}
      {field('output', u.completion_tokens, u.completion_source ?? null)}
      {field('cacheRead', cacheRead({ usage: u }))}
      {field('cacheWrite', cacheWrite({ usage: u }))}
      {field('reasoning', u.reported_details?.reasoning === false ? null : u.reasoning_tokens)}
      {field('audioInput', u.reported_details?.prompt?.audio === false ? null : u.audio_prompt_tokens)}
      {field('imageInput', u.reported_details?.prompt?.image === false ? null : u.image_prompt_tokens)}
      {field('audioOutput', u.reported_details?.completion?.audio === false ? null : u.audio_completion_tokens)}
      {field('imageOutput', u.reported_details?.completion?.image === false ? null : u.image_completion_tokens)}
    </InfoGrid>
    {reportedSubsets.length > 0 && <div data-slot="cache-subsets" className="space-y-3 rounded-lg border border-border/70 bg-muted/25 p-3">
      <p className="text-xs font-semibold">{t('logs:cacheSubsets')}</p>
      <InfoGrid cols={3}>
        {reportedSubsets.map(([key, n]) => field(key, n))}
      </InfoGrid>
      <p className="text-xs leading-5 text-muted-foreground">{t('logs:cacheSubsetsHint')}</p>
    </div>}
    {hasUpstream
      ? <div className="space-y-3 rounded-lg border border-border/70 bg-muted/25 p-3">
        <p className="text-xs font-semibold">{t('logs:usageSource')}</p>
        <InfoGrid cols={2}>
          {field('upstreamInput', u.upstream_usage?.prompt_tokens)}
          {field('upstreamOutput', u.upstream_usage?.completion_tokens)}
        </InfoGrid>
        <p className="text-xs leading-5 text-muted-foreground">{t('logs:usageSourceHint')}</p>
      </div>
      // 没有上游原始数可对照时，整块只剩一行：用量来源 · 来源未记录 / 本地估算
      : <p data-slot="usage-source-note" className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
        <span className="font-semibold">{t('logs:usageSource')}</span><span aria-hidden>·</span>
        {sources.map((name) => <Badge key={name} variant={tone(name)}>{t(`logs:source_${name}`)}</Badge>)}
      </p>}
    {!recorded && <p className="text-xs text-muted-foreground">{t('logs:historicalHint')}</p>}
  </DetailSection>
}
