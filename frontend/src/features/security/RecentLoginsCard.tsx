import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
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

/// 首屏行数：端点固定回 20 条，一次铺满要 800px，把这张审计卡顶到三屏开外。
/// 8 行覆盖"今天有没有异常"，其余按需展开。
const PREVIEW_ROWS = 8

/// 最近登录（new-api "登录会话"卡的对应物）：成功 / 失败各带 IP 与时间。
///
/// 不枚举会话、只列尝试记录：共享设备与撞库场景下用户最先要回答的是
/// "有没有不是我的登录"，看到失败尝试就该去改密码开两步验证——卡片文案直说。
///
/// 失败是信号、成功是背景噪音，故给一个「仅失败」筛选：撞库时失败会把成功记录
/// 挤出这 20 条窗口之外，先筛再看比翻完 20 行快。
export function RecentLoginsCard() {
  const { t } = useTranslation()
  const [failedOnly, setFailedOnly] = useState(false)
  const [expanded, setExpanded] = useState(false)
  const q = useQuery({
    queryKey: qk.myLogins,
    queryFn: () => apiFetch<{ data: LoginRow[] }>('/api/me/logins'),
    staleTime: 30_000,
  })
  const all = q.data?.data ?? []
  const failedCount = all.filter((r) => !r.ok).length
  const filtered = failedOnly ? all.filter((r) => !r.ok) : all
  const shown = expanded ? filtered : filtered.slice(0, PREVIEW_ROWS)
  const hidden = filtered.length - shown.length
  // UA 只取产品名段：完整 UA 一行放不下，也没人读得完
  const shortUa = (ua: string | null) => (ua ? (ua.split(' ')[0] ?? ua).slice(0, 40) : '—')

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('portal:loginsTitle')}</CardTitle>
        <CardDescription>{t('portal:loginsDesc')}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 pt-2">
        {failedCount > 0 && (
          <div className="flex flex-wrap items-center gap-2">
            <Button
              type="button"
              variant={failedOnly ? 'default' : 'outline'}
              className="h-7 px-2 text-xs"
              onClick={() => {
                setFailedOnly((v) => !v)
                setExpanded(false)
              }}
            >
              {t('portal:loginsFailedOnly', { n: failedCount })}
            </Button>
          </div>
        )}
        {q.isError || filtered.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('portal:loginsEmpty')}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border text-xs">
            {shown.map((r, i) => (
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
        {hidden > 0 && (
          <Button
            type="button"
            variant="ghost"
            className="h-7 self-start px-2 text-xs"
            onClick={() => setExpanded(true)}
          >
            {t('portal:loginsShowAll', { n: hidden })}
          </Button>
        )}
        {expanded && filtered.length > PREVIEW_ROWS && (
          <Button
            type="button"
            variant="ghost"
            className="h-7 self-start px-2 text-xs"
            onClick={() => setExpanded(false)}
          >
            {t('portal:loginsCollapse')}
          </Button>
        )}
      </CardContent>
    </Card>
  )
}
