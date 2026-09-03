import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Area, AreaChart, ResponsiveContainer, Tooltip, YAxis } from 'recharts'
import { Card, CardContent } from '@/components/ui/card'
import { InlineStat } from '@/components/ui/stat'
import { apiFetch } from '@/lib/api'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface RealtimePoint {
  ts: number
  requests: number
  tokens: number
  errors: number
  amount_micro: number
}

interface RealtimeResp {
  window_secs: number
  qps_milli: number
  requests: number
  errors: number
  error_rate_bp: number
  tokens: number
  amount_micro: number
  series: RealtimePoint[]
}

/// 实时条：秒级 QPS + 最近 60s 的请求/错误/token/金额（Redis 秒桶，5s 轮询）。
///
/// 与下方 KPI 卡的分工：KPI 是"今天怎么样"（CH 日聚合，分钟级新鲜度），
/// 这条是"此刻怎么样"（发新价、封渠道、被刷量的那一分钟看这里）。
/// 数据源只依赖 Redis，CH 未启用时本条照常工作——所以它不进 KpiCards 而是独立卡。
export function RealtimeCard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsRealtime,
    queryFn: () => apiFetch<RealtimeResp>('/admin/stats/realtime?window=60'),
    refetchInterval: 5_000,
    retry: false,
  })

  if (q.isError) {
    // 实时条是增益信息，拿不到就收起，不占版面报错（KPI 卡会报大错误）
    return null
  }
  const d = q.data
  const qps = d ? (d.qps_milli / 1_000).toLocaleString(locale, { maximumFractionDigits: 1 }) : '—'

  return (
    <Card>
      <CardContent className="flex flex-wrap items-center gap-x-8 gap-y-3 px-5 py-3">
        <div className="flex items-center gap-2 self-stretch border-r border-border pr-6">
          {/* 呼吸点：让"实时"可感知——数字不动时用户会怀疑页面死了 */}
          <span className="relative flex h-2 w-2">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-success opacity-60" />
            <span className="relative inline-flex h-2 w-2 rounded-full bg-success" />
          </span>
          <span className="text-xs font-semibold text-muted-foreground">
            {t('admin:realtimeTitle')}
          </span>
        </div>
        <InlineStat label="QPS" value={qps} />
        <InlineStat label={t('admin:realtimeReqs60')} value={d ? formatCount(d.requests, locale) : '—'} />
        <InlineStat
          label={t('admin:kpiErrorRate')}
          value={d ? formatBp(d.error_rate_bp, locale) : '—'}
          tone={d && d.error_rate_bp >= 500 ? 'bad' : d && d.error_rate_bp >= 100 ? 'warn' : 'default'}
        />
        <InlineStat label={t('admin:kpiTokens')} value={d ? formatCount(d.tokens, locale) : '—'} />
        <InlineStat
          label={t('admin:kpiRevenue')}
          value={d ? formatMoneyAggregate(d.amount_micro, locale) : '—'}
        />
        <div className="h-10 min-w-40 flex-1">
          {d && d.series.some((p) => p.requests > 0) && (
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={d.series} margin={{ top: 2, bottom: 2, left: 0, right: 0 }}>
                <defs>
                  <linearGradient id="g-rt" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor="var(--color-primary)" stopOpacity={0.4} />
                    <stop offset="100%" stopColor="var(--color-primary)" stopOpacity={0.02} />
                  </linearGradient>
                </defs>
                {/* 定死下界 0：sparkline 自动缩放会把 1→2 的波动画成暴涨 */}
                <YAxis hide domain={[0, 'auto']} />
                <Tooltip
                  formatter={(value) => [String(value), t('common:requests')]}
                  labelFormatter={() => ''}
                />
                <Area
                  type="monotone"
                  dataKey="requests"
                  stroke="var(--color-primary)"
                  fill="url(#g-rt)"
                  isAnimationActive={false}
                />
              </AreaChart>
            </ResponsiveContainer>
          )}
        </div>
      </CardContent>
    </Card>
  )
}
