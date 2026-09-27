import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, getRouteApi } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { KeyRound, Power, PowerOff, ScrollText, Trash2 } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { useState } from 'react'
import { Badge } from '@/components/ui/badge'
import { toast } from '@/components/ui/toast'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { IconButton } from '@/components/ui/icon-button'
import { Label } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { SearchInput } from '@/components/ui/search-input'
import { TableSkeleton } from '@/components/ui/skeleton'
import { Pagination } from '@/components/ui/pagination'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { UsageCell, useEntityUsage } from '@/features/analytics/UsageCell'
import { UserSearchInput, validUserFilter } from '@/features/users/UserSearchInput'
import { useDraft } from '@/hooks/use-draft'
import { usePermission } from '@/hooks/use-auth'
import { usePagination } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { posInt, text } from '@/lib/search-params'

const routeApi = getRouteApi('/admin/keys')

interface AdminKeyRow {
  id: number
  user_id: number
  username: string
  team_id: number | null
  name: string
  key_prefix: string
  status: number
  quota_mode: number
  quota_micro: number | null
  used_micro: number
  model_allowlist: string[] | null
  group_override: string | null
  ip_allowlist: string[] | null
  rpm_limit: number | null
  max_concurrency: number | null
  expires_at: string | null
  last_used_at: string | null
  created_at: string
}

/// 令牌管理面：跨用户排查与处置（停用/删除）。
/// 与门户自助页的区别是可跨用户检索——排查滥用时按用户名/令牌名定位；
/// 每行给"看它的日志"直达（滥用排查的下一步永远是看它调了什么）。
///
/// 行动作用图标（与渠道页同一形态）：此前是两个文字按钮竖排，"Disabled"既像状态
/// 又像动作，行高被撑成两倍、一屏只剩七八行。
export function AdminKeysPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const can = usePermission()
  const canManage = can('user.manage')
  const canReadLogs = can('billing.read')
  const showActions = canManage || canReadLogs
  const queryClient = useQueryClient()
  // 检索条件（关键词 / 用户 id）与页码都在地址里，用户页可以带 user_id 直达
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const query = search.q ?? ''
  const uid = search.user_id ?? null
  const [draft, setDraft] = useDraft(query)
  const [userIdDraft, setUserIdDraft] = useDraft(uid === null ? '' : String(uid))
  const [filterError, setFilterError] = useState(false)
  const invalidUser = filterError && !validUserFilter(userIdDraft)
  const { confirm, dialog } = useConfirm()

  const pager = usePagination()
  const keys = useQuery({
    queryKey: [...qk.adminKeys(uid, query), pager.offset, pager.limit],
    queryFn: () => {
      const params = new URLSearchParams({
        limit: String(pager.limit),
        offset: String(pager.offset),
      })
      if (query !== '') params.set('q', query)
      if (uid !== null) params.set('user_id', String(uid))
      return apiFetch<{ total: number; data: AdminKeyRow[] }>(`/admin/keys?${params}`)
    },
    // 翻页时保留上一页数据：表格不闪成骨架屏
    placeholderData: keepPreviousData,
  })

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminKeysAll })
  // 两个条件一起提交，并在同一次导航里回第一页
  const applySearch = () => {
    if (!validUserFilter(userIdDraft)) { setFilterError(true); return }
    setFilterError(false)
    void navigate({
      search: (prev) => ({ ...prev, q: text(draft), user_id: posInt(userIdDraft), page: undefined }),
    })
  }
  const clearFilters = () => {
    setDraft('')
    setUserIdDraft('')
    setFilterError(false)
    void navigate({ search: (prev) => ({ ...prev, q: undefined, user_id: undefined, page: undefined }) })
  }

  const setStatus = useMutation({
    mutationFn: (arg: { id: number; status: number }) =>
      apiFetch(`/admin/keys/${arg.id}`, { method: 'PATCH', body: { status: arg.status } }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (id: number) => apiFetch(`/admin/keys/${id}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const rows = keys.data?.data ?? []
  // 行内近期用量：`used_micro` 是这把 key 一生的累计，答不了"最近还在用吗"
  const usage = useEntityUsage(
    'api_key',
    rows.map((k) => k.id),
  )
  const expiry = (k: AdminKeyRow) => {
    if (k.expires_at === null) return <span className="text-muted-foreground">—</span>
    const at = dayjs(k.expires_at)
    // 已过期红、七天内到期黄：到期的 key 会静默失败，站长要先于用户看到
    const tone = at.isBefore(dayjs()) ? 'destructive' : at.diff(dayjs(), 'day') <= 7 ? 'warning' : 'muted'
    return <Badge variant={tone}>{at.format('YYYY-MM-DD')}</Badge>
  }

  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:keysNav')}
        description={t('admin:keysDesc')}
        icon={KeyRound}
        meta={
          keys.data && <Badge variant="muted">{t('admin:keyTotal', { n: keys.data.total })}</Badge>
        }
      />
      <Toolbar
        selection={keys.isFetching ? <span role="status" className="text-xs text-muted-foreground">{t('common:loading')}</span> : undefined}
        filters={
          <form className="grid w-full min-w-0 items-start gap-3 sm:grid-cols-2 lg:grid-cols-[minmax(0,20rem)_minmax(0,20rem)_auto]" onSubmit={(event) => { event.preventDefault(); applySearch() }}>
            <div className="flex min-w-0 flex-col gap-1">
              <Label htmlFor="kq" className="leading-4">{t('admin:keySearch')}</Label>
              <SearchInput
                id="kq"
                className="w-full"
                aria-label={t('admin:keySearch')}
                value={draft}
                placeholder={t('admin:keySearchHint')}
                onChange={setDraft}
                onSubmit={applySearch}
              />
            </div>
            <div className="flex min-w-0 flex-col gap-1">
              <Label htmlFor="kuid" className="leading-4">{t('admin:userFilterLabel')}</Label>
              <UserSearchInput
                id="kuid"
                value={userIdDraft}
                knownUsers={rows.map((key) => ({ id: key.user_id, username: key.username }))}
                onChange={(value) => { setUserIdDraft(value); setFilterError(false) }}
                onSubmit={applySearch}
                aria-invalid={invalidUser || undefined}
                aria-describedby={invalidUser ? 'kuid-error' : undefined}
              />
              {invalidUser && <p id="kuid-error" role="alert" className="text-xs text-destructive">{t('admin:userFilterInvalid')}</p>}
            </div>
            <div className="flex flex-wrap gap-2 lg:pt-5">
              <Button type="submit" size="sm" variant="outline">{t('common:search')}</Button>
              {(query !== '' || uid !== null || draft !== '' || userIdDraft !== '') && <Button size="sm" variant="ghost" onClick={clearFilters}>{t('common:clearFilters')}</Button>}
            </div>
          </form>
        }
      />

      {dialog}
      {keys.isError ? (
        <ErrorState message={describeError(keys.error)} onRetry={() => void keys.refetch()} />
      ) : keys.isPending ? (
        <TableSkeleton rows={8} cols={9} />
      ) : rows.length === 0 ? (
        <EmptyState title={query !== '' || uid !== null ? t('common:noResults') : undefined}
          hint={query !== '' || uid !== null ? t('common:noResultsHint') : undefined}
          action={query !== '' || uid !== null ? <Button variant="outline" onClick={clearFilters}>{t('common:clearFilters')}</Button> : undefined} />
      ) : (
        <Table stickyHeader aria-label={t('admin:keysNav')} aria-busy={keys.isFetching} scrollResetKey={`${query}:${uid}:${pager.offset}:${pager.limit}`}>
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('admin:username')}</Th>
              <Th>{t('portal:keyName')}</Th>
              <Th>{t('portal:keyPrefix')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('portal:keyUsed')}</Th>
              {usage.enabled && <Th>{t('admin:usageColumn')}</Th>}
              <Th>{t('portal:keyRpm')}</Th>
              <Th>{t('admin:keyExpires')}</Th>
              <Th>{t('admin:keyLastUsed')}</Th>
              {showActions && <Th>{t('common:actions')}</Th>}
            </Tr>
          </THead>
          <TBody>
            {rows.map((k) => (
              <Tr key={k.id}>
                <Td>{k.id}</Td>
                <Td className="max-w-40 truncate" title={k.username}>
                  {k.username}
                </Td>
                <Td>
                  <div className="flex flex-col">
                    <span className="max-w-40 truncate" title={k.name}>
                      {k.name || '—'}
                    </span>
                    {/* 限模型 / 覆盖分组 / 限 IP 是排查"为什么这把 key 打不通"的直接线索 */}
                    {(k.model_allowlist?.length || k.group_override || k.ip_allowlist?.length) && (
                      <span className="flex flex-wrap gap-1 pt-0.5">
                        {k.group_override && <Badge variant="muted">{k.group_override}</Badge>}
                        {k.model_allowlist && k.model_allowlist.length > 0 && (
                          <Badge variant="muted" title={k.model_allowlist.join(', ')}>
                            {t('admin:keyModelsLimited', { n: k.model_allowlist.length })}
                          </Badge>
                        )}
                        {k.ip_allowlist && k.ip_allowlist.length > 0 && (
                          <Badge variant="muted" title={k.ip_allowlist.join(', ')}>
                            {t('admin:keyIpLimited', { n: k.ip_allowlist.length })}
                          </Badge>
                        )}
                      </span>
                    )}
                  </div>
                </Td>
                <Td className="whitespace-nowrap font-mono text-xs">{k.key_prefix}…</Td>
                <Td>
                  <Badge variant={k.status === 1 ? 'success' : 'muted'}>
                    {k.status === 1 ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td className="whitespace-nowrap">{formatMoney(k.used_micro, locale)}</Td>
                {usage.enabled && (
                  <Td className="whitespace-nowrap">
                    <UsageCell
                      usage={usage.data?.[String(k.id)]}
                      unavailable={usage.unavailable}
                      link={{ api_key_id: k.id }}
                    />
                  </Td>
                )}
                <Td>{k.rpm_limit ?? '—'}</Td>
                <Td>{expiry(k)}</Td>
                <Td className="whitespace-nowrap text-xs">
                  {k.last_used_at ? dayjs(k.last_used_at).format('MM-DD HH:mm') : '—'}
                </Td>
                {showActions && <Td>
                  <div className="flex items-center gap-0.5">
                    {canReadLogs && <Link
                      to="/admin/logs"
                      search={{ api_key_id: k.id, hours: 168 }}
                      className="inline-flex h-8 w-8 items-center justify-center rounded-md hover:bg-muted"
                      title={t('admin:keyViewLogs')}
                      aria-label={t('admin:keyViewLogs')}
                    >
                      <ScrollText className="h-4 w-4" />
                    </Link>}
                    {canManage && <>
                    <IconButton
                      icon={k.status === 1 ? PowerOff : Power}
                      disabled={setStatus.isPending || remove.isPending || keys.isFetching}
                      label={k.status === 1 ? t('admin:keyDisable') : t('admin:keyEnable')}
                      onClick={() => setStatus.mutate({ id: k.id, status: k.status === 1 ? 2 : 1 })}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      disabled={setStatus.isPending || remove.isPending || keys.isFetching}
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: k.key_prefix }),
                          description: t('common:confirmKeyDelete'),
                          onConfirm: () => remove.mutate(k.id),
                        })
                      }
                    />
                    </>}
                  </div>
                </Td>}
              </Tr>
            ))}
          </TBody>
        </Table>
      )}
      <Pagination {...pager} total={keys.data?.total} />
    </div>
  )
}
