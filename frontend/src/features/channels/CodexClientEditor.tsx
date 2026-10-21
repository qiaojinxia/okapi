import { useTranslation } from 'react-i18next'
import { Input, Label } from '@/components/ui/input'
import { OptionalSection } from '@/components/ui/optional-section'
import { Switch } from '@/components/ui/switch'
import type { ChannelSettings, CodexClientGate } from './types'

const VERSION = /^\d+\.\d+\.\d+$/

/// Codex 渠道「仅限官方客户端」（`extensions.codex_client`）：开关 + 可选版本窗口。
/// 三项都空时整个键去掉，保存回去就是「不限制」。
export function CodexClientEditor({ settings, onChange }: {
  settings: ChannelSettings
  onChange: (settings: ChannelSettings) => void
}) {
  const { t } = useTranslation()
  const gate = settings.extensions?.codex_client ?? {}
  const update = (next: CodexClientGate) => {
    const { codex_client: _drop, ...extensions } = settings.extensions ?? {}
    const cleaned: CodexClientGate = {
      ...(next.official_only ? { official_only: true } : {}),
      ...(next.min_version ? { min_version: next.min_version } : {}),
      ...(next.max_version ? { max_version: next.max_version } : {}),
    }
    onChange({ ...settings, extensions: Object.keys(cleaned).length ? { ...extensions, codex_client: cleaned } : extensions })
  }
  const window = [gate.min_version && `≥ ${gate.min_version}`, gate.max_version && `≤ ${gate.max_version}`].filter(Boolean).join(' ')
  const summary = gate.official_only
    ? [t('admin:codexClientOfficialShort'), window].filter(Boolean).join(' · ')
    : t('admin:channelOptionsDefault')
  const invalid = (value?: string) => Boolean(value) && !VERSION.test(value!)
  return (
    <OptionalSection id="channel-codex-client" title={t('admin:codexClient')} summary={summary}>
      <div className="flex flex-col gap-1.5">
        <Switch checked={gate.official_only === true} label={t('admin:codexClientOfficial')}
          description={t('admin:codexClientOfficialHint')}
          onChange={(on) => update({ ...gate, official_only: on })} />
        {gate.official_only && <div className="mt-2 grid grid-cols-2 gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="d-codex-min">{t('admin:codexClientMinVersion')}</Label>
            <Input id="d-codex-min" value={gate.min_version ?? ''} placeholder="0.150.0" aria-invalid={invalid(gate.min_version)}
              onChange={(e) => update({ ...gate, min_version: e.target.value.trim() || undefined })} />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="d-codex-max">{t('admin:codexClientMaxVersion')}</Label>
            <Input id="d-codex-max" value={gate.max_version ?? ''} placeholder={t('admin:codexClientNoLimit')} aria-invalid={invalid(gate.max_version)}
              onChange={(e) => update({ ...gate, max_version: e.target.value.trim() || undefined })} />
          </div>
        </div>}
      </div>
    </OptionalSection>
  )
}
