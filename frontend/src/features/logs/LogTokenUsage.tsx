import { ArrowDown, ArrowUp, Database, Zap, ZapOff } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Tooltip } from '@/components/ui/tooltip'
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
  const readHint = hit ? [t('logs:cacheHitHint', { n: number(read) }), share && t('logs:cacheInputShare', { percent: share })].filter(Boolean).join(' ')
    : read === 0 ? t('logs:cacheMissHint') : t('logs:cacheReadMissing', { state: missing })
  const writeHint = write === null ? t('logs:cacheWriteMissing', { state: missing }) : t('logs:cacheWriteHint', { n: number(write) })
  const chip = 'inline-flex h-4 items-center gap-1 rounded border px-1 text-[11px] outline-none focus-visible:ring-2 focus-visible:ring-primary/40'
  const neutral = 'border-transparent bg-muted/60 text-muted-foreground'
  const unknown = 'border-dashed border-border bg-transparent text-muted-foreground'

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
      <Tooltip content={readHint}>
        <span data-slot="cache-read" data-state={hit ? 'hit' : read === 0 ? 'empty' : 'missing'} tabIndex={0}
          aria-label={`${t('logs:cacheRead')} ${read === null ? missing : number(read)}`}
          className={cn(chip, hit ? 'border-transparent bg-success/12 font-medium text-[color-mix(in_oklab,var(--success)_75%,var(--fg))] dark:text-success' : read === null ? unknown : neutral)}>
          {read === 0 ? <ZapOff aria-hidden className="h-3 w-3 shrink-0" /> : <Zap aria-hidden className="h-3 w-3 shrink-0" />}
          <span>{read === null ? '—' : number(read)}</span>
          {hit && share && <span className="ml-0.5 border-l border-current/20 pl-1" aria-label={t('logs:cacheInputShare', { percent: share })}>{share}</span>}
        </span>
      </Tooltip>
      <Tooltip content={writeHint}>
        <span data-slot="cache-write" data-state={writing ? 'write' : write === 0 ? 'empty' : 'missing'} tabIndex={0}
          aria-label={`${t('logs:cacheWrite')} ${write === null ? missing : number(write)}`}
          className={cn(chip, writing ? 'border-transparent bg-warning/12 text-[color-mix(in_oklab,var(--warning)_65%,var(--fg))] dark:text-warning' : write === null ? unknown : neutral)}>
          <Database aria-hidden className="h-3 w-3 shrink-0" />
          <span>{write === null ? '—' : number(write)}</span>
        </span>
      </Tooltip>
    </div>
  </div>
}
