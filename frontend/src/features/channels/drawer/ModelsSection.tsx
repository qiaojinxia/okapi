import { ModelTagsInput } from '@/features/models/model-input'
import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { Label } from '@/components/ui/input'
import { OptionalSection } from '@/components/ui/optional-section'
import { ModelPicker } from '@/features/channels/ModelPicker'
import { isCloudManaged } from '@/features/channels/types'
import { PoolMembership, PoolMembershipEditor } from '@/features/channels/PoolMembership'
import type { ChannelForm } from './use-channel-form'

/// 「模型与渠道池」：服务的模型（手填 / 从上游拉取）与池成员关系。
export function ModelsSection({ f, channel, discover, onDone }: {
  f: ChannelForm
  channel: ChannelRow | undefined
  discover: { isPending: boolean; mutate: () => void }
  onDone: () => void
}) {
  const { t } = useTranslation()
  const isEdit = channel !== undefined
  const { canManagePools, form, models, setModels, credential, newPools, setNewPools, oauth, canDiscover, unknownModels } = f
  return (
    <>
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
        {(isEdit || !oauth) && (
          <div className="flex flex-col gap-1.5">
            <Button size="sm" variant="outline" className="self-start" disabled={!canDiscover || discover.isPending}
              onClick={() => discover.mutate()}>
              {t('admin:fetchModels')}
            </Button>
            {!isEdit && credential.trim() === '' && <p className="text-xs text-muted-foreground">{t('admin:fetchModelsNeedsCredential')}</p>}
          </div>
        )}
        {!isEdit && oauth && <p className="text-xs text-muted-foreground">{t('admin:fetchModelsAfterLogin')}</p>}
        {unknownModels.length > 0 && (
          <p role="status" data-testid="discovered-unpriced" className="rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
            {t('admin:discoveredUnknown', { n: unknownModels.length, first: unknownModels.slice(0, 3).join(', ') })}{' '}
            <a href="/admin/pricing" target="_blank" rel="noopener noreferrer" className="text-primary hover:underline">{t('admin:discoveredUnknownGo')}</a>
          </p>
        )}
      </FieldGroup>
      {isEdit && canManagePools && (
        <PoolMembership
          channelId={channel.id}
          current={channel.pool_members ?? []}
          onDone={onDone}
        />
      )}
      {!canManagePools && (
        <p role="note" className="text-xs text-muted-foreground">{t('admin:poolsAdminOnly')}</p>
      )}

      {!isEdit && canManagePools && (
        <OptionalSection id="channel-pools" title={t('admin:poolMembership')} hint={t('admin:poolMembershipHint')}
          summary={newPools.map((member) => member.pool_code + (member.priority_override !== null || member.weight_override !== null
            ? ` (${t('admin:channelOptionsCustomized')})` : '')).join(', ') || t('admin:channelPoolsEmpty')}
          error={newPools.length === 0 ? t('admin:poolOrphanWarning') : undefined}>
          <PoolMembershipEditor value={newPools} onChange={setNewPools} />
        </OptionalSection>
      )}
    </>
  )
}
