import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { ProviderMark } from '@/features/channels/ProviderChip'
import { channelSettingsForSave } from '@/features/channels/types'
import { Tabs } from '@/components/ui/tabs'
import { toast } from '@/components/ui/toast'
import { ApiError, apiFetch } from '@/lib/api'
import { configuredModelsOptions } from '@/features/models/configured-models'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { maskChannelName } from '@/lib/mask'
import { BehaviorSection } from './drawer/BehaviorSection'
import { ConnectionSection } from './drawer/ConnectionSection'
import { ModelsSection } from './drawer/ModelsSection'
import { ScheduleSection } from './drawer/ScheduleSection'
import { useChannelForm } from './drawer/use-channel-form'

const EDIT_TABS = ['conn', 'models', 'sched', 'behavior'] as const
type EditTab = (typeof EDIT_TABS)[number]

export function ChannelDrawer({
  channel,
  onClose,
  onDone,
}: {
  channel: ChannelRow | undefined
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const isEdit = channel !== undefined
  const f = useChannelForm(channel)
  const { canManagePools, form, account, authorization, accountReady, costMilli, models, setModels, credential, setCredential, oauthInput, rotationKeyId, settings, controlValid, concurrency, concurrencyValid, priority, newPools, egressBinding, oauth, importToken, endpointValid, setUnknownModels } = f
  const [tab, setTab] = useState<EditTab>('conn')
  const create = useMutation({
    mutationFn: () =>
      apiFetch('/admin/channels', {
        method: 'POST',
        body: {
          name: form.name,
          provider: form.provider,
          api_base: form.api_base,
          credential,
          models,
          priority: priority ?? 0,
          settings: channelSettingsForSave(settings),
          max_concurrency: concurrency ?? undefined,
          pools: canManagePools ? newPools : undefined,
          cost_milli: costMilli ?? undefined,
          // 继承是后端缺省：只在选了别的出口时才带，默认提交体保持不变
          ...(egressBinding !== null && egressBinding.mode !== 'inherit' ? { egress: egressBinding } : {}),
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channel?.id ?? 0}`, {
        method: 'PATCH',
        body: {
          name: form.name,
          api_base: form.api_base,
          models,
          priority: priority ?? 0,
          settings: channelSettingsForSave(settings),
          cost_milli: costMilli ?? undefined,
        },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const rotate = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channel?.id ?? 0}/credential`, {
        method: 'POST',
        body: { credential, ...(Boolean(authorization?.access_token_prefix) && oauthInput === 'import'
          && rotationKeyId !== undefined ? { channel_key_id: rotationKeyId } : {}) },
      }),
    onSuccess: () => {
      setCredential('')
      toast.success(t('admin:credentialRotated'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const discover = useMutation({
    mutationFn: async () => {
      const found = isEdit
        ? await queryClient.fetchQuery({
          queryKey: qk.channelModels(channel.id),
          queryFn: () => apiFetch<{ models: string[] }>(`/admin/channels/${channel.id}/fetch-models`),
        })
        : await apiFetch<{ models: string[] }>('/admin/channels/fetch-models', {
          method: 'POST',
          body: {
            provider: form.provider,
            api_base: form.api_base,
            credential,
            settings: channelSettingsForSave(settings),
            ...(egressBinding !== null && egressBinding.mode !== 'inherit' ? { egress: egressBinding } : {}),
          },
        })
      // 价簿里还没有这些名字的模型：请求会因未定价被拒。读不到价簿（没有定价读取权限）就不提示
      const priced = await queryClient.fetchQuery(configuredModelsOptions())
        .then((list) => new Set(list.data.map((model) => model.model_name))).catch(() => null)
      return { found: found.models, unknown: priced === null ? [] : found.models.filter((name) => !priced.has(name)) }
    },
    onSuccess: ({ found, unknown }) => {
      const added = found.filter((name) => !models.includes(name))
      // 并入而不是覆盖：手填的名字（别名、上游列表里没有的）不会被一键清掉
      setModels((current) => [...current, ...found.filter((name) => !current.includes(name))])
      setUnknownModels(unknown)
      toast.success(t('admin:discovered', { n: found.length, added: added.length }))
    },
    // 上游回了 200 却不是 JSON（站点的网页前端）：几乎都是地址缺了版本段，通用的「请求参数有误」说不清
    onError: (err) => toast.error(
      err instanceof ApiError && err.param === 'upstream_models_not_json' ? t('admin:discoverNotJson') : describeError(err),
    ),
  })
  const canSubmit = isEdit
    ? form.name.trim() !== ''
    : accountReady && (!oauth || importToken) && form.name.trim() !== '' && credential.trim() !== '' && models.length > 0
      && egressBinding !== null

  return (
    <Drawer
      open
      onClose={onClose}
      title={isEdit ? t('admin:editChannel', { name: maskChannelName(channel.name, channel.keys.map((k) => k.account_label)) }) : t('admin:createChannel')}
      description={t('admin:channelDrawerDesc')}
      icon={<ProviderMark provider={form.provider} />}
      size={isEdit ? 'lg' : 'md'}
      footer={
        <>
          <Button variant="ghost" className="rounded-lg" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            className="rounded-lg px-5 shadow-[0_5px_14px_-8px_var(--primary)]"
            loading={create.isPending || save.isPending}
            disabled={!canSubmit || !endpointValid || !controlValid || !concurrencyValid || costMilli === null || priority === null || create.isPending || save.isPending}
            onClick={() => (isEdit ? save.mutate() : create.mutate())}
          >
            {isEdit ? t('common:save') : t('common:create')}
          </Button>
        </>
      }
    >
      <div className="channel-editor">
      {isEdit && (
        <Tabs
          className="w-full [&_[role=tablist]]:w-full [&_[role=tab]]:flex-1 [&_[role=tab]]:justify-center"
          items={EDIT_TABS.map((id) => ({
            id,
            label: t(
              (
                {
                  conn: 'admin:groupBasic',
                  models: 'admin:channelTabModels',
                  sched: 'admin:groupSchedule',
                  behavior: 'admin:groupBehavior',
                } as const
              )[id],
            ),
          }))}
          active={tab}
          onChange={(id) => setTab(id as EditTab)}
        />
      )}

          {account.isError && <div className="flex items-center justify-between gap-2">
            <p role="alert" className="text-xs text-destructive">{describeError(account.error)}</p>
            <Button size="sm" variant="outline" disabled={account.isFetching} onClick={() => void account.refetch()}>{t('common:retry')}</Button>
          </div>}
      {(!isEdit || tab === 'conn') && <ConnectionSection f={f} channel={channel} rotate={rotate} onDone={onDone} onClose={onClose} />}
      {(!isEdit || tab === 'models') && <ModelsSection f={f} channel={channel} discover={discover} onDone={onDone} />}
      {(!isEdit || tab === 'sched') && <ScheduleSection f={f} channel={channel} onDone={onDone} />}
      {isEdit && tab === 'behavior' && <BehaviorSection f={f} channel={channel} />}
      </div>
    </Drawer>
  )
}
