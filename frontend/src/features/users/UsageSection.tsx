import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useTranslation } from 'react-i18next'
import { Bar, BarChart, ResponsiveContainer, Tooltip, XAxis } from 'recharts'
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
              <BarChart data={d.daily.map((x) => ({ ...x, day: x.day.slice(5), usd: x.amount_micro / 1_000_000 }))}>
                <XAxis dataKey="day" fontSize={10} />
                <Tooltip
                  formatter={(v) => [`$${Number(v).toFixed(4)}`, t('common:amount')]}
                />
                <Bar
                  dataKey="usd"
                  fill="var(--color-primary)"
                  radius={[2, 2, 0, 0]}
                  isAnimationActive={false}
                />
              </BarChart>
            </ResponsiveContainer>
          </div>
        ) : (
          d.stats_available && <EmptyState className="py-4" />
        )}
        {d.by_model.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {d.by_model.slice(0, 6).map((m) => (
              <Badge key={m.model} variant="muted" title={`${formatCount(m.tokens, locale)} tokens`}>
                <span className="font-mono">{m.model}</span> · {formatMoney(m.amount_micro, locale)} ·{' '}
                {formatCount(m.requests, locale)}
              </Badge>
            ))}
          </div>
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
