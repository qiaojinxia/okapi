import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

interface LoginRow {
  ok: boolean
  at: string
  ip: string | null
  ua: string | null
  reason: string | null
}

/// 最近登录（new-api "登录会话"卡的对应物）：成功 / 失败各带 IP 与时间。
///
/// 不枚举会话、只列尝试记录：共享设备与撞库场景下用户最先要回答的是
/// "有没有不是我的登录"，看到失败尝试就该去改密码开两步验证——卡片文案直说。
export function RecentLoginsCard() {
  const { t } = useTranslation()
  const q = useQuery({
    queryKey: qk.myLogins,
    queryFn: () => apiFetch<{ data: LoginRow[] }>('/api/me/logins'),
    staleTime: 30_000,
  })
  const rows = q.data?.data ?? []
  // UA 只取产品名段：完整 UA 一行放不下，也没人读得完
  const shortUa = (ua: string | null) => (ua ? (ua.split(' ')[0] ?? ua).slice(0, 40) : '—')

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('portal:loginsTitle')}</CardTitle>
        <CardDescription>{t('portal:loginsDesc')}</CardDescription>
      </CardHeader>
      <CardContent className="pt-2">
        {q.isError || rows.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('portal:loginsEmpty')}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border text-xs">
            {rows.map((r, i) => (
              <li key={`${r.at}-${i}`} className="flex flex-wrap items-center gap-2 py-1.5">
                <Badge variant={r.ok ? 'success' : 'destructive'}>
                  {r.ok ? t('portal:loginsOk') : t('portal:loginsFailed')}
                </Badge>
                <span className="tabular-nums text-muted-foreground">
                  {dayjs(r.at).format('MM-DD HH:mm')}
                </span>
                <span className="font-mono">{r.ip ?? '—'}</span>
                <span className="truncate text-muted-foreground" title={r.ua ?? ''}>
                  {shortUa(r.ua)}
                </span>
                {!r.ok && r.reason && (
                  <span className="font-mono text-muted-foreground">{r.reason}</span>
                )}
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}
