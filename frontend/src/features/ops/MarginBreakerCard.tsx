import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { scaledInteger } from '@/features/settings/setting-catalog'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

/// 后端 `settings.margin_breaker` 的生效值（缺键已按缺省填好、越界已夹取）。
interface BreakerConfig {
  enabled: boolean
  window_hours: number
  min_requests: number
  min_cost_micro: number
  margin_bp: number
  cooldown_secs: number
  lift_secs: number
}

interface BlockRow {
  group_code: string
  channel_id: number
  channel_name: string | null
  state: 'blocked' | 'lifted'
  active: boolean
  since: number
  until: number
  requests: number
  amount_micro: number
  cost_micro: number
  margin_bp: number
}

/// 表单里全是人看的单位（小时 / 分钟 / 美元 / 百分比），提交时换回后端的整数口径。
interface Draft {
  enabled: boolean
  window_hours: string
  min_requests: string
  min_cost_usd: string
  threshold_pct: string
  cooldown_min: string
  lift_hours: string
}

function toDraft(c: BreakerConfig): Draft {
  return {
    enabled: c.enabled,
    window_hours: String(c.window_hours),
    min_requests: String(c.min_requests),
    min_cost_usd: (c.min_cost_micro / 1_000_000).toString(),
    threshold_pct: (c.margin_bp / 100).toString(),
    cooldown_min: String(Math.round(c.cooldown_secs / 60)),
    lift_hours: String(Math.round(c.lift_secs / 3600)),
  }
}

/// 带符号的百分比 → 万分比整数（阈值允许负数：容忍小幅亏损）。
function percentToBp(text: string): number | null {
  const trimmed = text.trim()
  const negative = trimmed.startsWith('-')
  const scaled = scaledInteger(negative ? trimmed.slice(1) : trimmed, 2)
  return scaled === null ? null : negative ? -scaled : scaled
}

function fromDraft(d: Draft): Record<string, unknown> | null {
  const window_hours = scaledInteger(d.window_hours, 0)
  const min_requests = scaledInteger(d.min_requests, 0)
  const min_cost_micro = scaledInteger(d.min_cost_usd, 6)
  const margin_bp = percentToBp(d.threshold_pct)
  const cooldown_min = scaledInteger(d.cooldown_min, 0)
  const lift_hours = scaledInteger(d.lift_hours, 0)
  if (
    window_hours === null ||
    min_requests === null ||
    min_cost_micro === null ||
    margin_bp === null ||
    cooldown_min === null ||
    lift_hours === null
  )
    return null
  return {
    enabled: d.enabled,
    window_hours,
    min_requests,
    min_cost_micro,
    margin_bp,
    cooldown_secs: cooldown_min * 60,
    lift_secs: lift_hours * 3600,
  }
}

/// 负毛利熔断（IMPLEMENTATION §11.34）：配置 + 当前被暂停的分组 × 渠道 + 解除。
///
/// 放在运维页而不是设置页：它会真的把渠道从候选里摘掉，属于"改变线上行为"的动作，
/// 与死信处置、留存裁剪同类；纯配置面（设置页）只保留 JSON 兜底入口。
export function MarginBreakerCard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const can = usePermission()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<Draft | null>(null)

  const q = useQuery({
    queryKey: qk.marginBreaker,
    queryFn: () => apiFetch<{ config: BreakerConfig; data: BlockRow[] }>('/admin/margin-breaker'),
  })
  // 表单以服务端生效值起步；用户一动就转成本地草稿，刷新列表不覆盖正在编辑的值
  const form = draft ?? (q.data ? toDraft(q.data.config) : null)
  const patch = (next: Partial<Draft>) => form && setDraft({ ...form, ...next })
  const parsed = form ? fromDraft(form) : null

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.marginBreaker })
  const save = useMutation({
    mutationFn: (value: Record<string, unknown>) =>
      apiFetch('/admin/settings', { method: 'POST', body: { key: 'margin_breaker', value } }),
    onSuccess: () => {
      toast.success(t('admin:saved'))
      setDraft(null)
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const lift = useMutation({
    mutationFn: (row: BlockRow) =>
      apiFetch<{ until: number }>('/admin/margin-breaker/lift', {
        method: 'POST',
        body: { group_code: row.group_code, channel_id: row.channel_id },
      }),
    onSuccess: (r) => {
      toast.success(t('admin:marginBreakerLifted', { until: dayjs.unix(r.until).format('MM-DD HH:mm') }))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const rows = q.data?.data ?? []
  const canConfigure = can('settings.write')
  const canLift = can('channel.write')
  const now = dayjs().unix()

  const stateBadge = (r: BlockRow) => {
    if (r.until <= now) return <Badge variant="muted">{t('admin:marginBreakerStateExpired')}</Badge>
    if (r.state === 'lifted') return <Badge variant="warning">{t('admin:marginBreakerStateLifted')}</Badge>
    return <Badge variant="destructive">{t('admin:marginBreakerStateBlocked')}</Badge>
  }

  const numberField = (id: keyof Omit<Draft, 'enabled'>, label: string, hint?: string) => (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={`mb-${id}`}>{label}</Label>
      <Input
        id={`mb-${id}`}
        className="w-40"
        inputMode="decimal"
        value={form?.[id] ?? ''}
        disabled={!canConfigure || form === null}
        onChange={(e) => patch({ [id]: e.target.value })}
      />
      {hint && <p className="max-w-xs text-xs text-muted-foreground">{hint}</p>}
    </div>
  )

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>
          {t('admin:marginBreakerTitle')}
          {q.data && (
            <Badge variant={q.data.config.enabled ? 'success' : 'muted'} className="ml-2">
              {q.data.config.enabled ? t('common:enabled') : t('common:disabled')}
            </Badge>
          )}
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:marginBreakerDesc')}</p>

        {form && (
          <div className="flex flex-col gap-3 rounded-lg border border-border p-4">
            <Switch
              label={t('admin:marginBreakerEnabled')}
              checked={form.enabled}
              disabled={!canConfigure}
              onChange={(v) => patch({ enabled: v })}
            />
            <div className="flex flex-wrap gap-4">
              {numberField('window_hours', t('admin:marginBreakerWindow'))}
              {numberField('min_requests', t('admin:marginBreakerMinRequests'))}
              {numberField('min_cost_usd', t('admin:marginBreakerMinCost'))}
              {numberField('threshold_pct', t('admin:marginBreakerThreshold'), t('admin:marginBreakerThresholdHint'))}
              {numberField('cooldown_min', t('admin:marginBreakerCooldown'))}
              {numberField('lift_hours', t('admin:marginBreakerLift'))}
            </div>
            {canConfigure && (
              <Button
                className="self-start"
                disabled={draft === null || parsed === null || save.isPending}
                onClick={() => parsed && save.mutate(parsed)}
              >
                {t('common:save')}
              </Button>
            )}
          </div>
        )}

        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : q.isPending ? (
          <LoadingState />
        ) : !q.data.config.enabled && rows.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('admin:marginBreakerDisabledHint')}</p>
        ) : rows.length === 0 ? (
          <EmptyState hint={t('admin:marginBreakerEmpty')} />
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:marginBreakerColPair')}</Th>
                <Th>{t('admin:marginBreakerColState')}</Th>
                <Th>{t('admin:marginBreakerColSample')}</Th>
                <Th numeric>{t('admin:marginBreakerColMargin')}</Th>
                <Th>{t('admin:marginBreakerColUntil')}</Th>
                {canLift && <Th className="text-right">{t('common:actions')}</Th>}
              </Tr>
            </THead>
            <TBody>
              {rows.map((r) => (
                <Tr key={`${r.group_code}|${r.channel_id}`} className={r.active ? undefined : 'opacity-60'}>
                  <Td>
                    <div className="flex flex-col">
                      <span className="font-mono text-xs">{r.group_code}</span>
                      <span className="text-xs text-muted-foreground">
                        #{r.channel_id} {r.channel_name ?? ''}
                      </span>
                    </div>
                  </Td>
                  <Td>{stateBadge(r)}</Td>
                  <Td className="text-xs text-muted-foreground">
                    {t('admin:marginBreakerSample', {
                      n: r.requests,
                      revenue: formatMoney(r.amount_micro, locale),
                      cost: formatMoney(r.cost_micro, locale),
                    })}
                  </Td>
                  {/* 负毛利标红：这一列就是"为什么被摘掉"的答案 */}
                  <Td numeric className={r.margin_bp < 0 ? 'text-destructive tabular-nums' : 'tabular-nums'}>
                    {formatBp(r.margin_bp, locale)}
                  </Td>
                  <Td className="whitespace-nowrap text-xs">{dayjs.unix(r.until).format('MM-DD HH:mm')}</Td>
                  {canLift && (
                    <Td className="text-right">
                      {r.state === 'blocked' && r.active && (
                        <Button size="sm" variant="outline" loading={lift.isPending} onClick={() => lift.mutate(r)}>
                          {t('admin:marginBreakerLiftAction')}
                        </Button>
                      )}
                    </Td>
                  )}
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
