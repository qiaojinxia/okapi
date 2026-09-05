import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 隐私与留痕（settings.record_ip_log）。
///
/// 来源 IP 此前一律记录且关不掉——文档写着这个开关，实现里全仓无人读它。开关收口在结算
/// 唯一入口，关掉后 PG 的 client_ip 列与 CH 明细同时不落；已记录的历史行不动。
export function PrivacyCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const current = useQuery({
    queryKey: ['setting', 'record_ip_log'],
    queryFn: () => apiFetch<{ value: boolean | null }>('/admin/settings/record_ip_log'),
  })
  // 缺省（键不存在）= 记录：存量站点一直在记，缺省关掉会让日志无声地少一列
  const enabled = current.data?.value !== false

  const save = useMutation({
    mutationFn: (next: boolean) =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'record_ip_log', value: next },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      void current.refetch()
      void queryClient.invalidateQueries({ queryKey: ['admin', 'settings'] })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:privacyTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <Switch
          label={t('admin:recordIpLog')}
          description={t('admin:recordIpLogHint')}
          checked={enabled}
          disabled={current.isPending || save.isPending}
          onChange={(next) => save.mutate(next)}
        />
      </CardContent>
    </Card>
  )
}
