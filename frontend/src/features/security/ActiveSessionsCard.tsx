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
/// 一次登录一条 cookie，而用户清一次 cookie、换个标签、跑个脚本就多一条——
/// 逐条列出来，同一台机器会占掉七八行，用户根本认不出哪条是自己的，也就不会去
/// 吊销可疑的那条。按 UA 产品段 + IP 归并才对得上心智里的"设备"；吊销整组走。
function groupByDevice(rows: SessionRow[]): DeviceGroup[] {
  const map = new Map<string, DeviceGroup>()
  for (const r of rows) {
    const ua = shortUa(r.ua)
    const key = `${ua ?? '?'}|${r.ip ?? '?'}`
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
export function ActiveSessionsCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const q = useQuery({
    queryKey: qk.mySessions,
    queryFn: () => apiFetch<{ data: SessionRow[]; limit: number | null }>('/api/me/sessions'),
    staleTime: 15_000,
  })
  const rows = q.data?.data ?? []
  const limit = q.data?.limit ?? null
  const devices = groupByDevice(rows)

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: qk.mySessions })
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
        <CardDescription>
          {t('security:sessionsDesc')}
          {limit !== null && <> {t('security:sessionsLimitHint', { n: limit })}</>}
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 pt-2">
        {q.isError || devices.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('security:sessionsEmpty')}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-border text-xs">
            {devices.map((d) => (
              <li key={d.key} className="flex flex-wrap items-center gap-2 py-1.5">
                {d.current && <Badge variant="success">{t('security:sessionsCurrent')}</Badge>}
                <span className="tabular-nums text-muted-foreground">
                  {d.latest > 0 ? dayjs.unix(d.latest).format('MM-DD HH:mm') : '—'}
                </span>
                <span className="font-mono">{d.ip ?? '—'}</span>
                <span className="min-w-0 flex-1 truncate text-muted-foreground" title={d.ua ?? ''}>
                  {d.ua ?? '—'}
                </span>
                {d.sids.length > 1 && (
                  <span className="tabular-nums text-muted-foreground">
                    {t('security:sessionsGrouped', { n: d.sids.length })}
                  </span>
                )}
                <Button
                  type="button"
                  variant="ghost"
                  className="h-7 px-2 text-xs"
                  loading={revokeDevice.isPending && revokeDevice.variables?.key === d.key}
                  onClick={() => revokeDevice.mutate(d)}
                >
                  {t('security:sessionsRevoke')}
                </Button>
              </li>
            ))}
          </ul>
        )}
        {devices.length > 0 && (
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
