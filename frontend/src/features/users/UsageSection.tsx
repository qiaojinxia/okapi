import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useTranslation } from 'react-i18next'
import { Bar, BarChart, LabelList, ResponsiveContainer, Tooltip, XAxis } from 'recharts'
import { Badge } from '@/components/ui/badge'
import { FieldGroup } from '@/components/ui/drawer'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface UsageResp {
  days: number
  stats_available: boolean
  daily: { day: string; requests: number; amount_micro: number }[]
  by_model: { model: string; requests: number; amount_micro: number; tokens: number }[]
  ledger: {
    event_id: number
    event_type: string
    delta_micro: number
    balance_after_micro: number | null
    actor: string
    tags: string[]
    reason: string | null
    created_at: string
  }[]
}

/// 代客用量视图（老 ok-api 用户详情 UsageOverviewTab 的吸收）。
///
/// 调余额、判退款之前先看两件事：他平时花多少（近 7 天按日柱 + Top 模型）、
/// 上次动过什么账（最近 10 条余额变动，含操作者——管理面要看得见谁调的）。
/// 三块都是"看"，没有一个提交按钮：这个页签的意义是让下一个动作有依据。
export function UsageSection({ userId }: { userId: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.userUsage(userId),
    queryFn: () => apiFetch<UsageResp>(`/admin/users/${userId}/usage?days=7`),
  })
  if (q.isError) return <ErrorState message={describeError(q.error)} />
  if (q.isPending) return <LoadingState />
  const d = q.data
  const total = d.daily.reduce((s, x) => s + x.amount_micro, 0)

  return (
    <div className="flex flex-col gap-4">
      <FieldGroup
        title={t('admin:userUsage7d', { amount: formatMoney(total, locale) })}
        hint={d.stats_available ? undefined : t('errors:stats_disabled')}
      >
        {d.stats_available && d.daily.length > 0 ? (
          <div className="h-24">
            <ResponsiveContainer width="100%" height="100%">
              <BarChart data={d.daily.map((x) => ({ ...x, day: x.day.slice(5), usd: x.amount_micro / 1_000_000 }))} margin={{ top: 14, right: 4, left: 4, bottom: 0 }}>
                <XAxis dataKey="day" fontSize={10} />
                <Tooltip
                  cursor={{ fill: 'var(--color-primary)', fillOpacity: 0.08 }}
                  content={({ active, payload }) => {
                    const row = payload?.[0]?.payload as { day?: string; amount_micro?: number; requests?: number } | undefined
                    if (!active || !row) return null
                    return (
                      <div className="note-body min-w-36 px-3 py-2 text-xs tabular-nums">
                        <p className="mb-1.5 border-b border-border/60 pb-1 font-mono text-[11px] font-medium text-muted-foreground">
                          {row.day}
                        </p>
                        <div className="space-y-1">
                          <div className="flex items-baseline justify-between gap-4">
                            <span className="text-muted-foreground">{t('common:amount')}</span>
                            <span className="font-semibold text-foreground">{formatMoney(row.amount_micro ?? 0, locale)}</span>
                          </div>
                          <div className="flex items-baseline justify-between gap-4">
                            <span className="text-muted-foreground">{t('common:requests')}</span>
                            <span className="font-medium text-foreground/90">{formatCount(row.requests ?? 0, locale)}</span>
                          </div>
                        </div>
                      </div>
                    )
                  }}
                />
                <Bar
                  dataKey="usd"
                  fill="var(--color-primary)"
                  radius={[3, 3, 0, 0]}
                  isAnimationActive={false}
                >
                  {/* 柱顶直接标当天消费，0 不标 */}
                  <LabelList dataKey="amount_micro" position="top" fontSize={9} fill="var(--color-muted-foreground)" formatter={(v) => Number(v) > 0 ? formatMoney(Number(v), locale) : ''} />
                </Bar>
              </BarChart>
            </ResponsiveContainer>
          </div>
        ) : (
          d.stats_available && <EmptyState className="py-4" />
        )}
        {d.by_model.length > 0 && (
          <ul className="divide-y divide-border/60 rounded-lg border border-border/75 bg-card/70 text-xs tabular-nums">
            {d.by_model.slice(0, 6).map((m) => (
              <li key={m.model} className="flex items-center justify-between gap-3 px-2.5 py-1.5" title={`${formatCount(m.tokens, locale)} tokens`}>
                <span className="min-w-0 truncate font-mono text-[11.5px] font-medium text-foreground">{m.model}</span>
                <span className="flex shrink-0 items-baseline gap-3">
                  <span className="text-[11px] text-muted-foreground">{formatCount(m.requests, locale)} · {formatCount(m.tokens, locale)}</span>
                  <span className="min-w-16 text-right font-semibold text-foreground">{formatMoney(m.amount_micro, locale)}</span>
                </span>
              </li>
            ))}
          </ul>
        )}
      </FieldGroup>

      <FieldGroup title={t('admin:userLedgerRecent')}>
        {d.ledger.length === 0 ? (
          <EmptyState className="py-4" />
        ) : (
          <ul className="flex flex-col divide-y divide-border text-xs">
            {d.ledger.map((e) => (
              <li key={e.event_id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
                <span className="w-24 shrink-0 text-muted-foreground">
                  {dayjs(e.created_at).format('MM-DD HH:mm')}
                </span>
                <span className={e.delta_micro > 0 ? 'w-24 font-medium text-success' : 'w-24 font-medium'}>
                  {e.delta_micro > 0 ? '+' : ''}
                  {formatMoney(e.delta_micro, locale)}
                </span>
                <Badge variant="muted">{t(`portal:ledgerType_${e.event_type}`)}</Badge>
                {e.tags.map((tag) => (
                  <Badge key={tag} variant="muted">
                    {t(`portal:ledgerTag_${tag}`, { defaultValue: tag })}
                  </Badge>
                ))}
                {e.reason && <span className="text-muted-foreground">“{e.reason}”</span>}
                <span className="ml-auto font-mono text-muted-foreground">{e.actor}</span>
              </li>
            ))}
          </ul>
        )}
      </FieldGroup>
    </div>
  )
}
