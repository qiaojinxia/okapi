import { ScanSearch } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { DetailSection, InfoGrid, InfoItem } from './detail-ui'
import { duration } from './types'
import type { LogDiagnostics } from './types'

export function LogRequestDetails({ row }: { row: {
  diagnostics?: LogDiagnostics | null; requested_model?: string | null; model: string
  upstream_model?: string | null; request_type?: string
} }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const d = row.diagnostics
  const models = [row.requested_model, row.model, row.upstream_model, d?.response_model]
    .filter((value): value is string => Boolean(value)).filter((value, index, values) => index === 0 || value !== values[index - 1])
  const fields = [
    [t('logs:responseModel'), d?.response_model],
    [t('logs:reasoningEffort'), d?.reasoning_effort],
    [t('logs:userAgent'), d?.user_agent],
    [t('logs:clientSession'), d?.session_id],
    [t('logs:imageSize'), d?.media?.image_size],
    [t('logs:imageQuality'), d?.media?.image_quality],
    [t('logs:requestedImages'), d?.media?.requested_images?.toLocaleString(locale)],
    [t('logs:videoSize'), d?.media?.video_size],
    [t('logs:requestedVideoSeconds'), d?.media?.requested_video_seconds == null ? undefined : `${d.media.requested_video_seconds.toLocaleString(locale)} s`],
    [t('logs:requestType'), row.request_type ? t(`logs:requestType_${row.request_type}`, { defaultValue: row.request_type }) : undefined],
    [t('logs:streamEndReason'), d?.stream_end_reason ? t(`logs:streamEnd_${d.stream_end_reason}`, { defaultValue: d.stream_end_reason }) : undefined],
  ].filter(([, value]) => value)
  if (!fields.length && models.length < 2 && !d?.attempts?.length) return null
  const chain = models.join(' → ')
  return <DetailSection icon={ScanSearch} title={t('logs:requestDiagnostics')}>
    <InfoGrid cols={2}>
      {models.length > 1 && <InfoItem wide label={t('logs:modelChain')} copy={chain}>{chain}</InfoItem>}
      {fields.map(([label, content]) => <InfoItem key={label} label={label} copy={content!}>{content}</InfoItem>)}
    </InfoGrid>
    {d?.attempts?.length ? <details className="rounded-lg border border-border/70 bg-card p-3" open={d.attempts.length > 1 || d.attempts.some(a => a.outcome === 'failure')}>
      <summary className="cursor-pointer text-sm font-semibold">{t('logs:attempts')} · {d.attempts.length}</summary>
      <ol className="mt-3 space-y-3">
        {d.attempts.map((attempt, index) => <li key={index} className={`min-w-0 space-y-2 rounded-lg border-l-2 bg-muted/30 p-3 text-xs ${attempt.outcome === 'failure' ? 'border-l-destructive' : attempt.outcome === 'success' ? 'border-l-success' : 'border-l-border'}`}>
          <div className="flex flex-wrap items-center gap-2"><span className="font-medium">#{index + 1} · {attempt.provider || '—'} · {t('analytics:dimChannel')} #{attempt.channel_id}</span>
            <Badge variant={attempt.outcome === 'failure' ? 'destructive' : 'muted'}>{attempt.status || (attempt.outcome === 'failure' ? t('logs:failed') : attempt.outcome === 'success' ? t('common:success') : '—')}</Badge>
            <span className="ml-auto tabular-nums" title={t('logs:attemptDuration')}>{duration(attempt.duration_ms, locale)}</span>
          </div>
          <p className="break-all font-mono">{attempt.upstream_model} · {attempt.upstream_endpoint}</p>
          <p className="text-muted-foreground">{t('admin:logsChannelKey')} #{attempt.channel_key_id}</p>
          {attempt.error_code && <p className="break-all font-mono text-destructive">{attempt.error_code}</p>}
          {attempt.error_message && <p className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{attempt.error_message}</p>}
        </li>)}
      </ol>
      {d.attempts_truncated && <p className="mt-3 text-xs text-muted-foreground">{t('logs:attemptsTruncated')}</p>}
    </details> : null}
  </DetailSection>
}
