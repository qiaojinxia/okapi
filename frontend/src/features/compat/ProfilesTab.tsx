import { Plus } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { useCompatVendors } from '@/features/channels/compat'
import { useProfiles } from '@/features/compat/api'
import type { SetTarget } from '@/features/compat/SetDrawer'
import { describeError } from '@/lib/i18n'

/// 适配档案：一类上游的处理方式只写一次，渠道的厂商字段选它就生效。
export function ProfilesTab({
  canCreate,
  onCreate,
  onOpen,
}: {
  canCreate: boolean
  onCreate: () => void
  onOpen: (target: SetTarget) => void
}) {
  const { t } = useTranslation()
  const profiles = useProfiles()
  const vendors = useCompatVendors(true)
  const names = new Map((vendors.data?.data ?? []).map((vendor) => [vendor.id, vendor.display_name]))

  if (profiles.isError) return <ErrorState message={describeError(profiles.error)} onRetry={() => void profiles.refetch()} />
  if (profiles.isPending) return <TableSkeleton rows={4} cols={6} />
  const rows = profiles.data
  if (rows.length === 0) {
    return (
      <EmptyState
        title={t('compat:profilesEmpty')}
        hint={t('compat:profilesEmptyHint')}
        action={
          canCreate ? (
            <Button onClick={onCreate}>
              <Plus className="h-4 w-4" />
              {t('compat:profileCreate')}
            </Button>
          ) : undefined
        }
      />
    )
  }
  return (
    <Table aria-label={t('compat:tabProfiles')}>
      <THead>
        <Tr>
          <Th>{t('compat:colProfile')}</Th>
          <Th>{t('compat:colExtends')}</Th>
          <Th>{t('compat:colHosts')}</Th>
          <Th>{t('compat:colLive')}</Th>
          <Th numeric>{t('compat:colChannels')}</Th>
          <Th>{t('compat:colActions')}</Th>
        </Tr>
      </THead>
      <TBody>
        {rows.map((profile) => (
          <Tr key={profile.id}>
            <Td className="max-w-72">
              <div className="flex min-w-0 flex-col">
                <span className="truncate font-medium">{profile.name}</span>
                <code className="truncate text-xs text-muted-foreground">{profile.vendor}</code>
                {profile.description !== null && (
                  <span className="truncate text-xs text-muted-foreground" title={profile.description}>
                    {profile.description}
                  </span>
                )}
              </div>
            </Td>
            <Td className="text-xs">{profile.extends === null ? '—' : (names.get(profile.extends) ?? profile.extends)}</Td>
            <Td className="max-w-56 truncate text-xs text-muted-foreground" title={profile.hosts.join(', ')}>
              {profile.hosts.length === 0 ? '—' : profile.hosts.join(', ')}
            </Td>
            <Td>
              <div className="flex flex-wrap items-center gap-1.5 text-xs">
                {profile.version === null ? (
                  <span className="text-muted-foreground">{t('compat:notPublished')}</span>
                ) : (
                  <span className="tabular-nums">{t('compat:liveShort', { version: profile.version, n: profile.rules })}</span>
                )}
                {profile.has_draft && <Badge variant="info">{t('compat:hasDraft')}</Badge>}
              </div>
            </Td>
            <Td numeric>{profile.channels}</Td>
            <Td>
              <Button size="xs" variant="outline" onClick={() => onOpen({ scope: 'profile', id: profile.id, name: profile.name })}>
                {t('compat:openProfile')}
              </Button>
            </Td>
          </Tr>
        ))}
      </TBody>
    </Table>
  )
}
