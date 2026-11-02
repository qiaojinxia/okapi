import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Field } from '@/components/ui/field'
import { OptionalSection } from '@/components/ui/optional-section'
import { OAuthLoginCard } from '@/features/channels/OAuthLoginCard'
import { ClientProfileEditor } from '@/features/channels/ClientProfileEditor'
import { ChannelEgress, NewChannelEgress } from '@/features/channels/ChannelEgress'
import { PROVIDERS, apiBasePlaceholder, channelSettingsForSave, needsVersionSegment } from '@/features/channels/types'
import { Select } from '@/components/ui/select'
import { Tabs } from '@/components/ui/tabs'
import type { ChannelForm } from './use-channel-form'
import { CredentialStatus } from './key-panels'

const PROVIDER_LABELS: Record<string, string> = {
  anthropic_max: 'admin:providerClaudeSubscription',
  codex: 'admin:providerCodexSubscription',
}

/// 「接入信息」：名称、协议、地址、凭证 / 订阅登录、出口。
export function ConnectionSection({ f, channel, rotate, onDone, onClose }: {
  f: ChannelForm
  channel: ChannelRow | undefined
  rotate: { isPending: boolean; mutate: () => void }
  onDone: () => void
  onClose: () => void
}) {
  const { t } = useTranslation()
  const isEdit = channel !== undefined
  const { canManagePools, form, setForm, account, authorization, accountReady, costMilli, models, credential, setCredential, oauthInput, setOauthInput, rotationKeyId, setRotationKeyId, settings, setSettings, controlValid, concurrency, concurrencyValid, priority, newPools, newEgress, setNewEgress, egressBinding, oauth, importToken, defaultBase, endpointRequired, endpointValid } = f
  const baseField = <div className="flex flex-col gap-1.5">
    <Label htmlFor="d-base">{t('admin:apiBase')}</Label>
    <Input id="d-base" value={form.api_base} placeholder={defaultBase ?? apiBasePlaceholder(form.provider)} required={endpointRequired}
      onChange={(e) => setForm((f) => ({ ...f, api_base: e.target.value }))} />
    {needsVersionSegment(form.provider, form.api_base) && (
      <p role="note" className="text-xs text-warning">{t('admin:apiBaseVersionHint', { example: apiBasePlaceholder(form.provider) })}</p>
    )}
    {/^http:\/\//i.test(form.api_base.trim()) && <p role="note" className="text-xs text-warning">{t('admin:apiBaseHttpWarning')}</p>}
    {form.provider === 'azure' && <p className="text-xs text-muted-foreground">{t('admin:azureApiBaseHint')}</p>}
    {form.provider === 'bedrock' && <p className="text-xs text-muted-foreground">{t('admin:bedrockApiBaseHint')}</p>}
    {form.provider === 'vertex' && <p className="text-xs text-muted-foreground">{t('admin:vertexApiBaseHint')}</p>}
  </div>
  return (
    <>
      <FieldGroup title={t('admin:groupBasic')} hint={t('admin:groupBasicHint')}>
        <div className="grid grid-cols-2 gap-4">
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
        <div data-slot="channel-auth-method" className="flex">
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
              : endpointValid && costMilli !== null && priority !== null && egressBinding !== null)}
            creationOptions={!isEdit ? { api_base: form.api_base, priority: priority ?? 0,
              pools: canManagePools ? newPools : undefined, cost_milli: costMilli ?? undefined,
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
  )
}
