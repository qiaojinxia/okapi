import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

export const REPORTING_TIMEZONE_KEY = 'reporting_timezone'

interface TimezoneSetting {
  value: string | null
  host_timezone?: string | null
  effective_timezone?: string | null
}

/// 当前时刻该时区相对 UTC 的偏移（`UTC+08:00`）；浏览器不认识的名字返回空串。
export function utcOffsetLabel(zone: string, at: Date = new Date()): string {
  try {
    const part = new Intl.DateTimeFormat('en-US', { timeZone: zone, timeZoneName: 'longOffset' })
      .formatToParts(at)
      .find((p) => p.type === 'timeZoneName')?.value
    if (!part) return ''
    return part === 'GMT' ? 'UTC+00:00' : part.replace('GMT', 'UTC')
  } catch {
    return ''
  }
}

/// 与 UTC 的时差在一年里是否都是整小时（取 1 月与 7 月，覆盖夏令时）。按小时汇总的历史按 UTC 整点切，
/// 半小时 / 45 分钟时差的时区切不出精确的本地日界，后端拒绝保存；这里提前置灰。
export function wholeHourZone(zone: string, year: number = new Date().getUTCFullYear()): boolean {
  return [0, 6].every((month) => {
    const label = utcOffsetLabel(zone, new Date(Date.UTC(year, month, 15, 12)))
    return label === '' || label.endsWith(':00')
  })
}

function browserZones(): string[] {
  const supported = (Intl as unknown as { supportedValuesOf?: (key: string) => string[] }).supportedValuesOf
  try {
    return supported ? supported('timeZone') : []
  } catch {
    return []
  }
}

/// 统计时区（settings.reporting_timezone）：统计报表里「一天」从几点开始切。
///
/// 缺省跟随服务器时区；选了某个时区后，ClickHouse 查询与 PG 会话立即按它切日 / 小时桶，
/// 不用重启。已记录的数据都以 UTC 存储，换时区只改变以后怎么切分，可以随时换回来。
export function TimezoneCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const current = useQuery({
    queryKey: qk.setting(REPORTING_TIMEZONE_KEY),
    queryFn: () => apiFetch<TimezoneSetting>(`/admin/settings/${REPORTING_TIMEZONE_KEY}`),
  })
  const stored = typeof current.data?.value === 'string' ? current.data.value : ''
  const [draft, setDraft] = useState<string | null>(null)
  const selected = draft ?? stored
  const host = current.data?.host_timezone ?? ''
  const browser = useMemo(() => {
    try {
      return Intl.DateTimeFormat().resolvedOptions().timeZone ?? ''
    } catch {
      return ''
    }
  }, [])

  const options = useMemo(() => {
    const zones = new Set(browserZones())
    // 已保存的值不在浏览器列表里（别名、较新的时区）也要能显示出来，不然下拉框会悄悄跳到第一项
    if (stored) zones.add(stored)
    if (browser) zones.add(browser)
    return [...zones].sort().map((zone) => {
      const offset = utcOffsetLabel(zone)
      const label = offset ? `${zone} (${offset})` : zone
      // 已保存的值即使不合规也保持可选，免得下拉框显示不出当前值
      return wholeHourZone(zone) || zone === stored
        ? { value: zone, label }
        : { value: zone, label: `${label} · ${t('analytics:tzFractional')}`, disabled: true }
    })
  }, [stored, browser, t])

  const save = useMutation({
    mutationFn: (value: string | null) =>
      apiFetch('/admin/settings', { method: 'POST', body: { key: REPORTING_TIMEZONE_KEY, value } }),
    onSuccess: () => {
      toast.success(t('admin:saved'))
      setDraft(null)
      // 时区一换，所有按天 / 按小时切分的视图（概览、分析、日历、渠道额度窗口、门户用量）都要重取
      void queryClient.invalidateQueries()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const effective = current.data?.effective_timezone ?? (stored || host)
  const hostLabel = host ? t('analytics:tzZoneWithOffset', { zone: host, offset: utcOffsetLabel(host) }).trim() : t('analytics:tzHostUnknown')
  const dirty = selected !== stored

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('analytics:tzTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <p className="text-sm text-muted-foreground">{t('analytics:tzDesc')}</p>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="reporting-timezone">{t('analytics:tzLabel')}</Label>
          <div className="flex flex-wrap items-center gap-2">
            <Select
              id="reporting-timezone"
              className="w-full max-w-md"
              value={selected}
              disabled={current.isPending || save.isPending}
              placeholder={t('analytics:tzFollowHost', { zone: hostLabel })}
              options={options}
              onChange={(value) => setDraft(value)}
            />
            <Button disabled={!dirty || save.isPending || current.isPending} onClick={() => save.mutate(selected || null)}>
              {t('common:save')}
            </Button>
          </div>
          {browser && browser !== selected && !(selected === '' && browser === host) && wholeHourZone(browser) && (
            <button
              type="button"
              className="self-start text-xs text-primary hover:underline disabled:opacity-50"
              disabled={save.isPending}
              onClick={() => setDraft(browser)}
            >
              {t('analytics:tzUseBrowser', { zone: browser })}
            </button>
          )}
        </div>
        {effective && (
          <p className="text-xs text-muted-foreground" data-testid="reporting-timezone-effective">
            {t('analytics:tzEffective', { zone: t('analytics:tzZoneWithOffset', { zone: effective, offset: utcOffsetLabel(effective) }).trim() })}
          </p>
        )}
        <p className="text-xs text-muted-foreground">{t('analytics:tzScope')}</p>
      </CardContent>
    </Card>
  )
}
