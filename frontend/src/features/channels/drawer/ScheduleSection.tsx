import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Input, Label } from '@/components/ui/input'
import { Field } from '@/components/ui/field'
import { OptionalSection } from '@/components/ui/optional-section'
import { ChannelControlEditor } from '@/features/channels/ChannelControlEditor'
import type { ChannelForm } from './use-channel-form'
import { KeySchedule } from './key-panels'

/// 「调度」：优先级、相对成本、每把 key 的并发与权重、账号级控制。
export function ScheduleSection({ f, channel, onDone }: {
  f: ChannelForm
  channel: ChannelRow | undefined
  onDone: () => void
}) {
  const { t } = useTranslation()
  const isEdit = channel !== undefined
  const { form, setForm, costMilli, settings, setSettings, setControlValid, newConcurrency, setNewConcurrency, concurrencyValid, priority, importToken } = f
  const scheduleCustom = form.priority !== '0' || form.cost !== '1'
  const scheduleSummary = scheduleCustom
    ? t('admin:channelScheduleSummary', { priority: form.priority, cost: form.cost })
    : t('admin:channelOptionsDefault')
  return (
    <>
      <OptionalSection id="channel-schedule-options" title={t('admin:channelScheduleOptions')} hint={t('admin:groupScheduleHint')}
        summary={scheduleSummary}
        error={costMilli === null ? t('errors:bad_request', { param: 'cost_milli' })
          : priority === null ? t('errors:bad_request', { param: 'priority' }) : undefined}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="d-priority">{t('admin:priority')}</Label>
          <Input
            id="d-priority"
            className="w-24"
            inputMode="numeric"
            value={form.priority}
            aria-invalid={priority === null}
            onChange={(e) => setForm((f) => ({ ...f, priority: e.target.value }))}
          />
        </div>
        <Field
          label={t('admin:channelCostMilli')}
          htmlFor="d-cost"
          hint={t('admin:channelCostMilliHint')}
          error={costMilli === null ? t('errors:bad_request', { param: 'cost_milli' }) : undefined}
        >
          <div className="flex items-center gap-2">
            <span className="text-sm text-muted-foreground">×</span>
            <Input
              id="d-cost"
              className="w-24"
              inputMode="decimal"
              value={form.cost}
              onChange={(e) => setForm((f) => ({ ...f, cost: e.target.value }))}
            />
          </div>
        </Field>
      </OptionalSection>

      {isEdit && <KeySchedule channel={channel} onDone={onDone} />}
      <ChannelControlEditor value={settings.account_control}
        onChange={(account_control) => setSettings((s) => ({ ...s, account_control }))}
        onValidChange={setControlValid} provider={form.provider} channelId={channel?.id}
        concurrencyValid={concurrencyValid}
        summaryPrefix={!isEdit && newConcurrency ? t('admin:channelConcurrencySummary', { count: newConcurrency }) : undefined}
        refreshable={isEdit ? channel.keys.some((key) => key.credential_kind === 1 && key.oauth_refreshable !== false) : !importToken}>
        {!isEdit && <div className="flex min-w-0 flex-col gap-1.5">
          <Label htmlFor="d-concurrency">{t('admin:channelInitialConcurrency')}</Label>
          <Input id="d-concurrency" inputMode="numeric" value={newConcurrency} placeholder={t('admin:channelLimitUnlimited')}
            aria-invalid={!concurrencyValid} onChange={(e) => setNewConcurrency(e.target.value)} />
        </div>}
      </ChannelControlEditor>
    </>
  )
}
