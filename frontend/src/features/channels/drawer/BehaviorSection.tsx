import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Label } from '@/components/ui/input'
import { OptionalSection } from '@/components/ui/optional-section'
import { ClientProfileEditor } from '@/features/channels/ClientProfileEditor'
import { CodexClientEditor } from '@/features/channels/CodexClientEditor'
import { defaultResponsesNative, speaksOpenAi, TOOL_LOSS_POLICIES } from '@/features/channels/types'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { TagInput } from '@/components/ui/tag-input'
import type { ChannelForm } from './use-channel-form'
import { ExtraHeadersEditor, InjectFieldsEditor } from './request-editors'

/// 「请求与计费行为」（只在编辑时有）：客户端模拟、思考内容、按响应模型计费、请求改写。
export function BehaviorSection({ f, channel }: {
  f: ChannelForm
  channel: ChannelRow
}) {
  const { t } = useTranslation()
  const { form, settings, setSettings } = f
  const behaviorCustom = settings.thinking_to_content || settings.bill_by_response_model
    || (settings.tool_loss_policy ?? 'safe') !== 'safe'
    || settings.responses_native !== undefined && settings.responses_native !== defaultResponsesNative(form.provider)
    || settings.strip_request_fields.length > 0 || Object.keys(settings.inject_request_fields ?? {}).length > 0
    || Object.keys(settings.extra_headers ?? {}).length > 0
  return (
    <>
      {(channel.provider === 'anthropic' || channel.provider === 'anthropic_max') && (
        <ClientProfileEditor settings={settings} onChange={setSettings} />
      )}
      {channel.provider === 'codex' && (
        <CodexClientEditor settings={settings} onChange={setSettings} />
      )}
      <OptionalSection id="channel-behavior-options" title={t('admin:groupBehavior')} hint={t('admin:groupBehaviorHint')} defaultOpen
        summary={t(behaviorCustom ? 'admin:channelOptionsCustomized' : 'admin:channelOptionsDefault')}>
        <Switch
          className="rounded-lg border border-border/60 bg-card/65 px-3 py-3"
          label={t('admin:thinkingToContent')}
          description={t('admin:thinkingToContentHint')}
          checked={settings.thinking_to_content}
          onChange={(v) => setSettings((s) => ({ ...s, thinking_to_content: v }))}
        />
        <Switch
          className="rounded-lg border border-border/60 bg-card/65 px-3 py-3"
          label={t('admin:billByResponseModel')}
          description={t('admin:billByResponseModelHint')}
          checked={settings.bill_by_response_model}
          onChange={(v) => setSettings((s) => ({ ...s, bill_by_response_model: v }))}
        />
        {speaksOpenAi(channel.provider) && (
          <Switch
            className="rounded-lg border border-border/60 bg-card/65 px-3 py-3"
            label={t('admin:responsesNative')}
            description={t('admin:responsesNativeHint')}
            checked={settings.responses_native ?? defaultResponsesNative(channel.provider)}
            onChange={(v) => setSettings((s) => ({ ...s, responses_native: v }))}
          />
        )}
        <div className="flex flex-col gap-4 border-t border-border/60 pt-4">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="d-tool-loss" className="text-[13px] text-foreground">{t('admin:toolLossPolicy')}</Label>
          <p className="text-xs leading-5 text-muted-foreground">{t('admin:toolLossPolicyHint')}</p>
          <Select
            id="d-tool-loss"
            className="w-72"
            value={settings.tool_loss_policy ?? 'safe'}
            onChange={(v) => setSettings((s) => ({ ...s, tool_loss_policy: TOOL_LOSS_POLICIES.find((p) => p === v) ?? 'safe' }))}
            options={TOOL_LOSS_POLICIES.map((p) => ({ value: p, label: t(`admin:toolLoss_${p}`) }))}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="d-strip" className="text-[13px] text-foreground">{t('admin:stripFields')}</Label>
          <p className="text-xs leading-5 text-muted-foreground">{t('admin:stripFieldsHint')}</p>
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
        </div>
      </OptionalSection>
    </>
  )
}
