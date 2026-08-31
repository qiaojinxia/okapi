import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
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
      <RefundCard />
      <SettingsCard />
    </div>
  )
}

/// 按日志退款（§5.3）：事件溯源冲销，账单/统计/余额三处口径自动一致且幂等。
/// 故重复提交同一 request_id 是安全的——后端返回 already_refunded 而非二次退款。
function RefundCard() {
  const { t } = useTranslation()
  const [requestId, setRequestId] = useState('')
  const [reason, setReason] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const refund = useMutation({
    mutationFn: () =>
      apiFetch<{ outcome: string; refunded_micro?: number }>('/admin/billing/refund', {
        method: 'POST',
        body: { request_id: requestId.trim(), reason: reason.trim() },
      }),
    onSuccess: (r) => {
      setMsg(t('admin:refundOutcome', { outcome: r.outcome }))
      setRequestId('')
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:refundTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-wrap items-end gap-3">
        <p className="w-full text-xs text-muted-foreground">{t('admin:refundHint')}</p>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="rid">{t('admin:refundRequestId')}</Label>
          <Input
            id="rid"
            className="w-80 font-mono text-xs"
            value={requestId}
            placeholder="00000000-0000-0000-0000-000000000000"
            onChange={(e) => setRequestId(e.target.value)}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="rreason">{t('admin:refundReason')}</Label>
          <Input id="rreason" value={reason} onChange={(e) => setReason(e.target.value)} />
        </div>
        <Button
          disabled={refund.isPending || requestId.trim() === ''}
          onClick={() => refund.mutate()}
        >
          {t('admin:refund')}
        </Button>
        {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
      </CardContent>
    </Card>
  )
}

interface SettingRow {
  key: string
  value: unknown
  is_secret: boolean
  configured: boolean
  updated_at: string
}

/// 系统设置总览 + 就地编辑。
/// 敏感键（含 secret/key/token/password/webhook/credential）后端只回
/// `configured` 布尔占位，明文永不出接口——故此处只能覆写、不能读回。
function SettingsCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const settings = useQuery({
    queryKey: ['admin', 'settings'],
    queryFn: () => apiFetch<{ data: SettingRow[] }>('/admin/settings'),
  })

  const save = useMutation({
    mutationFn: (key: string) =>
      apiFetch('/admin/settings', {
        method: 'POST',
        // 值按 JSON 解析：设置项类型多样（布尔/数字/对象/数组），统一交由后端 JSONB 承载
        body: { key, value: JSON.parse(draft) as unknown },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      setEditing(null)
      setDraft('')
      void queryClient.invalidateQueries({ queryKey: ['admin', 'settings'] })
    },
    onError: (err) =>
      setMsg(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:settingsTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:settingsHint')}</p>
        {settings.isError ? (
          <p className="text-sm text-destructive">{describeError(settings.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:settingKey')}</Th>
                <Th>{t('admin:settingValue')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(settings.data?.data ?? []).map((s) => (
                <Tr key={s.key}>
                  <Td className="font-mono text-xs">{s.key}</Td>
                  <Td className="max-w-72 truncate font-mono text-xs">
                    {s.is_secret ? (
                      <Badge variant={s.configured ? 'success' : 'muted'}>
                        {s.configured ? t('admin:settingSet') : t('admin:settingUnset')}
                      </Badge>
                    ) : (
                      JSON.stringify(s.value)
                    )}
                  </Td>
                  <Td>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => {
                        setEditing(s.key)
                        // 敏感键无明文可回填，留空强制显式覆写
                        setDraft(s.is_secret ? '' : JSON.stringify(s.value))
                      }}
                    >
                      {t('common:edit')}
                    </Button>
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
        {editing !== null && (
          <div className="flex flex-wrap items-end gap-3 border-t border-border pt-3">
            <div className="flex min-w-64 flex-1 flex-col gap-1.5">
              <Label htmlFor="set-val">{editing}</Label>
              <Input
                id="set-val"
                className="font-mono text-xs"
                value={draft}
                placeholder='"value" / 123 / true / {"k":"v"}'
                onChange={(e) => setDraft(e.target.value)}
              />
            </div>
            <Button size="sm" disabled={draft.trim() === ''} onClick={() => save.mutate(editing)}>
              {t('common:save')}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setEditing(null)}>
              {t('common:cancel')}
            </Button>
          </div>
        )}
        {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
      </CardContent>
    </Card>
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
