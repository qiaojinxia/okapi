import { CircleAlert } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { CopyButton } from '@/components/ui/copy-button'
import type { LogDiagnostics } from './types'

const hints: Record<string, string> = {
  no_available_channel: 'logs:errorNoChannel',
  unsupported_endpoint: 'logs:errorEndpoint',
  insufficient_quota: 'logs:errorQuota',
  rate_limited: 'logs:errorRateLimit',
  batch_failed: 'logs:errorBatchFailed',
  batch_partial: 'logs:errorBatchPartial',
}

export function LogErrorDetails({ failed, code, upstreamStatus, diagnostics }: {
  failed: boolean; code: string | null; upstreamStatus?: number; diagnostics?: LogDiagnostics | null
}) {
  const { t, i18n } = useTranslation()
  if (!failed && !code) return null
  const errorKey = `errors:${code}`
  const description = !code ? t('logs:errorCodeMissing')
    : hints[code] ? t(hints[code])
      : i18n.exists(errorKey) ? t(errorKey, { param: '' }) : t('logs:failureHint')
  return <section aria-label={t('logs:errorDetails')} className="min-w-0 space-y-3 overflow-hidden rounded-xl border border-destructive/25 border-l-4 border-l-destructive bg-destructive/5 p-4">
    <h3 className="flex items-center gap-2 text-sm font-semibold text-destructive"><CircleAlert aria-hidden className="h-4 w-4 shrink-0" />{t('logs:errorDetails')}</h3>
    <p className="whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">{diagnostics?.error_message || description}</p>
    {diagnostics?.error_phase && <p className="text-xs text-muted-foreground">{t('logs:errorPhase')} · {t(`logs:phase_${diagnostics.error_phase}`, { defaultValue: diagnostics.error_phase })}</p>}
    <dl className="grid gap-3 sm:grid-cols-2">
      <div className="min-w-0 space-y-1">
        <dt className="text-xs text-muted-foreground">{t('admin:logsErrorCode')}</dt>
        <dd className="flex min-w-0 items-start gap-2">
          <span className="min-w-0 flex-1 whitespace-pre-wrap break-all font-mono text-xs leading-6">{code || '—'}</span>
          {code && <CopyButton value={code} size="xs" />}
        </dd>
      </div>
      {upstreamStatus != null && upstreamStatus > 0 && <div className="space-y-1">
        <dt className="text-xs text-muted-foreground">{t('admin:logsUpstreamStatus')}</dt>
        <dd className="font-mono text-xs leading-6">{upstreamStatus}</dd>
      </div>}
    </dl>
    <p className="text-xs text-muted-foreground">{t('logs:supportHint')}</p>
  </section>
}
