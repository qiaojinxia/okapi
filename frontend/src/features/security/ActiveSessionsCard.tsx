import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
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
import { toast } from '@/components/ui/toast'
import { clampOffset } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

interface SessionRow {
  sid: string
  ip: string | null
  ua: string | null
  created_at: number
  current: boolean
}

/// 归并后的「设备」：同一浏览器 + 同一 IP 的多条会话合成一行。
interface DeviceGroup {
  key: string
  ua: string | null
  ip: string | null
  /// 组内最近一条会话的建立时间。
  latest: number
  /// 组内全部 sid（吊销按整组走）。
  sids: string[]
  /// 组内是否含当前浏览器的会话。
  current: boolean
}

/// UA 只取产品名段：完整 UA 一行放不下，也没人读得完。
const shortUa = (ua: string | null) => (ua ? (ua.split(' ')[0] ?? ua).slice(0, 40) : null)

/// 按「设备」归并会话。
///
/// 同一完整 UA + IP 归并；完整 UA 区分同一 IP 下不同浏览器，吊销按整组走。
function groupByDevice(rows: SessionRow[]): DeviceGroup[] {
  const map = new Map<string, DeviceGroup>()
  for (const r of rows) {
    const ua = r.ua
    const key = JSON.stringify([ua, r.ip])
    const hit = map.get(key)
    if (hit) {
      hit.sids.push(r.sid)
      hit.latest = Math.max(hit.latest, r.created_at)
      hit.current = hit.current || r.current
    } else {
      map.set(key, {
        key,
        ua,
        ip: r.ip,
        latest: r.created_at,
        sids: [r.sid],
        current: r.current,
      })
    }
  }
  // 当前浏览器置顶，其余按最近活跃降序——要找"不是我的"，最新的最可疑
  return [...map.values()].sort((a, b) => {
    if (a.current !== b.current) return a.current ? -1 : 1
    return b.latest - a.latest
  })
}

/// 有效 web 会话（与「最近登录」审计卡分开：审计是尝试记录，这里是还能兑 key 的 cookie）。
export function ActiveSessionsCard({ preview = false, active = true }: { preview?: boolean; active?: boolean }) {
  const { t } = useTranslation()
  const [offset, setOffset] = useState(0)
  const [pageSize, setPageSize] = useState(5)
  const queryClient = useQueryClient()
  const q = useQuery({
    queryKey: qk.mySessions,
    queryFn: () => apiFetch<{ data: SessionRow[]; limit: number | null }>('/api/me/sessions'),
    staleTime: 15_000,
    enabled: active,
    retry: false,
  })
  const rows = q.data?.data ?? []
  const limit = q.data?.limit ?? null
  const devices = groupByDevice(rows)
  const start = clampOffset(offset, pageSize, devices.length)
  const shown = preview ? devices.slice(0, 3) : devices.slice(start, start + pageSize)

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: qk.mySessions })
    void queryClient.invalidateQueries({ queryKey: qk.me, exact: true })
    void queryClient.invalidateQueries({ queryKey: qk.myProfile })
  }

  const revokeDevice = useMutation({
    mutationFn: async (group: DeviceGroup) => {
      // 逐条吊销：服务端按 sid 粒度，这里只是把"一台设备"翻译成它的若干 sid
      for (const sid of group.sids) {
        await apiFetch<{ ok: boolean }>(`/api/me/sessions/${encodeURIComponent(sid)}`, {
          method: 'DELETE',
        })
      }
    },
    onSuccess: () => {
      toast.success(t('security:sessionsRevoked'))
    },
    onError: (err) => toast.error(describeError(err)),
    onSettled: invalidate,
  })

  const revokeAll = useMutation({
    mutationFn: () => apiFetch<{ ok: boolean }>('/api/me/sessions', { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('security:sessionsRevokedAll'))
    },
    onError: (err) => toast.error(describeError(err)),
    onSettled: invalidate,
  })

  return (
    <Card
      data-slot="security-sessions"
      id={preview ? undefined : 'profile-sessions'}
      role="region"
      aria-label={t('security:sessionsTitle')}
      tabIndex={0}
      className={cn('min-w-0 scroll-mt-24 outline-none focus-visible:ring-2 focus-visible:ring-primary/40', preview && 'lg:min-h-40 lg:overflow-y-auto lg:overscroll-contain')}
    >
      <CardHeader>
        <CardTitle>{t('security:sessionsTitle')}</CardTitle>
        <CardDescription>
          {t('security:sessionsDesc')}
          {limit !== null && <> {t('security:sessionsLimitHint', { n: limit })}</>}
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 pt-2">
        {q.isPending ? <LoadingState className="py-4" /> : q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : devices.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('security:sessionsEmpty')}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border text-xs">
            {shown.map((d) => (
              <li key={d.key} className={cn('flex flex-wrap items-center gap-2', preview ? 'py-1.5' : 'py-3')}>
                {d.current && <Badge variant="success">{t('security:sessionsCurrent')}</Badge>}
                <span className="tabular-nums text-muted-foreground">
                  {d.latest > 0 ? dayjs.unix(d.latest).format(preview ? 'MM-DD HH:mm' : 'YYYY-MM-DD HH:mm:ss') : '—'}
                </span>
                <span className="min-w-0 font-mono break-all">{d.ip ?? '—'}</span>
                {d.sids.length > 1 && (
                  <span className="tabular-nums text-muted-foreground">
                    {t('security:sessionsGrouped', { n: d.sids.length })}
                  </span>
                )}
                {!preview && <Button
                  type="button"
                  variant="ghost"
                  className="h-7 px-2 text-xs"
                  loading={revokeDevice.isPending && revokeDevice.variables?.key === d.key}
                  disabled={q.isFetching || revokeAll.isPending || revokeDevice.isPending}
                  onClick={() => revokeDevice.mutate(d)}
                >
                  {t('security:sessionsRevoke')}
                </Button>}
                <span className={cn('min-w-0 text-muted-foreground', preview ? 'flex-1 truncate' : 'w-full break-all')} title={d.ua ?? ''}>
                  {(preview ? shortUa(d.ua) : d.ua) ?? '—'}
                </span>
              </li>
            ))}
          </ul>
        )}
        {!preview && q.isSuccess && <Pagination total={devices.length} limit={pageSize} offset={offset} onOffset={setOffset}
          pageSizes={[5, 10]} onLimit={(next) => { setPageSize(next); setOffset(0) }} disabled={q.isFetching || revokeDevice.isPending || revokeAll.isPending} className="rounded-lg shadow-none" />}
        {!preview && devices.length > 0 && (
          <Button
            type="button"
            variant="outline"
            className="self-start"
            loading={revokeAll.isPending}
            disabled={q.isFetching || revokeDevice.isPending}
            onClick={() => revokeAll.mutate()}
          >
            {t('security:sessionsRevokeAll')}
          </Button>
        )}
        {preview && <Link to="/portal/profile" search={{ tab: 'signins' }} hash="profile-sessions" className="inline-flex min-h-9 items-center gap-1 self-start text-xs font-medium text-primary hover:underline">
          {t('security:viewAllSessions')}<ArrowUpRight className="h-3.5 w-3.5" />
        </Link>}
      </CardContent>
    </Card>
  )
}
