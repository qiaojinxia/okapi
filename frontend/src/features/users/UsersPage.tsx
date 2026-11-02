import { Settings2 } from 'lucide-react'
import { getRouteApi } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader, Toolbar, ToolbarSearch } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { SearchInput } from '@/components/ui/search-input'
import { ListState } from '@/components/ui/list-state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { UsageCell, useEntityUsage } from '@/components/usage-cell'
import { UserDrawer } from '@/features/users/UserDrawer'
import { useDraft } from '@/hooks/use-draft'
import { pagedPath, usePagedQuery, withFilters } from '@/hooks/use-paged-query'
import type { Paged } from '@/hooks/use-paged-query'
import { apiFetch } from '@/lib/api'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { text } from '@/lib/search-params'
import { roleLabel } from '@/lib/roles'

const routeApi = getRouteApi('/admin/users')

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
  // 搜索词与页码都在地址里：刷新 / 分享 / 从用户抽屉深链出去再后退，都回到原来那一页
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const query = search.q ?? ''
  const [draft, setDraft] = useDraft(query)
  const [selected, setSelected] = useState<number | null>(null)
  // 搜索词与页码同一次导航更新：只按"新词 + 第一页"请求一次，不会先按旧页码空跑一趟
  const applySearch = (value: string) =>
    void navigate({ search: withFilters({ q: text(value) }) })

  const { query: users, pager, rows } = usePagedQuery({
    queryKey: (page) => qk.adminUsersPage(query, page),
    queryFn: (page) => apiFetch<Paged<UserRow>>(pagedPath('/admin/users', page, { q: query })),
  })

  // 行内用量（Sub2API 用户列表同有）：找人时"他花不花钱、最近活跃吗"与余额同样重要
  const usage = useEntityUsage(
    'user',
    rows.map((u) => u.id),
  )

  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:usersTitle')}
        description={t('admin:usersDesc')}
        meta={users.data && t('admin:usersTotal', { total: users.data.total })}
      />

      <Toolbar
        selectionClassName="break-words sm:max-w-sm"
        filters={
          <>
            <ToolbarSearch>
              <SearchInput
                id="u-search"
                className="min-w-0 flex-1"
                aria-label={t('admin:usersSearch')}
                value={draft}
                placeholder={t('admin:usersSearchHint')}
                onChange={setDraft}
                onSubmit={() => applySearch(draft)}
              />
              <Button size="sm" variant="outline" onClick={() => applySearch(draft)}>
                {t('common:search')}
              </Button>
            </ToolbarSearch>
            {query !== '' && (
              <Button size="sm" variant="ghost" onClick={() => applySearch('')}>
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

      <ListState
        query={users}
        skeleton={{ rows: 10, cols: 8 }}
        empty={
          <EmptyState
            title={query !== '' ? t('common:noResults') : undefined}
            hint={query !== '' ? t('common:noResultsHint') : undefined}
          />
        }
      >
        <Table stickyHeader>
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('common:username')}</Th>
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
      </ListState>

      <Pagination {...pager} total={users.data?.total} />

      {selected !== null && (
        <UserDrawer userId={selected} onClose={() => setSelected(null)} />
      )}
    </div>
  )
}
