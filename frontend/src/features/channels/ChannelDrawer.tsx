import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Tabs } from '@/components/ui/tabs'
import { toast } from '@/components/ui/toast'
import { ProviderMark } from '@/features/channels/ProviderChip'
import { channelSettingsForSave } from '@/features/channels/types'
import { configuredModelsOptions } from '@/features/models/configured-models'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { maskChannelName } from '@/lib/mask'
import { qk } from '@/lib/query-keys'
import { BehaviorSection } from './drawer/BehaviorSection'
import { ConnectionSection } from './drawer/ConnectionSection'
import { ModelsSection } from './drawer/ModelsSection'
import { ScheduleSection } from './drawer/ScheduleSection'
import { useChannelForm } from './drawer/use-channel-form'

const EDIT_TABS = ['conn', 'models', 'sched', 'behavior'] as const
type EditTab = (typeof EDIT_TABS)[number]
export type ChannelEditFocus = 'conn' | 'egress' | 'models' | 'pools' | 'schedule' | 'keys' | 'limits'

function initialTabFor(focus: ChannelEditFocus | undefined): EditTab {
  switch (focus) {
    case 'models':
    case 'pools':
      return 'models'
    case 'schedule':
    case 'keys':
    case 'limits':
      return 'sched'
    default:
      return 'conn'
  }
}

const FOCUS_SECTION_ID: Partial<Record<ChannelEditFocus, string>> = {
  egress: 'channel-egress-edit',
  pools: 'channel-pools',
  schedule: 'channel-schedule-options',
  limits: 'channel-quota-limits',
}

/// 已有渠道快速编辑抽屉：与 `ChannelCreateWorkspace` 共用 `useChannelForm` 及分区组件；
/// 新建流程已迁入页级「新建渠道」引导页签，本抽屉专注列表页原地微调。
export function ChannelDrawer({
  channel,
  focus,
  onClose,
  onDone,
}: {
  channel: ChannelRow
  /** 打开时直接落到对应页签与折叠段：点列表中任一字段即可直达对应配置。 */
  focus?: ChannelEditFocus
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const f = useChannelForm(channel)
  const {
    form,
    account,
    authorization,
    costMilli,
    models,
    setModels,
    credential,
    setCredential,
    oauthInput,
    rotationKeyId,
    settings,
    controlValid,
    concurrencyValid,
    priority,
    endpointValid,
    setUnknownModels,
  } = f
  const [tab, setTab] = useState<EditTab>(() => initialTabFor(focus))

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channel.id}`, {
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
      apiFetch(`/admin/channels/${channel.id}/credential`, {
        method: 'POST',
        body: {
          credential,
          ...(Boolean(authorization?.access_token_prefix) &&
          oauthInput === 'import' &&
          rotationKeyId !== undefined
            ? { channel_key_id: rotationKeyId }
            : {}),
        },
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
      const found = await queryClient.fetchQuery({
        queryKey: qk.channelModels(channel.id),
        queryFn: () => apiFetch<{ models: string[] }>(`/admin/channels/${channel.id}/fetch-models`),
      })
      const priced = await queryClient
        .fetchQuery(configuredModelsOptions())
        .then((list) => new Set(list.data.map((model) => model.model_name)))
        .catch(() => null)
      return {
        found: found.models,
        unknown: priced === null ? [] : found.models.filter((name) => !priced.has(name)),
      }
    },
    onSuccess: ({ found, unknown }) => {
      const added = found.filter((name) => !models.includes(name))
      setModels((current) => [...current, ...found.filter((name) => !current.includes(name))])
      setUnknownModels(unknown)
      toast.success(t('admin:discovered', { n: found.length, added: added.length }))
    },
    onError: (err) =>
      toast.error(
        err instanceof ApiError && err.param === 'upstream_models_not_json'
          ? t('admin:discoverNotJson')
          : describeError(err),
      ),
  })

  const canSubmit = form.name.trim() !== ''
  const settingsValid =
    endpointValid && controlValid && concurrencyValid && costMilli !== null && priority !== null
  const body = useRef<HTMLDivElement>(null)

  // 从列表直达某个配置块时，打开抽屉后自动把它滚进视野，免得还要在抽屉里手动往下找。
  useEffect(() => {
    if (!focus) return
    const targetId = FOCUS_SECTION_ID[focus]
    if (!targetId) return
    const frame = requestAnimationFrame(() => {
      body.current
        ?.querySelector<HTMLElement>(`#${targetId}`)
        ?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
    })
    return () => cancelAnimationFrame(frame)
  }, [focus])

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:editChannel', {
        name: maskChannelName(
          channel.name,
          channel.keys.map((k) => k.account_label),
        ),
      })}
      description={t('admin:channelDrawerDesc')}
      icon={<ProviderMark provider={form.provider} />}
      size="lg"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            className="px-5"
            loading={save.isPending}
            disabled={!canSubmit || !settingsValid || save.isPending}
            onClick={() => save.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <div ref={body} className="flex min-w-0 flex-col gap-4">
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

        {account.isError && (
          <div className="flex items-center justify-between gap-2">
            <p role="alert" className="text-xs text-destructive">
              {describeError(account.error)}
            </p>
            <Button
              size="sm"
              variant="outline"
              disabled={account.isFetching}
              onClick={() => void account.refetch()}
            >
              {t('common:retry')}
            </Button>
          </div>
        )}

        <div className="channel-editor">
          {tab === 'conn' && (
            <ConnectionSection f={f} channel={channel} rotate={rotate} onDone={onDone} />
          )}
          {tab === 'models' && (
            <ModelsSection
              f={f}
              channel={channel}
              poolsOpen={focus === 'pools'}
              discover={discover}
              onDone={onDone}
            />
          )}
          {tab === 'sched' && (
            <ScheduleSection
              f={f}
              channel={channel}
              limitsOpen={focus === 'limits'}
              scheduleOpen={focus === 'schedule'}
              onDone={onDone}
            />
          )}
          {tab === 'behavior' && <BehaviorSection f={f} channel={channel} />}
        </div>
      </div>
    </Drawer>
  )
}
