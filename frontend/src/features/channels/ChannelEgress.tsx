import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { OptionalSection } from '@/components/ui/optional-section'
import { toast } from '@/components/ui/toast'
import type { ChannelRow } from '@/features/channels/types'
import { EgressPicker, describeEgress, draftOf, sameBinding, toBinding } from '@/features/proxies/EgressPicker'
import type { EgressDraft } from '@/features/proxies/EgressPicker'
import { proxyGroupOptions, proxyOptions } from '@/features/proxies/options'
import type { ReconcileReport } from '@/features/proxies/types'
import { useReportToast } from '@/features/proxies/report'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 新建渠道时的出口选择（随建渠道一起提交；OAuth 登录在换码前就按它选代理）。
export function NewChannelEgress({ value, onChange }: { value: EgressDraft; onChange: (next: EgressDraft) => void }) {
  const { t } = useTranslation()
  const proxies = useQuery(proxyOptions())
  const groups = useQuery(proxyGroupOptions())
  const binding = toBinding(value)
  return (
    <OptionalSection
      id="channel-egress"
      title={t('admin:egressTitle')}
      hint={t('admin:egressSectionHint')}
      summary={binding ? describeEgress(t, binding, proxies.data, groups.data) : t('admin:egressIncomplete')}
      error={binding === null ? t('admin:egressIncomplete') : undefined}
    >
      <EgressPicker value={value} onChange={onChange} idPrefix="new-egress" />
    </OptionalSection>
  )
}

/// 编辑渠道的出口：独立保存（审计语义与渠道配置不同）。固定分配组下逐把 key 显示分到的代理。
export function ChannelEgress({ channel, onDone }: { channel: ChannelRow; onDone: () => void }) {
  const { t } = useTranslation()
  const proxies = useQuery(proxyOptions())
  const groups = useQuery(proxyGroupOptions())
  const [draft, setDraft] = useState<EgressDraft>(() => draftOf(channel.egress))
  const binding = toBinding(draft)
  const dirty = !sameBinding(binding, channel.egress ?? { mode: 'inherit' })
  const report = useReportToast()
  const save = useMutation({
    mutationFn: () =>
      apiFetch<{ assignment?: ReconcileReport }>(`/admin/channels/${channel.id}/egress`, {
        method: 'POST',
        body: binding,
      }),
    onSuccess: (r) => {
      report(r.assignment)
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const assigned = (channel.keys ?? []).filter((k) => k.egress_proxy_id != null)
  const proxyName = (id: number) => proxies.data?.find((p) => p.id === id)?.name ?? `#${id}`
  return (
    <OptionalSection
      id="channel-egress-edit"
      title={t('admin:egressTitle')}
      hint={t('admin:egressSectionHint')}
      summary={describeEgress(t, channel.egress, proxies.data, groups.data)}
      defaultOpen
    >
      <EgressPicker value={draft} onChange={setDraft} idPrefix="edit-egress" />
      {assigned.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5 text-xs">
          <span className="text-muted-foreground">{t('admin:egressPinnedTo')}</span>
          {assigned.map((k) => (
            <Badge key={k.id} variant="outline">
              {(channel.keys ?? []).length > 1 ? `#${k.id} → ` : ''}
              {proxyName(k.egress_proxy_id as number)}
            </Badge>
          ))}
        </div>
      )}
      <Button
        size="sm"
        variant="outline"
        className="self-start"
        disabled={!dirty || binding === null || save.isPending}
        onClick={() => save.mutate()}
      >
        {t('admin:egressSave')}
      </Button>
    </OptionalSection>
  )
}

/// 批量设置出口（渠道列表勾选后）：比如把某个池的全部渠道一次换到同一个代理组。
/// 出口不挂在池上（一个渠道可在多个池里），按池批量即「选中池内渠道 → 这里设」。
export function BatchEgressDrawer({
  ids,
  onClose,
  onDone,
}: {
  ids: number[]
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState<EgressDraft>({ mode: 'inherit' })
  const binding = toBinding(draft)
  const report = useReportToast()
  const save = useMutation({
    mutationFn: () =>
      apiFetch<{ affected: number; assignment?: ReconcileReport }>('/admin/channels/batch', {
        method: 'POST',
        body: { ids, action: 'set_egress', egress: binding },
      }),
    onSuccess: (r) => {
      report(r.assignment ?? undefined)
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })
  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:egressBatchTitle', { n: ids.length })}
      description={t('admin:egressBatchDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button disabled={binding === null || save.isPending} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
        </>
      }
    >
      <EgressPicker value={draft} onChange={setDraft} idPrefix="batch-egress" />
    </Drawer>
  )
}
