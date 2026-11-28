import { useMutation } from '@tanstack/react-query'
import { Layers, Plus, SlidersHorizontal, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { Tabs } from '@/components/ui/tabs'
import { toast } from '@/components/ui/toast'
import { ModelPicker } from '@/features/channels/ModelPicker'
import { PoolMembershipEditor, defaultMembership } from '@/features/channels/PoolMembership'
import { ratioToCostMilli } from '@/features/channels/types'
import { ModelTagsInput } from '@/features/models/model-input'
import type { PoolMember } from '@/features/pools/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { parseInt32 } from '@/lib/int32'

type BatchTab = 'models' | 'schedule' | 'pools'
type ModelOp = 'add_models' | 'remove_models'

/// 渠道批量配置抽屉：选中多条渠道后一次批量增删模型、统一调优先级/成本系数或替换所属调度池。
export function BatchChannelsDrawer({
  ids,
  onClose,
  onDone,
}: {
  ids: number[]
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [tab, setTab] = useState<BatchTab>('models')
  const [modelOp, setModelOp] = useState<ModelOp>('add_models')
  const [models, setModels] = useState<string[]>([])
  const [priorityText, setPriorityText] = useState('')
  const [costText, setCostText] = useState('')
  const [pools, setPools] = useState<PoolMember[]>(defaultMembership)

  const priorityBlank = priorityText.trim() === ''
  const parsedPriority = priorityBlank ? undefined : parseInt32(priorityText)
  const priorityValid = priorityBlank || parsedPriority !== null

  const costBlank = costText.trim() === ''
  const parsedCostMilli = costBlank ? undefined : ratioToCostMilli(costText)
  const costValid = costBlank || parsedCostMilli !== null

  const canSubmit =
    tab === 'models'
      ? models.length > 0
      : tab === 'schedule'
        ? priorityValid && costValid && (!priorityBlank || !costBlank)
        : true

  const save = useMutation({
    mutationFn: () => {
      if (tab === 'models') {
        return apiFetch<{ affected: number }>('/admin/channels/batch', {
          method: 'POST',
          body: { ids, action: modelOp, models },
        })
      }
      if (tab === 'schedule') {
        return apiFetch<{ affected: number }>('/admin/channels/batch', {
          method: 'POST',
          body: {
            ids,
            action: 'set_schedule',
            ...(parsedPriority !== undefined ? { priority: parsedPriority } : {}),
            ...(parsedCostMilli !== undefined ? { cost_milli: parsedCostMilli } : {}),
          },
        })
      }
      return apiFetch<{ affected: number }>('/admin/channels/batch', {
        method: 'POST',
        body: { ids, action: 'set_pools', pools },
      })
    },
    onSuccess: (r) => {
      toast.success(t('admin:batchDone', { n: r.affected }))
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      size="lg"
      title={t('admin:channelBatchTitle', { n: ids.length })}
      description={t('admin:channelBatchDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            className="px-5"
            loading={save.isPending}
            disabled={!canSubmit || save.isPending}
            onClick={() => save.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <div className="flex min-w-0 flex-col gap-5">
        <Tabs
          className="w-full [&_[role=tablist]]:w-full [&_[role=tab]]:flex-1 [&_[role=tab]]:justify-center"
          active={tab}
          onChange={(id) => setTab(id as BatchTab)}
          items={[
            { id: 'models', label: t('admin:channelBatchTabModels'), icon: Layers },
            { id: 'schedule', label: t('admin:channelBatchTabSchedule'), icon: SlidersHorizontal },
            { id: 'pools', label: t('admin:channelBatchTabPools') },
          ]}
        />

        {tab === 'models' && (
          <div className="flex flex-col gap-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <Label className="text-[13px] text-foreground">{t('admin:channelBatchModelOp')}</Label>
              <Segmented<ModelOp>
                size="sm"
                ariaLabel={t('admin:channelBatchModelOp')}
                value={modelOp}
                onChange={setModelOp}
                options={[
                  { value: 'add_models', label: t('admin:channelBatchAddModels'), icon: Plus },
                  { value: 'remove_models', label: t('admin:channelBatchRemoveModels'), icon: Trash2 },
                ]}
              />
            </div>
            <p className="text-xs leading-5 text-muted-foreground">{t('admin:channelBatchModelsHint')}</p>
            <ModelPicker value={models} onChange={setModels} />
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="batch-models-manual">{t('admin:modelsManual')}</Label>
              <ModelTagsInput
                id="batch-models-manual"
                value={models}
                onChange={setModels}
                placeholder={t('admin:tagInputHint')}
              />
            </div>
          </div>
        )}

        {tab === 'schedule' && (
          <div className="flex flex-col gap-4">
            <p className="text-xs leading-5 text-muted-foreground">{t('admin:channelBatchScheduleHint')}</p>
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="batch-priority">{t('admin:priority')}</Label>
                <Input
                  id="batch-priority"
                  inputMode="numeric"
                  value={priorityText}
                  aria-invalid={!priorityValid}
                  placeholder={t('admin:channelBatchPriorityPlaceholder')}
                  onChange={(e) => setPriorityText(e.target.value)}
                />
                <p className="text-xs leading-5 text-muted-foreground">{t('admin:priorityHint')}</p>
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="batch-cost">{t('admin:channelCost')}</Label>
                <Input
                  id="batch-cost"
                  inputMode="decimal"
                  value={costText}
                  aria-invalid={!costValid}
                  placeholder={t('admin:channelBatchCostPlaceholder')}
                  onChange={(e) => setCostText(e.target.value)}
                />
                <p className="text-xs leading-5 text-muted-foreground">{t('admin:channelCostHint')}</p>
              </div>
            </div>
          </div>
        )}

        {tab === 'pools' && (
          <div className="flex flex-col gap-3">
            <p className="text-xs leading-5 text-muted-foreground">{t('admin:channelBatchPoolsHint')}</p>
            <PoolMembershipEditor value={pools} onChange={setPools} />
            {pools.length === 0 && (
              <p role="alert" className="rounded-lg border border-warning/35 bg-warning/10 px-3 py-2 text-xs text-warning">
                {t('admin:poolOrphanWarning')}
              </p>
            )}
          </div>
        )}
      </div>
    </Drawer>
  )
}
