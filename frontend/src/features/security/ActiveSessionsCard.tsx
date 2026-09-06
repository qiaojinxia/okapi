import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

interface SessionRow {
  sid: string
  ip: string | null
  ua: string | null
  created_at: number
  current: boolean
}

/// 有效 web 会话（与「最近登录」审计卡分开：审计是尝试记录，这里是还能兑 key 的 cookie）。
export function ActiveSessionsCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const q = useQuery({
    queryKey: qk.mySessions,
    queryFn: () => apiFetch<{ data: SessionRow[] }>('/api/me/sessions'),
    staleTime: 15_000,
  })
  const rows = q.data?.data ?? []
  const shortUa = (ua: string | null) => (ua ? (ua.split(' ')[0] ?? ua).slice(0, 40) : '—')

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: qk.mySessions })
  }

  const revokeOne = useMutation({
    mutationFn: (sid: string) =>
      apiFetch<{ ok: boolean }>(`/api/me/sessions/${encodeURIComponent(sid)}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('security:sessionsRevoked'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const revokeAll = useMutation({
    mutationFn: () => apiFetch<{ ok: boolean }>('/api/me/sessions', { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('security:sessionsRevokedAll'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('security:sessionsTitle')}</CardTitle>
        <CardDescription>{t('security:sessionsDesc')}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 pt-2">
        {q.isError || rows.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('security:sessionsEmpty')}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border text-xs">
            {rows.map((r) => (
              <li key={r.sid} className="flex flex-wrap items-center gap-2 py-1.5">
                {r.current && <Badge variant="success">{t('security:sessionsCurrent')}</Badge>}
                <span className="tabular-nums text-muted-foreground">
                  {r.created_at > 0 ? dayjs.unix(r.created_at).format('MM-DD HH:mm') : '—'}
                </span>
                <span className="font-mono">{r.ip ?? '—'}</span>
                <span className="min-w-0 flex-1 truncate text-muted-foreground" title={r.ua ?? ''}>
                  {shortUa(r.ua)}
                </span>
                <Button
                  type="button"
                  variant="ghost"
                  className="h-7 px-2 text-xs"
                  loading={revokeOne.isPending && revokeOne.variables === r.sid}
                  onClick={() => revokeOne.mutate(r.sid)}
                >
                  {t('security:sessionsRevoke')}
                </Button>
              </li>
            ))}
          </ul>
        )}
        {rows.length > 0 && (
          <Button
            type="button"
            variant="outline"
            className="self-start"
            loading={revokeAll.isPending}
            onClick={() => revokeAll.mutate()}
          >
            {t('security:sessionsRevokeAll')}
          </Button>
        )}
      </CardContent>
    </Card>
  )
}
