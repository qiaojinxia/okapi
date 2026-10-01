import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { ArrowUpRight } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Pagination } from '@/components/ui/pagination'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { clampOffset } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

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
///
/// 失败是信号、成功是背景噪音，故给一个「仅失败」筛选：撞库时失败会把成功记录
/// 挤出这 20 条窗口之外，先筛再看比翻完 20 行快。
export function RecentLoginsCard({ preview = false, active = true }: { preview?: boolean; active?: boolean }) {
  const { t } = useTranslation()
  const [failedOnly, setFailedOnly] = useState(false)
  const [offset, setOffset] = useState(0)
  const [pageSize, setPageSize] = useState(5)
  const q = useQuery({
    queryKey: qk.myLogins,
    queryFn: () => apiFetch<{ data: LoginRow[] }>('/api/me/logins'),
    staleTime: 30_000,
    enabled: active,
    retry: false,
  })
  const all = q.data?.data ?? []
  const failedCount = all.filter((r) => !r.ok).length
  const filtered = !preview && failedOnly ? all.filter((r) => !r.ok) : all
  const start = clampOffset(offset, pageSize, filtered.length)
  const shown = preview ? filtered.slice(0, 3) : filtered.slice(start, start + pageSize)
  // UA 只取产品名段：完整 UA 一行放不下，也没人读得完
  const shortUa = (ua: string | null) => (ua ? (ua.split(' ')[0] ?? ua).slice(0, 40) : '—')

  return (
    <Card
      data-slot="security-logins"
      id={preview ? undefined : 'profile-logins'}
      role="region"
      aria-label={t('portal:loginsTitle')}
      tabIndex={0}
      className={cn('min-w-0 scroll-mt-24 outline-none focus-visible:ring-2 focus-visible:ring-primary/40', preview && 'lg:min-h-0 lg:overflow-y-auto lg:overscroll-contain')}
    >
      <CardHeader>
        <CardTitle>{t('portal:loginsTitle')}</CardTitle>
        <CardDescription>{preview ? t('portal:loginsDesc') : t('security:loginsFullDesc')}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 pt-2">
        {!preview && failedCount > 0 && (
          <div className="flex flex-wrap items-center gap-2">
            <Button
              type="button"
              variant={failedOnly ? 'default' : 'outline'}
              className="h-7 px-2 text-xs"
              aria-pressed={failedOnly}
              onClick={() => { setFailedOnly((v) => !v); setOffset(0) }}
            >
              {t('portal:loginsFailedOnly', { n: failedCount })}
            </Button>
          </div>
        )}
        {q.isPending ? <LoadingState className="py-4" /> : q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : filtered.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('portal:loginsEmpty')}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border text-xs">
            {shown.map((r, i) => (
              <li key={`${r.at}-${i}`} className={cn('flex flex-wrap items-center gap-2', preview ? 'py-1.5' : 'py-3')}>
                <Badge variant={r.ok ? 'success' : 'destructive'}>
                  {r.ok ? t('portal:loginsOk') : t('portal:loginsFailed')}
                </Badge>
                <span className="tabular-nums text-muted-foreground">
                  {dayjs(r.at).format(preview ? 'MM-DD HH:mm' : 'YYYY-MM-DD HH:mm:ss')}
                </span>
                <span className="min-w-0 font-mono break-all">{r.ip ?? '—'}</span>
                {!r.ok && r.reason && (
                  <span className="min-w-0 font-mono break-all text-muted-foreground">{r.reason}</span>
                )}
                <span className={cn('min-w-0 text-muted-foreground', preview ? 'flex-1 truncate' : 'w-full break-all')} title={r.ua ?? ''}>
                  {preview ? shortUa(r.ua) : r.ua ?? '—'}
                </span>
              </li>
            ))}
          </ul>
        )}
        {!preview && q.isSuccess && <Pagination total={filtered.length} limit={pageSize} offset={offset} onOffset={setOffset}
          pageSizes={[5, 10]} onLimit={(next) => { setPageSize(next); setOffset(0) }} disabled={q.isFetching} className="rounded-lg shadow-none" />}
        {preview && <Link to="/portal/profile" search={{ tab: 'signins' }} hash="profile-logins" className="inline-flex min-h-9 items-center gap-1 self-start text-xs font-medium text-primary hover:underline">
          {t('security:viewAllLogins')}<ArrowUpRight className="h-3.5 w-3.5" />
        </Link>}
      </CardContent>
    </Card>
  )
}
