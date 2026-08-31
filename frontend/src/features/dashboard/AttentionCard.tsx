import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { CheckCircle2, ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import type { ReconResp } from '@/features/ops/ReconciliationCard'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

interface ChannelStat {
  channel_id: number
  channel_name: string
  error_rate_bp: number
}
interface ModelRow {
  model_name: string
  pricing_mode: string | null
}
interface PoolRow {
  pool_code: string
  channel_count: number
}
/// 一条待办。`to` 指向能解决它的页面——发现问题和处理问题之间不该再让人找路。
interface Item {
  key: string
  text: string
  to: string
  tone: 'warning' | 'destructive'
}

/// "需要注意"面板。
///
/// 落地页的价值不是把所有数字摆出来，而是回答"我现在该做什么"。此前这里是一张
/// 三方对账漂移表——那是排障工具，不是日常动线，且没有漂移时整页空着。
/// 现在只列真正需要动手的项，全清时明确说"没有待办"而不是留一片空白。
export function AttentionCard({ days }: { days: number }) {
  const { t } = useTranslation()

  const channels = useQuery({
    queryKey: qk.statsChannels(days),
    queryFn: () => apiFetch<{ data: ChannelStat[] }>(`/admin/stats/channels?days=${days}`),
  })
  const models = useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: ModelRow[] }>('/admin/models'),
  })
  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: PoolRow[] }>('/admin/pools'),
  })
  const drift = useQuery({
    queryKey: qk.reconciliation,
    // 该端点返回 { drift_count, drifts }，不是通用的 { data } 形状
    queryFn: () => apiFetch<ReconResp>('/admin/reconciliation'),
  })

  const items: Item[] = []

  // 未定价模型：建了模型却没配价，请求会被直接拒——属于"配了一半"的典型漏项
  const unpriced = (models.data?.data ?? []).filter((m) => m.pricing_mode === null)
  if (unpriced.length > 0) {
    items.push({
      key: 'unpriced',
      text: t('admin:attnUnpriced', { n: unpriced.length, first: unpriced[0]?.model_name ?? '' }),
      to: '/admin/pricing',
      tone: 'destructive',
    })
  }

  // 空池：分组指向它就等于该组无可用渠道，症状是 503 而原因很不直观
  const emptyPools = (pools.data?.data ?? []).filter((p) => p.channel_count === 0)
  if (emptyPools.length > 0) {
    items.push({
      key: 'empty-pool',
      text: t('admin:attnEmptyPool', {
        n: emptyPools.length,
        first: emptyPools[0]?.pool_code ?? '',
      }),
      to: '/admin/pools',
      tone: 'warning',
    })
  }

  // 高错误率渠道：5% 起视为需要处置（与渠道健康卡同阈值）
  const bad = (channels.data?.data ?? []).filter((c) => c.error_rate_bp >= 500)
  if (bad.length > 0) {
    items.push({
      key: 'bad-channel',
      text: t('admin:attnBadChannel', {
        n: bad.length,
        first: bad[0]?.channel_name ?? '',
      }),
      to: '/admin/channels',
      tone: 'destructive',
    })
  }

  // 对账漂移：Redis/PG/CH 三方口径不一致，属于要人工介入的账目问题
  const drifted = drift.data?.drifts ?? []
  if (drifted.length > 0) {
    items.push({
      key: 'drift',
      text: t('admin:attnDrift', { n: drifted.length }),
      to: '/admin/ops',
      tone: 'destructive',
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:attnTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        {items.length === 0 ? (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <CheckCircle2 className="h-4 w-4 text-success" />
            {t('admin:attnClear')}
          </div>
        ) : (
          items.map((item) => (
            <Link
              key={item.key}
              to={item.to}
              className="flex items-center justify-between gap-3 rounded-md border border-border px-3 py-2.5 transition-colors hover:bg-muted/50"
            >
              <span className="flex min-w-0 items-center gap-2">
                <Badge variant={item.tone}>
                  {t(item.tone === 'destructive' ? 'admin:attnUrgent' : 'admin:attnNotice')}
                </Badge>
                <span className="truncate text-sm">{item.text}</span>
              </span>
              <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />
            </Link>
          ))
        )}
      </CardContent>
    </Card>
  )
}
