import { Settings2, Users } from 'lucide-react'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { SearchInput } from '@/components/ui/search-input'
import { TableSkeleton } from '@/components/ui/skeleton'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { UsageCell, useEntityUsage } from '@/features/analytics/UsageCell'
import { UserDrawer } from '@/features/users/UserDrawer'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { roleLabel } from '@/features/users/types'

const LIMIT = 20



interface UserRow {
  id: number
  username: string
  email: string | null
  role: number
  status: number
  balance_micro: number
  admin_role_id: number | null
  price_multiplier: string
}



/// 用户列表页。
///
/// 只负责找人：搜索、翻页、进入某个用户的管理抽屉。角色定义在 /admin/roles，
/// 此前两者同页会让人以为编辑角色只影响当前选中的用户。
export function UsersPage() {
  const { t, i18n } = useTranslation()
  const [search, setSearch] = useState('')
  const [query, setQuery] = useState('')
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<number | null>(null)

  const users = useQuery({
    queryKey: [...qk.adminUsers(query), offset],
    queryFn: () => {
      const params = new URLSearchParams({ limit: String(LIMIT), offset: String(offset) })
      if (query !== '') params.set('q', query)
      return apiFetch<{ total: number; data: UserRow[] }>(`/admin/users?${params}`)
    },
  })

  const rows = users.data?.data ?? []
  // 行内用量（Sub2API 用户列表同有）：找人时"他花不花钱、最近活跃吗"与余额同样重要
  const usage = useEntityUsage(
    'user',
    rows.map((u) => u.id),
  )

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:usersTitle')}
        description={t('admin:usersDesc')}
        icon={Users}
        meta={
          users.data && (
            <Badge variant="muted">{t('admin:usersTotal', { total: users.data.total })}</Badge>
          )
        }
      />

      <Toolbar
        filters={
          <>
            <SearchInput
              id="u-search"
              className="w-72"
              aria-label={t('admin:usersSearch')}
              value={search}
              placeholder={t('admin:usersSearchHint')}
              onChange={setSearch}
              onSubmit={() => {
                setOffset(0)
                setQuery(search.trim())
              }}
            />
            <Button
              size="sm"
              onClick={() => {
                setOffset(0)
                setQuery(search.trim())
              }}
            >
              {t('common:search')}
            </Button>
            {query !== '' && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  setSearch('')
                  setQuery('')
                  setOffset(0)
                }}
              >
                {t('common:clearFilters')}
              </Button>
            )}
          </>
        }
        selection={
          <span className="text-xs text-muted-foreground">
            {query !== '' ? t('admin:usersSearchingFor', { q: query }) : t('admin:usersSearchEnterHint')}
          </span>
        }
      />

      {users.isError ? (
        <ErrorState message={describeError(users.error)} onRetry={() => void users.refetch()} />
      ) : users.isPending ? (
        <TableSkeleton rows={10} cols={8} />
      ) : rows.length === 0 ? (
        <EmptyState
          title={query !== '' ? t('common:noResults') : undefined}
          hint={query !== '' ? t('common:noResultsHint') : undefined}
        />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('admin:username')}</Th>
              <Th>{t('admin:usersEmail')}</Th>
              <Th>{t('admin:usersRole')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('common:balance')}</Th>
              {usage.enabled && <Th>{t('admin:usageColumn')}</Th>}
              <Th>{t('admin:usersMultiplier')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((u) => (
              <Tr key={u.id}>
                <Td>{u.id}</Td>
                <Td className="max-w-48 truncate">{u.username}</Td>
                <Td className="max-w-48 truncate text-xs text-muted-foreground">
                  {u.email ?? '—'}
                </Td>
                <Td>
                  <Badge variant={u.role >= 10 ? 'success' : 'muted'}>
                    {roleLabel(u.role, t)}
                    {u.admin_role_id !== null ? ` #${u.admin_role_id}` : ''}
                  </Badge>
                </Td>
                <Td>
                  <Badge variant={u.status === 1 ? 'success' : 'destructive'}>
                    {u.status === 1 ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td>{formatMoney(u.balance_micro, i18n.language)}</Td>
                {usage.enabled && (
                  <Td>
                    <UsageCell
                      usage={usage.data?.[String(u.id)]}
                      unavailable={usage.unavailable}
                      link={{ user_id: u.id }}
                    />
                  </Td>
                )}
                <Td className="font-mono text-xs">×{u.price_multiplier}</Td>
                <Td>
                  <IconButton
                    icon={Settings2}
                    label={t('admin:usersManage')}
                    variant="outline"
                    onClick={() => setSelected(u.id)}
                  />
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      <Pagination
        total={users.data?.total ?? 0}
        limit={LIMIT}
        offset={offset}
        onOffset={setOffset}
      />

      {selected !== null && (
        <UserDrawer userId={selected} onClose={() => setSelected(null)} />
      )}
    </div>
  )
}
