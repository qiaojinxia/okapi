import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronLeft, ChevronRight, Compass, LayoutList, RotateCcw } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import {
  GuidedChecklist,
  GuidedStepIntro,
  GuidedSummaryCard,
  GuidedSummaryRow,
  GuidedWorkspace,
} from '@/components/ui/guided-workspace'
import { Segmented } from '@/components/ui/segmented'
import { toast } from '@/components/ui/toast'
import { OAuthLoginSteps, useOAuthLogin } from '@/features/channels/OAuthLoginCard'
import { ProviderChip, ProviderMark } from '@/features/channels/ProviderChip'
import { channelSettingsForSave, type ChannelRow } from '@/features/channels/types'
import { configuredModelsOptions } from '@/features/models/configured-models'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { ConnectionSection, PROVIDER_LABELS } from './drawer/ConnectionSection'
import { ModelsSection } from './drawer/ModelsSection'
import { ScheduleSection } from './drawer/ScheduleSection'
import { useChannelForm } from './drawer/use-channel-form'

type CreateStep = 'conn' | 'models' | 'auth'
type CreateMode = 'guided' | 'all'

const NOOP_ROTATE = { isPending: false, mutate: () => {} }

/// 页内新建渠道工作区：作为 `/admin/channels` 的独立页签运行，取代窄右侧抽屉。
///
/// 缺省为「引导模式」（接入 → 模型与调度 →〔订阅浏览器登录〕登录授权），并支持一键切换
/// 「完整表单」模式同屏填写（共享同一份表单状态，切换不丢草稿）；右侧常驻配置摘要与
/// 就绪检查清单，点击任意清单项即可跳转到对应步骤。
export function ChannelCreateWorkspace({
  template,
  onClearTemplate,
  onCancel,
  onCreated,
  onReset,
}: {
  template?: ChannelRow
  onClearTemplate?: () => void
  onCancel: () => void
  onCreated: () => void
  onReset: () => void
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const f = useChannelForm(undefined, template)
  const {
    canManagePools,
    form,
    account,
    accountReady,
    costMilli,
    models,
    setModels,
    credential,
    settings,
    controlValid,
    concurrency,
    concurrencyValid,
    priority,
    newPools,
    newEgress,
    egressBinding,
    oauth,
    importToken,
    endpointValid,
    setUnknownModels,
  } = f

  const [mode, setMode] = useState<CreateMode>('guided')
  const [step, setStep] = useState<CreateStep>('conn')
  const loginFlow = oauth && !importToken

  const login = useOAuthLogin({
    provider: form.provider,
    name: form.name,
    models,
    settings: channelSettingsForSave(settings),
    valid:
      controlValid &&
      concurrencyValid &&
      endpointValid &&
      costMilli !== null &&
      priority !== null &&
      egressBinding !== null,
    creationOptions: {
      api_base: form.api_base,
      priority: priority ?? 0,
      pools: canManagePools ? newPools : undefined,
      cost_milli: costMilli ?? undefined,
      ...(egressBinding !== null && egressBinding.mode !== 'inherit' ? { egress: egressBinding } : {}),
    },
    maxConcurrency: concurrency ?? undefined,
    onDone: onCreated,
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
    onSuccess: onCreated,
    onError: (err) => toast.error(describeError(err)),
  })

  const discover = useMutation({
    mutationFn: async () => {
      const found = await apiFetch<{ models: string[] }>('/admin/channels/fetch-models', {
        method: 'POST',
        body: {
          provider: form.provider,
          api_base: form.api_base,
          credential,
          settings: channelSettingsForSave(settings),
          ...(egressBinding !== null && egressBinding.mode !== 'inherit' ? { egress: egressBinding } : {}),
        },
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

  const canSubmit =
    accountReady &&
    (!oauth || importToken) &&
    form.name.trim() !== '' &&
    credential.trim() !== '' &&
    models.length > 0 &&
    egressBinding !== null
  const settingsValid =
    endpointValid && controlValid && concurrencyValid && costMilli !== null && priority !== null

  const steps: CreateStep[] = loginFlow ? ['conn', 'models', 'auth'] : ['conn', 'models']
  const current: CreateStep = steps.includes(step) ? step : 'conn'
  const missing: Record<CreateStep, string | null> = {
    conn:
      form.name.trim() === ''
        ? t('admin:channelStepNeedName')
        : !accountReady
          ? t('admin:channelStepNeedProvider')
          : !endpointValid
            ? t('admin:channelStepNeedEndpoint')
            : !loginFlow && credential.trim() === ''
              ? t('admin:channelStepNeedCredential')
              : egressBinding === null
                ? t('admin:channelStepNeedEgress')
                : null,
    models:
      models.length === 0
        ? t('admin:channelStepNeedModels')
        : !settingsValid
          ? t('admin:channelStepNeedFix')
          : null,
    auth:
      login.started === null
        ? t('admin:oauthNeedOpen')
        : login.code.trim() === ''
          ? t('admin:oauthNeedCode')
          : null,
  }
  const firstBlocked = steps.findIndex((id) => missing[id] !== null)
  const reachable = firstBlocked === -1 ? steps.length - 1 : firstBlocked
  const index = steps.indexOf(current)
  const last = index === steps.length - 1
  const busy = create.isPending || login.exchange.isPending
  const allMissing = steps.map((id) => missing[id]).find((msg) => msg !== null) ?? null

  const bodyRef = useRef<HTMLDivElement>(null)
  // 换步后将焦点送入当前步首个可填控件（优先输入框/下拉，避免落在快捷卡片上；触屏不抢焦点）
  useEffect(() => {
    if (mode !== 'guided') return
    const panel = bodyRef.current?.querySelector<HTMLElement>('[data-step-active]')
    if (window.matchMedia?.('(pointer: coarse)').matches) return
    const target =
      panel?.querySelector<HTMLElement>(
        'input:not([type=hidden]):not(:disabled), textarea:not(:disabled), select:not(:disabled)',
      ) ?? panel?.querySelector<HTMLElement>('button:not(:disabled):not([aria-hidden])')
    target?.focus({ preventScroll: true })
  }, [current, mode])

  const next = () => {
    if (mode === 'all') {
      if (allMissing !== null) return
      if (loginFlow) login.exchange.mutate()
      else create.mutate()
      return
    }
    if (missing[current] !== null) return
    if (!last) {
      setStep(steps[index + 1]!)
      return
    }
    if (loginFlow) login.exchange.mutate()
    else create.mutate()
  }

  const stepLabel: Record<CreateStep, string> = {
    conn: t('admin:channelStepConnection'),
    models: t('admin:channelStepModels'),
    auth: t('admin:channelStepAuth'),
  }
  const providerDisplay = PROVIDER_LABELS[form.provider]
    ? t(PROVIDER_LABELS[form.provider]!)
    : form.provider

  const authSummary = loginFlow
    ? t('admin:channelPreviewAuthOauth')
    : importToken
      ? t('admin:channelPreviewAuthImport')
      : t('admin:channelPreviewAuthKey')

  const egressSummary =
    newEgress.mode === 'inherit'
      ? t('admin:channelPreviewEgressInherit')
      : newEgress.mode === 'direct'
        ? t('admin:channelPreviewEgressDirect')
        : newEgress.mode === 'proxy'
          ? t('admin:channelPreviewEgressProxy')
          : t('admin:channelPreviewEgressGroup')

  const jumpToStep = (target: CreateStep) => {
    if (mode !== 'guided') return
    const targetIdx = steps.indexOf(target)
    if (targetIdx >= 0 && targetIdx <= reachable) setStep(target)
  }

  const checklist = [
    {
      id: 'name',
      label: t('admin:channelName'),
      done: form.name.trim() !== '',
      detail: form.name.trim() || undefined,
      onClick: () => jumpToStep('conn'),
    },
    {
      id: 'endpoint',
      label: t('admin:groupBasic'),
      done: accountReady && endpointValid,
      detail: providerDisplay,
      onClick: () => jumpToStep('conn'),
    },
    {
      id: 'credential',
      label: loginFlow ? t('admin:oauthLoginTitle') : t('admin:groupCredential'),
      done: loginFlow ? login.started !== null && login.code.trim() !== '' : credential.trim() !== '',
      detail: authSummary,
      onClick: () => jumpToStep(loginFlow ? 'auth' : 'conn'),
      disabled: loginFlow && steps.indexOf('auth') > reachable,
    },
    {
      id: 'egress',
      label: t('admin:channelStatusEgress'),
      done: egressBinding !== null,
      detail: egressSummary,
      onClick: () => jumpToStep('conn'),
    },
    {
      id: 'models',
      label: t('admin:groupModels'),
      done: models.length > 0 && settingsValid,
      detail:
        models.length > 0
          ? t('admin:channelModelCount', { n: models.length })
          : t('admin:channelPreviewModelsEmpty'),
      onClick: () => jumpToStep('models'),
      disabled: steps.indexOf('models') > reachable,
    },
  ]

  const aside = (
    <GuidedSummaryCard
      title={t('admin:channelLivePreviewTitle')}
      subtitle={t('admin:channelLivePreviewSub')}
    >
      <div className="flex items-center gap-3 rounded-xl border border-border/80 bg-card p-3 shadow-xs">
        <ProviderMark provider={form.provider} size="sm" />
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="truncate text-sm font-semibold text-foreground">
            {form.name.trim() || t('admin:channelPreviewUnnamed')}
          </span>
          <div className="flex flex-wrap items-center gap-1.5">
            <ProviderChip provider={form.provider} compact />
            <span className="text-[11px] text-muted-foreground">{authSummary}</span>
          </div>
        </div>
      </div>

      <div className="flex flex-col gap-2 border-b border-border/65 pb-3">
        <GuidedSummaryRow label={t('admin:provider')} value={providerDisplay} />
        <GuidedSummaryRow label={t('admin:channelStatusEgress')} value={egressSummary} />
        {canManagePools && (
          <GuidedSummaryRow
            label={t('admin:poolMembership')}
            value={newPools.map((m) => m.pool_code).join(', ') || '—'}
            mono
          />
        )}
        <GuidedSummaryRow
          label={t('admin:groupModels')}
          value={
            models.length > 0
              ? t('admin:channelModelCount', { n: models.length })
              : t('admin:channelPreviewModelsEmpty')
          }
        />
      </div>

      {models.length > 0 && (
        <div className="flex flex-wrap gap-1 border-b border-border/65 pb-3">
          {models.slice(0, 8).map((m) => (
            <code
              key={m}
              className="max-w-full truncate rounded bg-card px-1.5 py-0.5 font-mono text-[11px] text-foreground/85 ring-1 ring-inset ring-border/70"
            >
              {m}
            </code>
          ))}
          {models.length > 8 && (
            <span className="inline-flex items-center rounded bg-muted px-1.5 py-0.5 text-[11px] font-medium text-muted-foreground tabular-nums">
              +{models.length - 8}
            </span>
          )}
        </div>
      )}

      <GuidedChecklist items={checklist} />
    </GuidedSummaryCard>
  )

  return (
    <GuidedWorkspace
      dataSlot="channel-create-workspace"
      title={t('admin:createChannel')}
      description={t('admin:channelCreateDesc')}
      icon={<ProviderMark provider={form.provider} />}
      headerAction={
        <>
          <Segmented<CreateMode>
            size="sm"
            ariaLabel={t('admin:channelCreateMode')}
            dataSlot="channel-create-mode"
            value={mode}
            onChange={setMode}
            options={[
              { value: 'guided', label: t('admin:channelModeGuided'), icon: Compass },
              { value: 'all', label: t('admin:channelModeAll'), icon: LayoutList },
            ]}
          />
          <Button size="sm" variant="ghost" disabled={busy} onClick={onReset}>
            <RotateCcw aria-hidden className="h-3.5 w-3.5" />
            {t('admin:channelResetDraft')}
          </Button>
        </>
      }
      steps={
        mode === 'guided'
          ? {
              ariaLabel: t('admin:channelSteps'),
              items: steps.map((id) => ({ id, label: stepLabel[id] })),
              active: current,
              reachable,
              onSelect: (id) => setStep(id as CreateStep),
            }
          : undefined
      }
      aside={aside}
      hint={mode === 'guided' ? missing[current] : allMissing}
      footer={
        mode === 'guided' ? (
          <>
            {index === 0 ? (
              <Button variant="ghost" onClick={onCancel}>
                {t('common:cancel')}
              </Button>
            ) : (
              <Button variant="outline" disabled={busy} onClick={() => setStep(steps[index - 1]!)}>
                <ChevronLeft aria-hidden className="h-4 w-4" />
                {t('common:back')}
              </Button>
            )}
            <Button
              className="px-5"
              loading={busy}
              disabled={missing[current] !== null || busy || (last && !loginFlow && !canSubmit)}
              onClick={next}
            >
              {!last ? (
                <>
                  {t('common:next')}
                  <ChevronRight aria-hidden className="h-4 w-4" />
                </>
              ) : loginFlow ? (
                t('admin:oauthCreateChannel')
              ) : (
                t('common:create')
              )}
            </Button>
          </>
        ) : (
          <>
            <Button variant="ghost" onClick={onCancel}>
              {t('common:cancel')}
            </Button>
            <Button
              className="px-5"
              loading={busy}
              disabled={allMissing !== null || busy || (!loginFlow && !canSubmit)}
              onClick={next}
            >
              {loginFlow ? t('admin:oauthCreateChannel') : t('common:create')}
            </Button>
          </>
        )
      }
    >
      <div ref={bodyRef} className="flex min-w-0 flex-col gap-4">
        {template !== undefined && (
          <div
            role="status"
            data-slot="channel-template-banner"
            className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-primary/25 bg-primary/[0.06] px-4 py-2.5 text-xs text-foreground"
          >
            <span>{t('admin:channelTemplateBanner', { name: template.name })}</span>
            {onClearTemplate && (
              <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={onClearTemplate}>
                {t('admin:channelTemplateClear')}
              </Button>
            )}
          </div>
        )}
        {account.isError && (
          <div className="flex items-center justify-between gap-2 rounded-xl border border-destructive/30 bg-destructive/8 px-4 py-2.5">
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

        {/* 各步保持挂载、只隐藏：引导模式回头改步骤，或与完整表单互切，均不丢已填草稿与授权码 */}
        <div
          className="channel-editor"
          hidden={mode === 'guided' && current !== 'conn'}
          data-step-active={(mode === 'guided' && current === 'conn') || undefined}
        >
          {mode === 'guided' && (
            <GuidedStepIntro
              stepNumber={1}
              title={t('admin:channelStepConnection')}
              description={t('admin:channelStepConnIntro')}
            />
          )}
          <ConnectionSection
            f={f}
            channel={undefined}
            showQuickProviders
            rotate={NOOP_ROTATE}
            onDone={onCreated}
          />
        </div>

        <div
          className="channel-editor"
          hidden={mode === 'guided' && current !== 'models'}
          data-step-active={(mode === 'guided' && current === 'models') || undefined}
        >
          {mode === 'guided' && (
            <GuidedStepIntro
              stepNumber={2}
              title={t('admin:channelStepModels')}
              description={t('admin:channelStepModelsIntro')}
            />
          )}
          <ModelsSection f={f} channel={undefined} discover={discover} onDone={onCreated} />
          <ScheduleSection f={f} channel={undefined} onDone={onCreated} />
        </div>

        {loginFlow && (
          <div
            className="channel-editor"
            hidden={mode === 'guided' && current !== 'auth'}
            data-step-active={(mode === 'guided' && current === 'auth') || undefined}
          >
            {mode === 'guided' && (
              <GuidedStepIntro
                stepNumber={3}
                title={t('admin:channelStepAuth')}
                description={t('admin:channelStepAuthIntro')}
              />
            )}
            <p data-slot="channel-create-summary" className="text-xs leading-5 text-muted-foreground">
              {t('admin:channelCreateSummary', {
                name: form.name.trim(),
                provider: providerDisplay,
                count: models.length,
              })}
            </p>
            <FieldGroup title={t('admin:oauthLoginTitle')} hint={t('admin:oauthLoginHint')}>
              <OAuthLoginSteps login={login} codeId="oauth-code" />
            </FieldGroup>
          </div>
        )}
      </div>
    </GuidedWorkspace>
  )
}
