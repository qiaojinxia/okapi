import { ModelTagsInput } from '@/features/models/model-input'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelRow, ChannelSettings } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Field } from '@/components/ui/field'
import { OptionalSection } from '@/components/ui/optional-section'
import { KeyParamRow } from '@/features/channels/KeyParamRow'
import { OAuthKeyHealth } from '@/features/channels/oauth-key-health'
import { ModelPicker } from '@/features/channels/ModelPicker'
import { OAuthLoginCard } from '@/features/channels/OAuthLoginCard'
import { ClientProfileEditor } from '@/features/channels/ClientProfileEditor'
import { ChannelControlEditor } from '@/features/channels/ChannelControlEditor'
import { ChannelEgress, NewChannelEgress } from '@/features/channels/ChannelEgress'
import { toBinding } from '@/features/proxies/EgressPicker'
import type { EgressDraft } from '@/features/proxies/EgressPicker'
import { parseLimit } from './account-controls/policy'
import { useAccountCapabilities } from './account-controls/api'
import {
  PROVIDERS,
  apiBasePlaceholder,
  costMilliToRatio,
  channelSettingsForSave,
  defaultApiBase,
  defaultResponsesNative,
  isCloudManaged,
  ratioToCostMilli,
  readSettings,
  speaksOpenAi,
} from '@/features/channels/types'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Tabs } from '@/components/ui/tabs'
import { TagInput } from '@/components/ui/tag-input'
import {
  PoolMembership,
  PoolMembershipEditor,
  defaultMembership,
} from '@/features/channels/PoolMembership'
import type { PoolMember } from '@/features/pools/types'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

const EDIT_TABS = ['conn', 'models', 'sched', 'behavior'] as const
type EditTab = (typeof EDIT_TABS)[number]
const PROVIDER_LABELS: Record<string, string> = {
  anthropic_max: 'admin:providerClaudeSubscription',
  codex: 'admin:providerCodexSubscription',
}

function ExtraHeadersEditor({
  value,
  onChange,
}: {
  value: Record<string, string>
  onChange: (next: Record<string, string>) => void
}) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState<Array<[string, string]>>(() => Object.entries(value))
  const commit = (next: Array<[string, string]>) => {
    setDraft(next)
    onChange(Object.fromEntries(next.filter(([k]) => k.trim() !== '')))
  }
  const setRow = (index: number, name: string, val: string) => {
    const next = [...draft]
    next[index] = [name, val]
    commit(next)
  }
  const removeRow = (index: number) => {
    commit(draft.filter((_, i) => i !== index))
  }
  return (
    <div className="flex flex-col gap-1.5">
      <Label>{t('admin:extraHeaders')}</Label>
      <p className="text-xs text-muted-foreground">{t('admin:extraHeadersHint')}</p>
      {draft.map(([name, val], i) => (
        <div key={i} className="flex gap-2">
          <Input
            value={name}
            placeholder="OpenAI-Organization"
            onChange={(e) => setRow(i, e.target.value, val)}
          />
          <Input value={val} placeholder="org-…" onChange={(e) => setRow(i, name, e.target.value)} />
          <Button type="button" variant="ghost" onClick={() => removeRow(i)}>
            ×
          </Button>
        </div>
      ))}
      <Button type="button" variant="outline" onClick={() => commit([...draft, ['', '']])}>
        {t('admin:extraHeadersAdd')}
      </Button>
    </div>
  )
}

function parseInjectValue(raw: string): unknown {
  const trimmed = raw.trim()
  if (trimmed === '') return ''
  try {
    return JSON.parse(trimmed) as unknown
  } catch {
    return raw
  }
}

function formatInjectValue(value: unknown): string {
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

function InjectFieldsEditor({
  value,
  onChange,
}: {
  value: Record<string, unknown>
  onChange: (next: Record<string, unknown>) => void
}) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState<Array<[string, string]>>(() =>
    Object.entries(value).map(([k, v]) => [k, formatInjectValue(v)]),
  )
  const commit = (next: Array<[string, string]>) => {
    setDraft(next)
    const obj: Record<string, unknown> = {}
    for (const [k, v] of next) {
      if (k.trim() === '') continue
      obj[k.trim()] = parseInjectValue(v)
    }
    onChange(obj)
  }
  const setRow = (index: number, name: string, val: string) => {
    const next = [...draft]
    next[index] = [name, val]
    commit(next)
  }
  const removeRow = (index: number) => {
    commit(draft.filter((_, i) => i !== index))
  }
  return (
    <div className="flex flex-col gap-1.5">
      <Label>{t('admin:injectFields')}</Label>
      <p className="text-xs text-muted-foreground">{t('admin:injectFieldsHint')}</p>
      {draft.map(([name, val], i) => (
        <div key={i} className="flex gap-2">
          <Input
            value={name}
            placeholder="temperature"
            onChange={(e) => setRow(i, e.target.value, val)}
          />
          <Input
            value={val}
            placeholder='0.2 or "forced"'
            onChange={(e) => setRow(i, name, e.target.value)}
          />
          <Button type="button" variant="ghost" onClick={() => removeRow(i)}>
            ×
          </Button>
        </div>
      ))}
      <Button type="button" variant="outline" onClick={() => commit([...draft, ['', '']])}>
        {t('admin:injectFieldsAdd')}
      </Button>
    </div>
  )
}

/// 渠道抽屉：新建与编辑共用，但形态不同。
///
/// - 新建只问三件必答事（接入 / 凭证 / 模型），行为开关与调度参数走缺省——
///   建渠道时用户根本还不知道要不要 thinking 转正文，问了也是瞎选；
/// - 编辑按"接入 / 模型 / 调度 / 行为"分页签：此前六段纵排滚起来找不到北。
///   表单状态提升在抽屉层，切页签不丢改动；底部"保存"提交全部页签的字段。
///
/// 凭证轮换、模型发现、per-key 参数、可见性各自是独立端点（审计语义不同），
/// 故它们在所属页签内单独提交，不并进主"保存"。
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
  const [tab, setTab] = useState<EditTab>('conn')
  const [form, setForm] = useState({
    name: channel?.name ?? '',
    provider: channel?.provider ?? 'openai',
    api_base: channel?.api_base ?? '',
    priority: String(channel?.priority ?? 0),
    cost: costMilliToRatio(channel?.cost_milli ?? 1000),
    dataRetention: channel?.data_retention ?? '',
  })
  const account = useAccountCapabilities(form.provider)
  const authorization = account.capabilities?.authorization
  const accountReady = account.isSuccess
  const costMilli = ratioToCostMilli(form.cost)
  const [models, setModels] = useState<string[]>(channel?.models ?? [])
  const [credential, setCredential] = useState('')
  const [oauthInput, setOauthInput] = useState<'login' | 'import'>(
    channel?.keys.some((key) => key.oauth_refreshable === false)
      ? 'import' : 'login',
  )
  const [rotationKeyId, setRotationKeyId] = useState<number | undefined>(
    channel?.keys.length === 1 ? channel.keys[0]?.id : undefined,
  )
  const [settings, setSettings] = useState<ChannelSettings>(readSettings(channel?.settings ?? null))
  const [controlValid, setControlValid] = useState(true)
  const [newConcurrency, setNewConcurrency] = useState('')
  const concurrency = parseLimit(newConcurrency)
  const concurrencyValid = concurrency !== null && (concurrency === undefined || concurrency <= 2_147_483_647)
  // 新建时的池成员关系：缺省只进 default 池（建完即对 default 分组可用）；
  // 渠道只服务它所在的池，全站分组都配了专属池的站点在这里勾对应的池
  const [newPools, setNewPools] = useState<PoolMember[]>(defaultMembership)
  // 新建时的出口（§11.41）：缺省继承全局默认；OAuth 登录在换码前就按它选代理
  const [newEgress, setNewEgress] = useState<EgressDraft>({ mode: 'inherit' })
  const egressBinding = toBinding(newEgress)

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
          priority: Number(form.priority) || 0,
          settings: channelSettingsForSave(settings),
          max_concurrency: concurrency ?? undefined,
          pools: newPools,
          cost_milli: costMilli ?? undefined,
          // 空串 = 清除声明；后端据此把键从 settings 里删掉
          data_retention: form.dataRetention,
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
          priority: Number(form.priority) || 0,
          settings: channelSettingsForSave(settings),
          cost_milli: costMilli ?? undefined,
          data_retention: form.dataRetention,
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

  // Login uses exchange; imported Claude tokens use the ordinary channel create endpoint.
  const oauth = Boolean(authorization)
  const importToken = Boolean(authorization?.access_token_prefix) && oauthInput === 'import'
  const defaultBase = account.descriptor?.default_base ?? defaultApiBase(form.provider)
  const endpointRequired = defaultBase === undefined
  const endpointValid = !endpointRequired || form.api_base.trim() !== ''
  const scheduleCustom = form.priority !== '0' || form.cost !== '1' || form.dataRetention !== ''
  const retentionLabels = { none: 'admin:channelRetentionNone', transient: 'admin:channelRetentionTransient', trains: 'admin:channelRetentionTrains' } as const
  const scheduleSummary = scheduleCustom ? [
    t('admin:channelScheduleSummary', { priority: form.priority, cost: form.cost }),
    form.dataRetention && t(retentionLabels[form.dataRetention as keyof typeof retentionLabels] ?? 'admin:channelRetentionUnset'),
  ].filter(Boolean).join(' · ') : t('admin:channelOptionsDefault')
  const behaviorCustom = settings.thinking_to_content || settings.bill_by_response_model
    || settings.responses_native !== undefined && settings.responses_native !== defaultResponsesNative(form.provider)
    || settings.strip_request_fields.length > 0 || Object.keys(settings.inject_request_fields ?? {}).length > 0
    || Object.keys(settings.extra_headers ?? {}).length > 0
  const baseField = <div className="flex flex-col gap-1.5">
    <Label htmlFor="d-base">{t('admin:apiBase')}</Label>
    <Input id="d-base" value={form.api_base} placeholder={defaultBase ?? apiBasePlaceholder(form.provider)} required={endpointRequired}
      onChange={(e) => setForm((f) => ({ ...f, api_base: e.target.value }))} />
    {form.provider === 'azure' && <p className="text-xs text-muted-foreground">{t('admin:azureApiBaseHint')}</p>}
    {form.provider === 'bedrock' && <p className="text-xs text-muted-foreground">{t('admin:bedrockApiBaseHint')}</p>}
    {form.provider === 'vertex' && <p className="text-xs text-muted-foreground">{t('admin:vertexApiBaseHint')}</p>}
  </div>
  const canSubmit = isEdit
    ? form.name.trim() !== ''
    : accountReady && (!oauth || importToken) && form.name.trim() !== '' && credential.trim() !== '' && models.length > 0
      && egressBinding !== null

  return (
    <Drawer
      open
      onClose={onClose}
      title={isEdit ? t('admin:editChannel', { name: channel.name }) : t('admin:createChannel')}
      description={t('admin:channelDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={!canSubmit || !endpointValid || !controlValid || !concurrencyValid || costMilli === null || create.isPending || save.isPending}
            onClick={() => (isEdit ? save.mutate() : create.mutate())}
          >
            {isEdit ? t('common:save') : t('common:create')}
          </Button>
        </>
      }
    >
      {isEdit && (
        <Tabs
          className="mb-4"
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
      {(!isEdit || tab === 'conn') && (
        <>
          <FieldGroup title={t('admin:groupBasic')} hint={t('admin:groupBasicHint')}>
            <div className="grid grid-cols-2 gap-3">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="d-name">{t('admin:channelName')}</Label>
                <Input
                  id="d-name"
                  value={form.name}
                  onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="d-provider">{t('admin:provider')}</Label>
                {isEdit ? (
                  // 协议决定请求转换路径，改了等于换渠道语义；已有渠道只读，需要换就新建
                  <Input id="d-provider" value={PROVIDER_LABELS[form.provider] ? t(PROVIDER_LABELS[form.provider]!) : form.provider} readOnly className="opacity-60" />
                ) : (
                  <Select
                    id="d-provider"
                    value={form.provider}
                    onChange={(v) => {
                      setForm((f) => ({ ...f, provider: v }))
                      setCredential('')
                      setOauthInput('login')
                      setSettings(({ extensions: _drop, ...s }) => s)
                    }}
                    options={(account.data?.data.length ? account.data.data.map((item) => item.id) : PROVIDERS).map((p) => ({ value: p, label: PROVIDER_LABELS[p] ? t(PROVIDER_LABELS[p]!) : p }))}
                  />
                )}
              </div>
              {endpointRequired && <div className="col-span-2">{baseField}</div>}
            </div>
          </FieldGroup>
          {(!endpointRequired || form.provider === 'bedrock' || form.provider === 'azure') && (
            <OptionalSection key={form.provider} id="channel-connection-options" title={t('admin:channelConnectionOptions')}
              summary={t(!endpointRequired && form.api_base.trim() !== '' && form.api_base.replace(/\/$/, '') !== defaultBase?.replace(/\/$/, '')
                || form.provider === 'bedrock' && settings.aws_region || form.provider === 'azure' && settings.api_version
                ? 'admin:channelOptionsCustomized' : endpointRequired ? 'admin:channelOptionsDefault' : 'admin:channelConnectionDefault')}
              hint={t('admin:channelConnectionOptionsHint')}>
              {!endpointRequired && baseField}
              {form.provider === 'codex' && <p role="note" className="text-xs leading-5 text-muted-foreground">{t('admin:codexEndpointHint')}</p>}
              {form.provider === 'bedrock' && (
                <div className="col-span-2">
                  <Field
                    label={t('admin:bedrockRegion')}
                    htmlFor="d-aws-region"
                    hint={t('admin:bedrockRegionHint')}
                  >
                    <Input
                      id="d-aws-region"
                      className="w-56"
                      value={settings.aws_region ?? ''}
                      placeholder="us-east-1"
                      onChange={(e) => {
                        const v = e.target.value.trim()
                        // 空 = 从 api_base 主机名解析：删键而不是存空串
                        setSettings(({ aws_region: _drop, ...s }) =>
                          v === '' ? s : { ...s, aws_region: v },
                        )
                      }}
                    />
                  </Field>
                </div>
              )}
              {form.provider === 'azure' && (
                <div className="col-span-2">
                  <Field
                    label={t('admin:azureApiVersion')}
                    htmlFor="d-api-version"
                    hint={t('admin:azureApiVersionHint')}
                  >
                    <Input
                      id="d-api-version"
                      className="w-56"
                      value={settings.api_version ?? ''}
                      placeholder="2024-10-21"
                      onChange={(e) => {
                        const v = e.target.value.trim()
                        // 空 = 跟随后端缺省：把键删掉，而不是存一个空串让后端去猜
                        setSettings(({ api_version: _drop, ...s }) =>
                          v === '' ? s : { ...s, api_version: v },
                        )
                      }}
                    />
                  </Field>
                </div>
              )}
            </OptionalSection>
          )}

          {authorization?.access_token_prefix && (
            <div data-slot="channel-auth-method" className="flex pb-4">
              <Tabs ariaLabel={t('admin:oauthInputMethod')} active={oauthInput}
                items={[{ id: 'login', label: t('admin:oauthBrowserLogin') }, { id: 'import', label: t('admin:oauthTokenImport') }]}
                onChange={(mode) => {
                  setOauthInput(mode as 'login' | 'import')
                  setCredential('')
                  if (!isEdit && mode === 'import') setSettings((s) => ({ ...s, extensions: {
                    ...s.extensions, ...(authorization.import_profile ? { client_profile: authorization.import_profile } : {}),
                  } }))
                }} />
            </div>
          )}
          {oauth && !importToken ? (
            <FieldGroup title={t('admin:oauthLoginTitle')} hint={t('admin:oauthLoginHint')}>
              {isEdit && channel.keys.length > 1 && <div>
                <Label htmlFor="d-reauthorization-key">{t('admin:oauthTokenTarget')}</Label>
                <Select id="d-reauthorization-key" value={rotationKeyId === undefined ? '' : String(rotationKeyId)}
                  placeholder={t('admin:oauthTokenTarget')} onChange={(value) => setRotationKeyId(Number(value))}
                  options={channel.keys.map((key) => ({ value: String(key.id), label: `#${key.id}` }))} />
              </div>}
              <OAuthLoginCard
                provider={form.provider}
                name={form.name}
                models={models}
                settings={channelSettingsForSave(settings)}
                valid={controlValid && concurrencyValid && (isEdit ? rotationKeyId !== undefined
                  : endpointValid && costMilli !== null && egressBinding !== null)}
                creationOptions={!isEdit ? { api_base: form.api_base, priority: Number(form.priority) || 0,
                  pools: newPools, cost_milli: costMilli ?? undefined, data_retention: form.dataRetention,
                  ...(egressBinding !== null && egressBinding.mode !== 'inherit' ? { egress: egressBinding } : {}) }
                  : undefined}
                maxConcurrency={concurrency ?? undefined}
                channelId={channel?.id}
                channelKeyId={isEdit ? rotationKeyId : undefined}
                onDone={() => {
                  onDone()
                  if (!isEdit) onClose()
                }}
              />
            </FieldGroup>
          ) : (
          <FieldGroup title={importToken ? t('admin:oauthTokenImport') : t('admin:groupCredential')}
            hint={importToken ? t('admin:oauthTokenImportHint') : t('admin:groupCredentialHint')}>
            {importToken && isEdit && channel.keys.length > 1 && (
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="d-rotation-key">{t('admin:oauthTokenTarget')}</Label>
                <Select id="d-rotation-key" value={rotationKeyId === undefined ? '' : String(rotationKeyId)}
                  placeholder={t('admin:oauthTokenTarget')} onChange={(value) => setRotationKeyId(Number(value))}
                  options={channel.keys.map((key) => ({ value: String(key.id), label: `#${key.id}` }))} />
              </div>
            )}
            <div className="flex items-end gap-2">
              <div className="flex flex-1 flex-col gap-1.5">
                <Label htmlFor="d-cred">
                  {isEdit ? t('admin:rotateCredential') : t('admin:credential')}
                </Label>
                <Input
                  id="d-cred"
                  type={importToken ? 'password' : 'text'}
                  autoComplete="off"
                  value={credential}
                  placeholder={
                    importToken ? 'sk-ant-oat01-…'
                    : form.provider === 'bedrock'
                      ? 'AKIA…:SECRET[:SESSION_TOKEN]'
                      : form.provider === 'vertex'
                        ? '{"type":"service_account",…}'
                        : 'sk-...'
                  }
                  onChange={(e) => setCredential(e.target.value)}
                />
                {form.provider === 'bedrock' && (
                  <p className="text-xs text-muted-foreground">{t('admin:bedrockCredentialHint')}</p>
                )}
                {form.provider === 'vertex' && (
                  <p className="text-xs text-muted-foreground">{t('admin:vertexCredentialHint')}</p>
                )}
              </div>
              {isEdit && (
                <Button
                  variant="outline"
                  disabled={!accountReady || credential.trim() === '' || rotate.isPending
                    || importToken && channel.keys.length > 1 && rotationKeyId === undefined}
                  onClick={() => rotate.mutate()}
                >
                  {t('admin:rotate')}
                </Button>
              )}
            </div>
          </FieldGroup>
          )}
          {!isEdit && (form.provider === 'anthropic' || form.provider === 'anthropic_max')
            && (!oauth || importToken) && (
            <ClientProfileEditor settings={settings} onChange={setSettings} />
          )}
          {isEdit && <CredentialStatus channel={channel} onDone={onDone} />}
          {/* 出口与凭证同属「接入」：订阅账号的登录、刷新、额度查询、测活都从这个出口出去 */}
          {isEdit ? <ChannelEgress channel={channel} onDone={onDone} />
            : <NewChannelEgress value={newEgress} onChange={setNewEgress} />}
        </>
      )}

      {(!isEdit || tab === 'models') && (
        <FieldGroup
          title={t('admin:groupModels')}
          hint={
            form.provider === 'azure'
              ? t('admin:azureModelsHint')
              : isCloudManaged(form.provider)
                ? t('admin:cloudModelsHint')
                : oauth
                  ? t('admin:oauthModelsHint')
                  : t('admin:groupModelsHint')
          }
        >
          <ModelPicker value={models} onChange={setModels} />
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="d-models">{t('admin:modelsManual')}</Label>
            <ModelTagsInput
              id="d-models"
              value={models}
              onChange={setModels}
              placeholder={t('admin:tagInputHint')}
            />
          </div>
          {isEdit && (
            <Button
              size="sm"
              variant="outline"
              className="self-start"
              onClick={() =>
                void queryClient
                  .fetchQuery({
                    queryKey: qk.channelModels(channel.id),
                    queryFn: () =>
                      apiFetch<{ models: string[] }>(`/admin/channels/${channel.id}/fetch-models`),
                  })
                  .then((r) => {
                    setModels(r.models)
                    toast.success(t('admin:discovered', { n: r.models.length }))
                  })
                  .catch((err: unknown) => toast.error(describeError(err)))
              }
            >
              {t('admin:fetchModels')}
            </Button>
          )}
        </FieldGroup>
      )}
      {isEdit && tab === 'models' && (
        <PoolMembership
          channelId={channel.id}
          current={channel.pool_members ?? []}
          onDone={onDone}
        />
      )}

      {!isEdit && (
        <OptionalSection id="channel-pools" title={t('admin:poolMembership')} hint={t('admin:poolMembershipHint')}
          summary={newPools.map((member) => member.pool_code + (member.priority_override !== null || member.weight_override !== null
            ? ` (${t('admin:channelOptionsCustomized')})` : '')).join(', ') || t('admin:channelPoolsEmpty')}
          error={newPools.length === 0 ? t('admin:poolOrphanWarning') : undefined}>
          <PoolMembershipEditor value={newPools} onChange={setNewPools} />
        </OptionalSection>
      )}

      {(!isEdit || tab === 'sched') && (
          <OptionalSection id="channel-schedule-options" title={t('admin:channelScheduleOptions')} hint={t('admin:groupScheduleHint')}
            summary={scheduleSummary}
            error={costMilli === null ? t('errors:bad_request', { param: 'cost_milli' }) : undefined}>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="d-priority">{t('admin:priority')}</Label>
              <Input
                id="d-priority"
                className="w-24"
                inputMode="numeric"
                value={form.priority}
                onChange={(e) => setForm((f) => ({ ...f, priority: e.target.value }))}
              />
            </div>
            <Field
              label={t('admin:channelDataRetention')}
              htmlFor="d-retention"
              hint={t('admin:channelDataRetentionHint')}
            >
              <Select
                id="d-retention"
                className="w-44"
                value={form.dataRetention}
                onChange={(v) => setForm((f) => ({ ...f, dataRetention: v }))}
                placeholder={t('admin:channelRetentionUnset')}
                options={[
                  { value: 'none', label: t('admin:channelRetentionNone') },
                  { value: 'transient', label: t('admin:channelRetentionTransient') },
                  { value: 'trains', label: t('admin:channelRetentionTrains') },
                ]}
              />
            </Field>
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
      )}

      {isEdit && tab === 'sched' && <KeySchedule channel={channel} onDone={onDone} />}
      {(!isEdit || tab === 'sched') && <ChannelControlEditor value={settings.account_control}
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
      </ChannelControlEditor>}

      {isEdit && tab === 'behavior' && (channel.provider === 'anthropic' || channel.provider === 'anthropic_max') && (
        <ClientProfileEditor settings={settings} onChange={setSettings} />
      )}
      {isEdit && tab === 'behavior' && (
        <OptionalSection id="channel-behavior-options" title={t('admin:groupBehavior')} hint={t('admin:groupBehaviorHint')} defaultOpen
          summary={t(behaviorCustom ? 'admin:channelOptionsCustomized' : 'admin:channelOptionsDefault')}>
          <Switch
            label={t('admin:thinkingToContent')}
            description={t('admin:thinkingToContentHint')}
            checked={settings.thinking_to_content}
            onChange={(v) => setSettings((s) => ({ ...s, thinking_to_content: v }))}
          />
          <Switch
            label={t('admin:billByResponseModel')}
            description={t('admin:billByResponseModelHint')}
            checked={settings.bill_by_response_model}
            onChange={(v) => setSettings((s) => ({ ...s, bill_by_response_model: v }))}
          />
          {speaksOpenAi(channel.provider) && (
            <Switch
              label={t('admin:responsesNative')}
              description={t('admin:responsesNativeHint')}
              checked={settings.responses_native ?? defaultResponsesNative(channel.provider)}
              onChange={(v) => setSettings((s) => ({ ...s, responses_native: v }))}
            />
          )}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="d-strip">{t('admin:stripFields')}</Label>
            <p className="text-xs text-muted-foreground">{t('admin:stripFieldsHint')}</p>
            <TagInput
              id="d-strip"
              value={settings.strip_request_fields}
              onChange={(v) => setSettings((s) => ({ ...s, strip_request_fields: v }))}
              placeholder="logit_bias"
            />
          </div>
          <InjectFieldsEditor
            value={settings.inject_request_fields ?? {}}
            onChange={(inject_request_fields) =>
              setSettings(({ inject_request_fields: _drop, ...s }) =>
                Object.keys(inject_request_fields).length === 0 ? s : { ...s, inject_request_fields },
              )
            }
          />
          <ExtraHeadersEditor
            value={settings.extra_headers ?? {}}
            onChange={(extra_headers) =>
              setSettings(({ extra_headers: _drop, ...s }) =>
                Object.keys(extra_headers).length === 0 ? s : { ...s, extra_headers },
              )
            }
          />
        </OptionalSection>
      )}
    </Drawer>
  )
}

/// 每把 key 的并发与权重属于调度；凭证状态（到期、刷新、重新授权）由 `CredentialStatus` 放在接入信息。
function KeySchedule({ channel, onDone }: { channel: ChannelRow; onDone: () => void }) {
  const { t } = useTranslation()
  const keys = channel.keys ?? []
  const refreshEnabled = channel.settings?.account_control?.refresh_mode !== 'external'
  const rows = keys.map((k) => (
    <KeyParamRow key={k.id} channelId={channel.id} provider={channel.provider} row={k}
      showIdentity={keys.length > 1} showCredential={false} refreshEnabled={refreshEnabled} onDone={onDone} />
  ))
  // 折叠区块包在 key 行里时拿不到同级的 first/last 间距规则，这里补上与其他区块一致的分隔和留白
  return (
    <div className="border-t border-border py-4">
      {keys.length <= 1 ? rows
        : <FieldGroup title={t('admin:channelKeys')} hint={t('admin:channelKeysHint')}>{rows}</FieldGroup>}
    </div>
  )
}

/// 订阅凭证的当前状态与它的操作，和上方的凭证输入同属「接入」。
function CredentialStatus({ channel, onDone }: { channel: ChannelRow; onDone: () => void }) {
  const keys = (channel.keys ?? []).filter((k) => k.credential_kind === 1)
  const refreshEnabled = channel.settings?.account_control?.refresh_mode !== 'external'
  if (keys.length === 0) return null
  return (
    <div className="flex flex-col gap-3">
      {keys.map((k) => (
        <div key={k.id} className="flex flex-col gap-1">
          {keys.length > 1 && <span className="text-xs font-medium">#{k.id}</span>}
          <OAuthKeyHealth channelId={channel.id} provider={channel.provider} row={k}
            refreshEnabled={refreshEnabled} onDone={onDone} />
        </div>
      ))}
    </div>
  )
}
