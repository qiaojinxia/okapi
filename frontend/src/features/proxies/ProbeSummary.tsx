import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import type { ProbeResult } from './types'

/// 一次代理测试的结果：通 → 出口 IP / 国家 / 延迟；不通 → 失败类别 + 错误链摘要。
export function ProbeSummary({ result }: { result: ProbeResult }) {
  const { t } = useTranslation()
  if (result.ok) {
    return (
      <div role="status" className="flex flex-wrap items-center gap-2 text-xs">
        <Badge variant="success" dot>{t('admin:proxyProbeOk')}</Badge>
        <span className="font-mono">{result.exit_ip ?? t('admin:proxyExitUnknown')}</span>
        {result.country && <Badge variant="outline">{result.country}</Badge>}
        {result.latency_ms !== undefined && (
          <span className="text-muted-foreground tabular-nums">{result.latency_ms} ms</span>
        )}
        {result.status !== undefined && result.status !== 200 && (
          <span className="text-muted-foreground">HTTP {result.status}</span>
        )}
      </div>
    )
  }
  return (
    <div role="alert" className="flex flex-col gap-1 text-xs">
      <span className="flex items-center gap-2">
        <Badge variant="destructive" dot>{t('admin:proxyProbeFailed')}</Badge>
        <span>{t(`admin:proxyProbe_${result.error_code ?? 'probe_failed'}`, { defaultValue: result.error_code ?? '' })}</span>
      </span>
      {result.error && <span className="break-all font-mono text-muted-foreground">{result.error}</span>}
    </div>
  )
}
