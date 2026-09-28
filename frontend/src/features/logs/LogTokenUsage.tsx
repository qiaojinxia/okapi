import { ArrowDown, ArrowUp, Database, Zap } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'
import { cacheRead, cacheReadShare, cacheWrite } from './types'
import type { LogRow } from './types'

/** Cache quantities are input subsets, not additional tokens or guaranteed savings. */
export function LogTokenUsage({ row }: { row: LogRow }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const read = cacheRead(row), write = cacheWrite(row)
  const hit = read !== null && read > 0, writing = write !== null && write > 0
  const share = cacheReadShare(row, locale)
  const missing = t(row.usage_details_recorded ? 'logs:unreported' : 'logs:notRecorded')
  const number = (value: number) => value.toLocaleString(locale)

  return <div data-slot="log-token-usage" className="grid w-full min-w-72 gap-0.5 text-left text-xs leading-4 tabular-nums whitespace-nowrap">
    <div className="grid grid-cols-2 items-center gap-4">
      <span data-slot="token-input" className="inline-flex items-center gap-1.5">
        <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded bg-info/10 text-info"><ArrowDown aria-hidden className="h-3 w-3" /></span>
        <span className="text-muted-foreground">{t('logs:inputShort')}</span>
        <span className="font-semibold text-foreground">{number(row.usage.prompt_tokens)}</span>
      </span>
      <span data-slot="token-output" className="inline-flex items-center gap-1.5">
        <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded bg-primary/10 text-primary"><ArrowUp aria-hidden className="h-3 w-3" /></span>
        <span className="text-muted-foreground">{t('logs:outputShort')}</span>
        <span className="font-semibold text-foreground">{number(row.usage.completion_tokens)}</span>
      </span>
    </div>
    <div data-slot="token-cache" className="flex items-center gap-1.5">
      <span className={cn('inline-flex items-center gap-1 rounded px-1.5', hit ? 'bg-success/12 font-medium text-[color-mix(in_oklab,var(--success)_75%,var(--fg))] dark:text-success' : 'bg-muted/70 text-muted-foreground')}
        title={hit ? t('logs:cacheHitHint', { n: number(read) }) : read === 0 ? t('logs:cacheMissHint') : t('logs:cacheReadMissing', { state: missing })}>
        {hit ? <>
          <Zap aria-hidden className="h-3 w-3" />
          {t('logs:cacheHit')} <span>{number(read)}</span>
          {share && <span className="ml-0.5 border-l border-current/20 pl-1.5" aria-label={t('logs:cacheInputShare', { percent: share })} title={t('logs:cacheInputShare', { percent: share })}>{share}</span>}
        </> : read === 0 ? t('logs:cacheMiss') : <>{t('logs:readShort')} {missing}</>}
      </span>
      <span className={cn('inline-flex items-center gap-1 rounded px-1.5', writing ? 'bg-warning/12 text-[color-mix(in_oklab,var(--warning)_65%,var(--fg))] dark:text-warning' : 'bg-muted/70 text-muted-foreground')}
        title={write === null ? t('logs:cacheWriteMissing', { state: missing }) : t('logs:cacheWriteHint', { n: number(write) })}>
        {writing && <Database aria-hidden className="h-3 w-3" />}
        {t('logs:writeShort')} <span>{write === null ? missing : number(write)}</span>
      </span>
    </div>
  </div>
}
