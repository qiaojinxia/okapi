import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { EgressPicker, draftOf, sameBinding, toBinding } from './EgressPicker'
import type { EgressDraft } from './EgressPicker'
import { egressDefaultOptions } from './options'
import { useReportToast } from './report'
import type { ReconcileReport } from './types'

/// 全局默认出口（系统设置 →「出口代理」页签）：所有「跟随全局默认」的渠道走这里。
/// 服务器在受限网络、所有上游都要走代理时设一次即可。
export function DefaultEgressCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const can = usePermission()
  const current = useQuery(egressDefaultOptions())
  const [draft, setDraft] = useState<EgressDraft | null>(null)
  const report = useReportToast()
  const value = draft ?? draftOf(current.data?.egress)
  const binding = toBinding(value)
  const dirty = draft !== null && !sameBinding(binding, current.data?.egress ?? null)
  const save = useMutation({
    mutationFn: () =>
      apiFetch<{ assignment?: ReconcileReport }>('/admin/egress/default', {
        method: 'PUT',
        body: binding,
      }),
    onSuccess: (r) => {
      report(r.assignment)
      setDraft(null)
      void queryClient.invalidateQueries({ queryKey: qk.egressDefault })
      void queryClient.invalidateQueries({ queryKey: qk.adminProxies })
      void queryClient.invalidateQueries({ queryKey: qk.adminProxyGroups })
    },
    onError: (err) => toast.error(describeError(err)),
  })
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:egressDefaultTitle')}</CardTitle>
        <CardDescription>{t('admin:egressDefaultDesc')}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {current.isError ? (
          <ErrorState message={describeError(current.error)} onRetry={() => void current.refetch()} />
        ) : (
          <div className="flex flex-wrap items-start gap-3">
            <div className="min-w-0 flex-1">
              <EgressPicker value={value} onChange={setDraft} allowInherit={false} idPrefix="egress-default" />
            </div>
            <Button
              disabled={!dirty || binding === null || !can('channel.write') || save.isPending}
              onClick={() => save.mutate()}
            >
              {t('admin:egressDefaultSave')}
            </Button>
          </div>
        )}
        <Link to="/admin/proxies" className="inline-flex items-center gap-1 self-start text-xs text-primary hover:underline">
          {t('admin:egressManageProxies')}
          <ArrowUpRight aria-hidden className="h-3.5 w-3.5" />
        </Link>
      </CardContent>
    </Card>
  )
}
