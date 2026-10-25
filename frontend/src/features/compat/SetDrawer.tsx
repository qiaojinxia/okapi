import { useTranslation } from 'react-i18next'
import { Drawer } from '@/components/ui/drawer'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { useProfile } from '@/features/compat/api'
import { ProfileSettings } from '@/features/compat/ProfileSettings'
import { SetWorkspace } from '@/features/compat/SetWorkspace'
import type { SetScope } from '@/features/compat/types'
import { describeError } from '@/lib/i18n'

/// 页面里点开的一个规则集：渠道层（从收件箱进来审阅）或适配档案。全局护栏直接铺在页面里，不走抽屉。
export interface SetTarget {
  scope: Exclude<SetScope, 'guard'>
  id: number
  /// 标题里显示的名字（渠道名 / 档案名）；档案会再从详情里取最新的。
  name: string | null
}

function ProfileBody({ id, onClose }: { id: number; onClose: () => void }) {
  const profile = useProfile(id)
  if (profile.isError) return <ErrorState message={describeError(profile.error)} onRetry={() => void profile.refetch()} />
  if (profile.isPending) return <Skeleton className="h-64 w-full" />
  return (
    <SetWorkspace
      scope="profile"
      id={id}
      idPrefix={`profile-${id}`}
      extendsVendor={profile.data.extends}
      settings={<ProfileSettings id={id} onDeleted={onClose} />}
    />
  )
}

export function SetDrawer({ target, onClose }: { target: SetTarget | null; onClose: () => void }) {
  const { t } = useTranslation()
  if (target === null) return null
  const title =
    target.scope === 'profile'
      ? t('compat:drawerProfile', { name: target.name ?? `#${target.id}` })
      : t('compat:drawerChannel', { name: target.name ?? `#${target.id}` })
  return (
    <Drawer
      open
      onClose={onClose}
      size="xl"
      title={title}
      description={target.scope === 'profile' ? t('compat:drawerProfileHint') : t('compat:drawerChannelHint')}
    >
      {target.scope === 'profile' ? (
        <ProfileBody key={target.id} id={target.id} onClose={onClose} />
      ) : (
        <SetWorkspace key={target.id} scope="channel" id={target.id} idPrefix={`channel-${target.id}`} />
      )}
    </Drawer>
  )
}
