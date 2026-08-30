import { useMutation, useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

export const Route = createFileRoute('/admin/ops')({
  component: OpsPage,
})

interface LeaderboardRow {
  user_id: number | string
  username: string
  requests: number | string
  tokens: number | string
  amount_micro: number | string
}

function OpsPage() {
  return (
    <div className="flex flex-col gap-4">
      <LeaderboardCard />
      <RetentionCard />
      <NotifyCard />
    </div>
  )
}

/* 用户消耗排行（#1790-11）：CH mv_user_day 聚合。 */
function LeaderboardCard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [days, setDays] = useState(7)
  const board = useQuery({
    queryKey: ['admin-leaderboard', days],
    queryFn: () =>
      apiFetch<{ data: LeaderboardRow[] }>(`/admin/leaderboard?days=${days}&limit=20`),
  })

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>{t('admin:leaderboard')}</CardTitle>
        <div className="flex gap-1">
          {[7, 30, 90].map((d) => (
            <Button
              key={d}
              size="sm"
              variant={days === d ? 'default' : 'outline'}
              onClick={() => setDays(d)}
            >
              {t('admin:lastDays', { days: d })}
            </Button>
          ))}
        </div>
      </CardHeader>
      <CardContent>
        {board.isError && (
          <p className="text-xs text-muted-foreground">{describeError(board.error)}</p>
        )}
        {board.data && board.data.data.length === 0 && (
          <p className="text-xs text-muted-foreground">{t('common:empty')}</p>
        )}
        {board.data && board.data.data.length > 0 && (
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b text-left text-xs text-muted-foreground">
                <th className="py-1.5 pr-2">#</th>
                <th className="py-1.5 pr-2">{t('admin:userId')}</th>
                <th className="py-1.5 pr-2">{t('admin:username')}</th>
                <th className="py-1.5 pr-2 text-right">{t('admin:requests')}</th>
                <th className="py-1.5 pr-2 text-right">{t('admin:tokens')}</th>
                <th className="py-1.5 text-right">{t('admin:spend')}</th>
              </tr>
            </thead>
            <tbody>
              {board.data.data.map((row, i) => (
                <tr key={String(row.user_id)} className="border-b last:border-0">
                  <td className="py-1.5 pr-2 text-muted-foreground">{i + 1}</td>
                  <td className="py-1.5 pr-2">{String(row.user_id)}</td>
                  <td className="py-1.5 pr-2">{row.username || '—'}</td>
                  <td className="py-1.5 pr-2 text-right">{String(row.requests)}</td>
                  <td className="py-1.5 pr-2 text-right">{String(row.tokens)}</td>
                  <td className="py-1.5 text-right">{formatMoney(Number(row.amount_micro), locale)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </CardContent>
    </Card>
  )
}

/* 数据保留策略（#1790-1）：retention_months，0=永久；worker 裁剪超期 PG 月分区。 */
function RetentionCard() {
  const { t } = useTranslation()
  const [months, setMonths] = useState('')
  const [msg, setMsg] = useState<string | null>(null)
  const current = useQuery({
    queryKey: ['setting', 'retention_months'],
    queryFn: () => apiFetch<{ value: number | null }>('/admin/settings/retention_months'),
  })
  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'retention_months', value: Number(months) },
      }),
    onSuccess: () => {
      setMsg(t('admin:saved'))
      void current.refetch()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:retention')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">
          {t('admin:retentionHint', { current: current.data?.value ?? 0 })}
        </p>
        <div className="flex items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="retention">{t('admin:retentionMonths')}</Label>
            <Input
              id="retention"
              inputMode="numeric"
              className="w-40"
              value={months}
              onChange={(e) => setMonths(e.target.value)}
            />
          </div>
          <Button disabled={save.isPending || months === ''} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
          {msg && <span className="pb-2 text-xs text-muted-foreground">{msg}</span>}
        </div>
      </CardContent>
    </Card>
  )
}

/* 通知多路（#1790-8）：settings.notify_channels JSON 配置。 */
function NotifyCard() {
  const { t } = useTranslation()
  const [text, setText] = useState<string | null>(null)
  const [msg, setMsg] = useState<string | null>(null)
  const current = useQuery({
    queryKey: ['setting', 'notify_channels'],
    queryFn: () => apiFetch<{ value: unknown | null }>('/admin/settings/notify_channels'),
  })
  const shown =
    text ?? (current.data ? JSON.stringify(current.data.value ?? [], null, 2) : '')

  const save = useMutation({
    mutationFn: () => {
      const parsed: unknown = JSON.parse(shown)
      if (!Array.isArray(parsed)) throw new Error('bad_request')
      return apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'notify_channels', value: parsed },
      })
    },
    onSuccess: () => {
      setMsg(t('admin:saved'))
      void current.refetch()
    },
    onError: (err) =>
      setMsg(err instanceof SyntaxError ? t('admin:notifyBadJson') : describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:notify')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:notifyHint')}</p>
        <textarea
          className="min-h-40 w-full rounded-md border bg-transparent p-2 font-mono text-xs"
          spellCheck={false}
          value={shown}
          onChange={(e) => setText(e.target.value)}
          placeholder='[{"type":"webhook","url":"https://...","events":["drift","channel_cooldown","balance_low"],"min_interval_secs":300}]'
        />
        <div className="flex items-center gap-3">
          <Button disabled={save.isPending || shown.trim() === ''} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
          {msg && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>
      </CardContent>
    </Card>
  )
}
