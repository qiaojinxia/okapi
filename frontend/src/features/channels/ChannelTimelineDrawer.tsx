import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { BarChart3, ScrollText } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  Bar,
  BarChart,
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import type { TimelinePoint, TimelineResp } from '@/features/analytics/types'
import { BAD_BP, WARN_BP } from '@/features/stats/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount } from '@/lib/money'
import { qk } from '@/lib/query-keys'

const RANGES = [6, 24, 168] as const
type Range = (typeof RANGES)[number]

/// 补齐时间轴上的空桶并统一粒度：≤24h 保持 5 分钟（72 / 288 个点），7 天并成小时
/// （168 个点——2016 根 5 分钟柱会细成发丝）。空桶不补的话，只在一个时段有流量的
/// 渠道会画成一根占满全宽的柱子，"从几点开始坏"就读不出来了。桶键与后端同格式
/// （`YYYY-MM-DD HH:MM:SS`，UTC）。合并小时桶时分位数取该小时内的最大值——
/// 时间线要回答"最糟到什么程度"，取均值会把一次 30s 的尖峰抹平。
function fillTimeline(points: TimelinePoint[], hours: Range): TimelinePoint[] {
  const stepMin = hours > 24 ? 60 : 5
  const stepMs = stepMin * 60_000
  const pad = (n: number) => String(n).padStart(2, '0')
  const keyOf = (ms: number) => {
    const d = new Date(Math.floor(ms / stepMs) * stepMs)
    return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:00`
  }
  const merged = new Map<string, TimelinePoint>()
  for (const p of points) {
    const key = keyOf(Date.parse(`${p.bucket.replace(' ', 'T')}Z`))
    const cur = merged.get(key)
    if (cur === undefined) {
      merged.set(key, { ...p, bucket: key })
    } else {
      const requests = cur.requests + p.requests
      const errors = cur.errors + p.errors
      merged.set(key, {
        bucket: key,
        requests,
        errors,
        error_rate_bp: requests > 0 ? Math.round((errors * 10_000) / requests) : 0,
        ttft_p50_ms: Math.max(cur.ttft_p50_ms, p.ttft_p50_ms),
        ttft_p95_ms: Math.max(cur.ttft_p95_ms, p.ttft_p95_ms),
        failovers: cur.failovers + p.failovers,
        tokens_per_1k_sec: Math.max(cur.tokens_per_1k_sec, p.tokens_per_1k_sec),
      })
    }
  }
  const out: TimelinePoint[] = []
  const now = Date.now()
  const start = now - hours * 3_600_000
  for (let ms = Math.floor(start / stepMs) * stepMs; ms <= now; ms += stepMs) {
    const key = keyOf(ms)
    out.push(
      merged.get(key) ?? {
        bucket: key,
        requests: 0,
        errors: 0,
        error_rate_bp: 0,
        ttft_p50_ms: 0,
        ttft_p95_ms: 0,
        failovers: 0,
        tokens_per_1k_sec: 0,
      },
    )
  }
  return out
}

/// 单渠道健康时间线（5 分钟桶，mv_channel_5min）。
///
/// 渠道行的"近 24h 错误率 25%"只回答**有多糟**；这里回答**从几点开始糟、现在还在
/// 糟吗**——决定"等它自愈"还是"现在就切"的是后者。上图请求量按成功/失败堆叠
/// （失败段一眼可见），下图 TTFT p50/p95（变慢往往先于变坏）。
export function ChannelTimelineDrawer({
  channel,
  open,
  onClose,
}: {
  channel: { id: number; name: string; provider: string }
  open: boolean
  onClose: () => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [hours, setHours] = useState<Range>(24)
  const q = useQuery({
    queryKey: qk.channelTimeline(channel.id, hours),
    queryFn: () => apiFetch<TimelineResp>(`/admin/stats/channels/${channel.id}/timeline?hours=${hours}`),
    enabled: open,
    retry: false,
  })

  const rangeLabel: Record<Range, string> = {
    6: t('admin:timelineRange6h'),
    24: t('admin:timelineRange24h'),
    168: t('admin:timelineRange7d'),
  }
  // 桶标签：24h 内给 HH:mm，7 天给 MM-DD HH:mm
  const label = (bucket: string) => (hours > 24 ? bucket.slice(5, 16) : bucket.slice(11, 16))
  const rows = fillTimeline(q.data?.data ?? [], hours).map((p) => ({
    bucket: label(p.bucket),
    ok: p.requests - p.errors,
    errors: p.errors,
    p50: p.ttft_p50_ms,
    p95: p.ttft_p95_ms,
    failovers: p.failovers,
  }))
  const total = q.data
  const variant =
    total === undefined
      ? 'muted'
      : total.error_rate_bp >= BAD_BP
        ? 'destructive'
        : total.error_rate_bp >= WARN_BP
          ? 'warning'
          : 'success'

  return (
    <Drawer
      open={open}
      onClose={onClose}
      size="lg"
      title={t('admin:timelineTitle', { name: channel.name })}
      description={t('admin:timelineDesc')}
      footer={
        <div className="flex w-full flex-wrap items-center justify-between gap-2">
          <div className="flex gap-2">
            <Link
              to="/admin/logs"
              search={{ channel_id: channel.id, hours, errors_only: (total?.errors ?? 0) > 0 ? true : undefined }}
            >
              <Button variant="outline" size="sm">
                <ScrollText className="mr-1.5 h-3.5 w-3.5" />
                {t('admin:timelineViewLogs')}
              </Button>
            </Link>
            <Link to="/admin/stats" search={{ channel_id: channel.id, days: hours > 24 ? 7 : 1 }}>
              <Button variant="outline" size="sm">
                <BarChart3 className="mr-1.5 h-3.5 w-3.5" />
                {t('admin:timelineViewAnalytics')}
              </Button>
            </Link>
          </div>
          <Button variant="ghost" size="sm" onClick={onClose}>
            {t('common:close')}
          </Button>
        </div>
      }
    >
      <div className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Segmented
            options={RANGES.map((r) => ({ value: r, label: rangeLabel[r] }))}
            value={hours}
            onChange={setHours}
            size="sm"
          />
          {total && (
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <Badge variant="muted">{channel.provider}</Badge>
              <span>{t('admin:health24hRequests', { n: formatCount(total.requests, locale) })}</span>
              <Badge variant={variant}>
                {t('admin:timelineErrorRate', { v: formatBp(total.error_rate_bp, locale) })}
              </Badge>
            </div>
          )}
        </div>

        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : q.isLoading ? (
          <LoadingState />
        ) : (total?.requests ?? 0) === 0 ? (
          <EmptyState hint={t('admin:timelineEmpty')} />
        ) : (
          <>
            <div>
              <p className="mb-1 text-xs font-medium text-muted-foreground">{t('admin:timelineRequests')}</p>
              <div className="h-48">
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart data={rows} barCategoryGap={1}>
                    <CartesianGrid strokeDasharray="3 3" className="stroke-border" vertical={false} />
                    <XAxis dataKey="bucket" fontSize={10} minTickGap={32} />
                    <YAxis fontSize={10} allowDecimals={false} />
                    <Tooltip contentStyle={{ fontSize: 12 }} />
                    <Legend itemSorter={null} />
                    <Bar dataKey="ok" name={t('admin:timelineOk')} stackId="r" fill="var(--color-primary)" isAnimationActive={false} />
                    <Bar dataKey="errors" name={t('logs:failed')} stackId="r" fill="var(--color-destructive)" isAnimationActive={false} />
                  </BarChart>
                </ResponsiveContainer>
              </div>
            </div>
            <div>
              <p className="mb-1 text-xs font-medium text-muted-foreground">{t('admin:timelineTtft')}</p>
              <div className="h-40">
                <ResponsiveContainer width="100%" height="100%">
                  <LineChart data={rows}>
                    <CartesianGrid strokeDasharray="3 3" className="stroke-border" vertical={false} />
                    <XAxis dataKey="bucket" fontSize={10} minTickGap={32} />
                    <YAxis fontSize={10} unit=" ms" />
                    <Tooltip contentStyle={{ fontSize: 12 }} formatter={(v) => `${formatCount(Number(v), locale)} ms`} />
                    <Legend itemSorter={null} />
                    <Line type="monotone" dataKey="p50" name="p50" stroke="var(--color-chart-1)" dot={false} isAnimationActive={false} />
                    <Line type="monotone" dataKey="p95" name="p95" stroke="var(--color-chart-2)" dot={false} isAnimationActive={false} />
                  </LineChart>
                </ResponsiveContainer>
              </div>
            </div>
          </>
        )}
      </div>
    </Drawer>
  )
}
