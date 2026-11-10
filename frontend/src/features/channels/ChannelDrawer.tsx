import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import type { ChannelRow } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Steps } from '@/components/ui/steps'
import { OAuthLoginSteps, useOAuthLogin } from '@/features/channels/OAuthLoginCard'
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
import { ConnectionSection, PROVIDER_LABELS } from './drawer/ConnectionSection'
import { ModelsSection } from './drawer/ModelsSection'
import { ScheduleSection } from './drawer/ScheduleSection'
import { useChannelForm } from './drawer/use-channel-form'

const EDIT_TABS = ['conn', 'models', 'sched', 'behavior'] as const
type EditTab = (typeof EDIT_TABS)[number]
/// 新建是一条引导：接入 → 模型与调度 →（订阅账号浏览器登录时）登录授权。
/// 订阅账号的渠道是「换取凭证」那一下建出来的，没有另外的保存——所以它是最后一步的主按钮，
/// 而不是埋在表单中段的一个按钮外加底部一个永远点不了的「新建」。
type CreateStep = 'conn' | 'models' | 'auth'

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
  const [step, setStep] = useState<CreateStep>('conn')
  const loginFlow = !isEdit && oauth && !importToken
  const login = useOAuthLogin({
    provider: form.provider,
    name: form.name,
    models,
    settings: channelSettingsForSave(settings),
    valid: controlValid && concurrencyValid && endpointValid && costMilli !== null && priority !== null && egressBinding !== null,
    creationOptions: { api_base: form.api_base, priority: priority ?? 0,
      pools: canManagePools ? newPools : undefined, cost_milli: costMilli ?? undefined,
      ...(egressBinding !== null && egressBinding.mode !== 'inherit' ? { egress: egressBinding } : {}) },
    maxConcurrency: concurrency ?? undefined,
    onDone: () => {
      onDone()
      onClose()
    },
  })
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
  const settingsValid = endpointValid && controlValid && concurrencyValid && costMilli !== null && priority !== null

  // 每一步还差什么：返回 null 才能往下走；文案显示在底部按钮旁，按钮不会无缘无故是灰的
  const steps: CreateStep[] = loginFlow ? ['conn', 'models', 'auth'] : ['conn', 'models']
  const current = steps.includes(step) ? step : 'conn'
  const missing: Record<CreateStep, string | null> = {
    conn: form.name.trim() === '' ? t('admin:channelStepNeedName')
      : !accountReady ? t('admin:channelStepNeedProvider')
      : !endpointValid ? t('admin:channelStepNeedEndpoint')
      : !loginFlow && credential.trim() === '' ? t('admin:channelStepNeedCredential')
      : egressBinding === null ? t('admin:channelStepNeedEgress')
      : null,
    models: models.length === 0 ? t('admin:channelStepNeedModels')
      : !settingsValid ? t('admin:channelStepNeedFix')
      : null,
    auth: login.started === null ? t('admin:oauthNeedOpen') : login.code.trim() === '' ? t('admin:oauthNeedCode') : null,
  }
  const firstBlocked = steps.findIndex((id) => missing[id] !== null)
  const reachable = firstBlocked === -1 ? steps.length - 1 : firstBlocked
  const index = steps.indexOf(current)
  const last = index === steps.length - 1
  const busy = create.isPending || save.isPending || login.exchange.isPending
  const body = useRef<HTMLDivElement>(null)
  // 换步后回到这一步的开头，焦点进第一个可填的控件（触屏不抢焦点，免得弹键盘）
  useEffect(() => {
    if (isEdit) return
    const panel = body.current?.querySelector<HTMLElement>('[data-step-active]')
    panel?.closest('[data-slot=drawer-body]')?.scrollTo({ top: 0 })
    if (window.matchMedia?.('(pointer: coarse)').matches) return
    panel?.querySelector<HTMLElement>('input:not([type=hidden]):not(:disabled), button:not(:disabled):not([aria-hidden])')?.focus({ preventScroll: true })
  }, [current, isEdit])
  const next = () => {
    if (missing[current] !== null) return
    if (!last) { setStep(steps[index + 1]!); return }
    if (loginFlow) login.exchange.mutate()
    else create.mutate()
  }
  const stepLabel: Record<CreateStep, string> = {
    conn: t('admin:channelStepConnection'), models: t('admin:channelStepModels'), auth: t('admin:channelStepAuth'),
  }

  return (
    <Drawer
      open
      onClose={onClose}
      title={isEdit ? t('admin:editChannel', { name: maskChannelName(channel.name, channel.keys.map((k) => k.account_label)) }) : t('admin:createChannel')}
      description={t(isEdit ? 'admin:channelDrawerDesc' : 'admin:channelCreateDesc')}
      icon={<ProviderMark provider={form.provider} />}
      size={isEdit ? 'lg' : 'md'}
      footer={isEdit ? (
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
      ) : (
        <>
          <p role="status" data-slot="step-hint" className="mr-auto min-w-0 basis-full text-xs leading-5 text-muted-foreground empty:hidden sm:basis-0 sm:flex-1">
            {missing[current] ?? ''}
          </p>
          {index === 0 ? (
            <Button variant="ghost" onClick={onClose}>{t('common:cancel')}</Button>
          ) : (
            <Button variant="outline" disabled={busy} onClick={() => setStep(steps[index - 1]!)}>
              <ChevronLeft aria-hidden className="h-4 w-4" />{t('common:back')}
            </Button>
          )}
          <Button className="px-5" loading={busy} disabled={missing[current] !== null || busy || (last && !loginFlow && !canSubmit)} onClick={next}>
            {!last ? <>{t('common:next')}<ChevronRight aria-hidden className="h-4 w-4" /></>
              : loginFlow ? t('admin:oauthCreateChannel') : t('common:create')}
          </Button>
        </>
      )}
    >
      <div ref={body} className="flex min-w-0 flex-col gap-4">
      {isEdit ? (
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
      ) : (
        <Steps ariaLabel={t('admin:channelSteps')} items={steps.map((id) => ({ id, label: stepLabel[id] }))}
          active={current} reachable={reachable} onSelect={(id) => setStep(id as CreateStep)} />
      )}

      {account.isError && <div className="flex items-center justify-between gap-2">
        <p role="alert" className="text-xs text-destructive">{describeError(account.error)}</p>
        <Button size="sm" variant="outline" disabled={account.isFetching} onClick={() => void account.refetch()}>{t('common:retry')}</Button>
      </div>}
      {isEdit ? (
        <div className="channel-editor">
          {tab === 'conn' && <ConnectionSection f={f} channel={channel} rotate={rotate} onDone={onDone} />}
          {tab === 'models' && <ModelsSection f={f} channel={channel} discover={discover} onDone={onDone} />}
          {tab === 'sched' && <ScheduleSection f={f} channel={channel} onDone={onDone} />}
          {tab === 'behavior' && <BehaviorSection f={f} channel={channel} />}
        </div>
      ) : (
        // 各步保持挂载、只隐藏：往回改一步不会丢后面已经填的内容（折叠段的展开状态、授权码都在）
        <>
          <div className="channel-editor" hidden={current !== 'conn'} data-step-active={current === 'conn' || undefined}>
            <ConnectionSection f={f} channel={undefined} rotate={rotate} onDone={onDone} />
          </div>
          <div className="channel-editor" hidden={current !== 'models'} data-step-active={current === 'models' || undefined}>
            <ModelsSection f={f} channel={undefined} discover={discover} onDone={onDone} />
            <ScheduleSection f={f} channel={undefined} onDone={onDone} />
          </div>
          {loginFlow && (
            <div className="channel-editor" hidden={current !== 'auth'} data-step-active={current === 'auth' || undefined}>
              <p data-slot="channel-create-summary" className="text-xs leading-5 text-muted-foreground">
                {t('admin:channelCreateSummary', {
                  name: form.name.trim(),
                  provider: PROVIDER_LABELS[form.provider] ? t(PROVIDER_LABELS[form.provider]!) : form.provider,
                  count: models.length,
                })}
              </p>
              <FieldGroup title={t('admin:oauthLoginTitle')} hint={t('admin:oauthLoginHint')}>
                <OAuthLoginSteps login={login} codeId="oauth-code" />
              </FieldGroup>
            </div>
          )}
        </>
      )}
      </div>
    </Drawer>
  )
}
