import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { DEFAULT_PROBE_POLICY } from './types'
import type { ProbePolicy } from './types'

const KEY = 'egress_probe_policy'

/// 后台探测（`settings.egress_probe_policy`）：定时经每个启用的代理请求探测地址，刷新出口 IP / 延迟；
/// 出口 IP 变了、或有渠道在用的代理不可达时，按「通知渠道」里订阅的事件告警。只记事实，不影响熔断。
export function ProbePolicyCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const current = useQuery({
    queryKey: qk.setting(KEY),
    queryFn: () => apiFetch<{ value: Partial<ProbePolicy> | null }>(`/admin/settings/${KEY}`),
  })
  const stored: ProbePolicy = { ...DEFAULT_PROBE_POLICY, ...current.data?.value }
  const [draft, setDraft] = useState<{ minutes: string; target: string } | null>(null)
  const minutes = draft?.minutes ?? String(Math.round(stored.interval_secs / 60))
  const target = draft?.target ?? stored.target ?? ''
  const interval = Number(minutes) * 60
  const intervalValid = Number.isInteger(interval) && interval >= 60 && interval <= 86_400
  const targetValid = target.trim() === '' || /^https?:\/\/[^\s/]+/i.test(target.trim())

  const save = useMutation({
    mutationFn: (next: ProbePolicy) =>
      apiFetch('/admin/settings', { method: 'POST', body: { key: KEY, value: next } }),
    onSuccess: () => {
      toast.success(t('common:success'))
      setDraft(null)
      void current.refetch()
      void queryClient.invalidateQueries({ queryKey: qk.adminSettings })
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const next = (patch: Partial<ProbePolicy>): ProbePolicy => ({
    ...stored,
    interval_secs: intervalValid ? interval : stored.interval_secs,
    target: target.trim() === '' ? null : target.trim(),
    ...patch,
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:egressProbeTitle')}</CardTitle>
        <CardDescription>{t('admin:egressProbeDesc')}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <Switch
          label={t('admin:egressProbeEnabled')}
          description={t('admin:egressProbeEnabledHint')}
          checked={stored.enabled}
          disabled={current.isPending || save.isPending}
          onChange={(enabled) => save.mutate(next({ enabled }))}
        />
        {/* 顶部对齐：探测地址下面的说明较长，底部对齐会让两栏输入框错开 */}
        <div className="flex flex-wrap items-start gap-3">
          <Field
            label={t('admin:egressProbeInterval')}
            htmlFor="egress-probe-interval"
            error={intervalValid ? undefined : t('errors:bad_request', { param: 'interval_secs' })}
            className="w-40"
          >
            <Input
              id="egress-probe-interval"
              inputMode="numeric"
              value={minutes}
              aria-invalid={!intervalValid}
              onChange={(e) => setDraft({ minutes: e.target.value, target })}
            />
          </Field>
          <Field
            label={t('admin:egressProbeTarget')}
            htmlFor="egress-probe-target"
            hint={t('admin:egressProbeTargetHint')}
            error={targetValid ? undefined : t('errors:bad_request', { param: 'target' })}
            className="min-w-64 flex-1"
          >
            <Input
              id="egress-probe-target"
              className="font-mono text-sm"
              value={target}
              placeholder="https://www.cloudflare.com/cdn-cgi/trace"
              aria-invalid={!targetValid}
              onChange={(e) => setDraft({ minutes, target: e.target.value })}
            />
          </Field>
          <Button
            variant="outline"
            className="mt-6"
            disabled={draft === null || !intervalValid || !targetValid || save.isPending}
            onClick={() => save.mutate(next({}))}
          >
            {t('common:save')}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
