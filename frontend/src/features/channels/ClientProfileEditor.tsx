import { useTranslation } from 'react-i18next'
import { Select } from '@/components/ui/select'
import { Input, Label } from '@/components/ui/input'
import { OptionalSection } from '@/components/ui/optional-section'
import type { ChannelSettings } from './types'

export function ClientProfileEditor({ settings, onChange }: {
  settings: ChannelSettings
  onChange: (settings: ChannelSettings) => void
}) {
  const { t } = useTranslation()
  const profile = settings.extensions?.client_profile
  const value = profile?.name === 'native' ? 'native' : profile?.mode ?? 'legacy'
  const labels = { legacy: 'admin:clientProfileLegacy', native: 'admin:clientProfileNative',
    auto: 'admin:clientProfileAuto', passthrough: 'admin:clientProfilePassthrough', mimic: 'admin:clientProfileMimic' } as const
  // 版本只由服务端决定：新配置不写版本，保存后服务端填入它实现的最新客户端。
  const revision = profile?.name === 'claude-code' ? profile.revision ?? t('admin:clientProfileLatest') : ''
  const summary = [t(labels[value]), value !== 'passthrough' ? revision : ''].filter(Boolean).join(' · ')
  return (
    <OptionalSection id="channel-client-options" title={t('admin:clientProfile')} summary={summary}>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="d-client-profile">{t('admin:clientProfile')}</Label>
        <Select id="d-client-profile" value={value} onChange={(mode) => {
          const { client_profile: _drop, ...extensions } = settings.extensions ?? {}
          onChange({ ...settings, extensions: mode === 'legacy' ? extensions : {
            ...extensions,
            client_profile: mode === 'native' ? { name: 'native' } : {
              ...(profile?.name === 'claude-code' ? profile : {}),
              name: 'claude-code', mode: mode as 'auto' | 'passthrough' | 'mimic',
            },
          } })
        }} options={[
          { value: 'legacy', label: t('admin:clientProfileLegacy') },
          { value: 'native', label: t('admin:clientProfileNative') },
          { value: 'auto', label: t('admin:clientProfileAuto') },
          { value: 'passthrough', label: t('admin:clientProfilePassthrough') },
          { value: 'mimic', label: t('admin:clientProfileMimic') },
        ]} />
        {profile?.name === 'claude-code' && value !== 'passthrough' && <>
          <Label htmlFor="d-client-profile-version">{t('admin:clientProfileVersion')}</Label>
          <Input id="d-client-profile-version" value={revision} readOnly />
          <Label htmlFor="d-client-profile-entrypoint">{t('admin:clientProfileEntrypoint')}</Label>
          <Select id="d-client-profile-entrypoint" value={profile.entrypoint ?? 'cli'} onChange={(entrypoint) => {
            onChange({ ...settings, extensions: { ...settings.extensions, client_profile: {
              ...profile, entrypoint: entrypoint as 'cli' | 'sdk-cli',
            } } })
          }} options={[
            { value: 'cli', label: t('admin:clientProfileCli') },
            { value: 'sdk-cli', label: t('admin:clientProfileSdk') },
          ]} />
        </>}
        <p className="text-xs text-muted-foreground">{t('admin:clientProfileHint')}</p>
      </div>
    </OptionalSection>
  )
}
