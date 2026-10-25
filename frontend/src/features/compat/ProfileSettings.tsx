import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { CopyButton } from '@/components/ui/copy-button'
import { Field } from '@/components/ui/field'
import { Input, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { TagInput } from '@/components/ui/tag-input'
import { toast } from '@/components/ui/toast'
import { useProfile, useProfileActions } from '@/features/compat/api'
import { useCompatVendors } from '@/features/channels/compat'
import type { ProfileDetail } from '@/features/compat/types'
import { usePermission } from '@/hooks/use-auth'
import { describeError } from '@/lib/i18n'

/// 档案的名称 / 说明 / 叠加的内置厂商 / 主机建议清单。slug 创建后不可改（渠道里存的是 `custom:<slug>`）。
/// 主机清单只用于「这个地址要套这个档案吗」的建议，不会静默改变任何渠道的行为。
export function ProfileSettings({ id, onDeleted }: { id: number; onDeleted: () => void }) {
  const detail = useProfile(id)
  if (detail.isError) return <ErrorState message={describeError(detail.error)} onRetry={() => void detail.refetch()} />
  if (detail.isPending) return <Skeleton className="h-64 w-full" />
  // 以服务端版本重新挂载：保存后表单按新数据重置，不会残留旧草稿
  return <SettingsForm key={detail.data.updated_at} profile={detail.data} onDeleted={onDeleted} />
}

function SettingsForm({ profile, onDeleted }: { profile: ProfileDetail; onDeleted: () => void }) {
  const { t } = useTranslation()
  const can = usePermission()
  const canEdit = can('compat.profile.write')
  const vendors = useCompatVendors(true)
  const actions = useProfileActions()
  const { confirm, dialog } = useConfirm()
  const [name, setName] = useState(profile.name)
  const [description, setDescription] = useState(profile.description ?? '')
  const [extendsVendor, setExtendsVendor] = useState(profile.extends ?? '')
  const [hosts, setHosts] = useState(profile.hosts)

  const changed =
    name.trim() !== profile.name ||
    description.trim() !== (profile.description ?? '') ||
    extendsVendor !== (profile.extends ?? '') ||
    JSON.stringify(hosts) !== JSON.stringify(profile.hosts)
  const nameBad = name.trim() === ''

  const save = () =>
    actions.update.mutate(
      {
        id: profile.id,
        body: {
          name: name.trim(),
          description: description.trim() === '' ? null : description.trim(),
          extends: extendsVendor === '' ? null : extendsVendor,
          hosts,
        },
      },
      {
        onSuccess: () => toast.success(t('compat:profileSaved')),
        onError: (err) => toast.error(describeError(err)),
      },
    )
  const remove = () =>
    confirm({
      title: t('compat:profileDeleteTitle', { name: profile.name }),
      description: t('compat:profileDeleteBody'),
      requireText: profile.slug,
      onConfirm: () =>
        actions.remove.mutate(profile.id, {
          onSuccess: () => {
            toast.success(t('compat:profileDeleted'))
            onDeleted()
          },
          onError: (err) => toast.error(describeError(err)),
        }),
    })

  return (
    <div className="flex flex-col gap-4">
      {dialog}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span>{t('compat:profileVendorId')}</span>
        <code className="text-foreground">{profile.vendor}</code>
        <CopyButton value={profile.vendor} label={t('compat:copyVendorId')} />
        <span>·</span>
        <span>{t('compat:profileChannels', { n: profile.channels })}</span>
      </div>
      <Field label={t('compat:fieldName')} htmlFor="profile-name" required>
        <Input id="profile-name" value={name} maxLength={128} readOnly={!canEdit} onChange={(e) => setName(e.target.value)} />
      </Field>
      <Field label={t('compat:fieldDescription')} htmlFor="profile-description">
        <Textarea
          id="profile-description"
          value={description}
          maxLength={512}
          rows={3}
          readOnly={!canEdit}
          onChange={(e) => setDescription(e.target.value)}
        />
      </Field>
      <Field label={t('compat:fieldExtends')} htmlFor="profile-extends" hint={t('compat:fieldExtendsHint')}>
        <Select
          id="profile-extends"
          className="w-72"
          value={extendsVendor}
          disabled={!canEdit}
          placeholder={t('compat:extendsNone')}
          options={(vendors.data?.data ?? []).map((vendor) => ({ value: vendor.id, label: vendor.display_name }))}
          onChange={setExtendsVendor}
        />
      </Field>
      <Field label={t('compat:fieldHosts')} htmlFor="profile-hosts" hint={t('compat:fieldHostsHint')}>
        <TagInput id="profile-hosts" value={hosts} onChange={canEdit ? setHosts : () => undefined} placeholder="relay.example.com" />
      </Field>
      {canEdit && (
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-4">
          <div className="flex flex-col gap-1">
            <Button
              variant="destructive"
              size="sm"
              className="self-start"
              disabled={profile.channels > 0 || actions.remove.isPending}
              onClick={remove}
            >
              {t('compat:profileDelete')}
            </Button>
            {profile.channels > 0 && <p className="text-xs text-muted-foreground">{t('compat:profileInUse', { n: profile.channels })}</p>}
          </div>
          <Button loading={actions.update.isPending} disabled={!changed || nameBad} onClick={save}>
            {t('compat:save')}
          </Button>
        </div>
      )}
    </div>
  )
}
