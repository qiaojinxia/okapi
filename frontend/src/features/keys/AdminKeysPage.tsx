import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { KeyRound, Power, PowerOff, ScrollText, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { toast } from '@/components/ui/toast'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { IconButton } from '@/components/ui/icon-button'
import { Input, Label } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { SearchInput } from '@/components/ui/search-input'
import { TableSkeleton } from '@/components/ui/skeleton'
import { Pagination } from '@/components/ui/pagination'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { UsageCell, useEntityUsage } from '@/features/analytics/UsageCell'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

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
  rpm_limit: number | null
  max_concurrency: number | null
  expires_at: string | null
  last_used_at: string | null
  created_at: string
}

const LIMIT = 20

/// 令牌管理面：跨用户排查与处置（停用/删除）。
/// 与门户自助页的区别是可跨用户检索——排查滥用时按用户名/令牌名定位；
/// 每行给"看它的日志"直达（滥用排查的下一步永远是看它调了什么）。
///
/// 行动作用图标（与渠道页同一形态）：此前是两个文字按钮竖排，"Disabled"既像状态
/// 又像动作，行高被撑成两倍、一屏只剩七八行。
export function AdminKeysPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [query, setQuery] = useState('')
  const [userId, setUserId] = useState('')
  const [offset, setOffset] = useState(0)
  const { confirm, dialog } = useConfirm()

  const uid = userId.trim() === '' ? null : Number(userId)
  const keys = useQuery({
    queryKey: [...qk.adminKeys(uid, query), offset],
    queryFn: () => {
      const params = new URLSearchParams({ limit: String(LIMIT), offset: String(offset) })
      if (query !== '') params.set('q', query)
      if (uid !== null && Number.isFinite(uid)) params.set('user_id', String(uid))
      return apiFetch<{ total: number; data: AdminKeyRow[] }>(`/admin/keys?${params}`)
    },
  })

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ['admin', 'keys'] })
  const applySearch = () => {
    setOffset(0)
    setQuery(search.trim())
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
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:keysNav')}
        description={t('admin:keysDesc')}
        icon={KeyRound}
        meta={
          keys.data && <Badge variant="muted">{t('admin:keyTotal', { n: keys.data.total })}</Badge>
        }
      />
      <Toolbar
        filters={
          <>
            <SearchInput
              id="kq"
              className="w-64"
              aria-label={t('admin:keySearch')}
              value={search}
              placeholder={t('admin:keySearchHint')}
              onChange={setSearch}
              onSubmit={applySearch}
            />
            <div className="flex items-center gap-2">
              <Label htmlFor="kuid">{t('admin:keyFilterUser')}</Label>
              <Input
                id="kuid"
                className="w-28"
                value={userId}
                inputMode="numeric"
                onChange={(e) => setUserId(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') applySearch()
                }}
              />
            </div>
            <Button size="sm" onClick={applySearch}>
              {t('common:search')}
            </Button>
          </>
        }
      />

      {dialog}
      {keys.isError ? (
        <ErrorState message={describeError(keys.error)} onRetry={() => void keys.refetch()} />
      ) : keys.isPending ? (
        <TableSkeleton rows={8} cols={9} />
      ) : rows.length === 0 ? (
        <EmptyState />
      ) : (
        <Table>
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
              <Th>{t('common:actions')}</Th>
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
                    {/* 限模型 / 覆盖分组是排查"为什么这把 key 打不到某模型"的直接线索 */}
                    {(k.model_allowlist?.length || k.group_override) && (
                      <span className="flex flex-wrap gap-1 pt-0.5">
                        {k.group_override && <Badge variant="muted">{k.group_override}</Badge>}
                        {k.model_allowlist && k.model_allowlist.length > 0 && (
                          <Badge variant="muted" title={k.model_allowlist.join(', ')}>
                            {t('admin:keyModelsLimited', { n: k.model_allowlist.length })}
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
                <Td>
                  <div className="flex items-center gap-0.5">
                    <Link
                      to="/admin/logs"
                      search={{ api_key_id: k.id, hours: 168 }}
                      className="inline-flex h-8 w-8 items-center justify-center rounded-md hover:bg-muted"
                      title={t('admin:keyViewLogs')}
                      aria-label={t('admin:keyViewLogs')}
                    >
                      <ScrollText className="h-4 w-4" />
                    </Link>
                    <IconButton
                      icon={k.status === 1 ? PowerOff : Power}
                      label={k.status === 1 ? t('admin:keyDisable') : t('admin:keyEnable')}
                      onClick={() => setStatus.mutate({ id: k.id, status: k.status === 1 ? 2 : 1 })}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: k.key_prefix }),
                          description: t('common:confirmKeyDelete'),
                          onConfirm: () => remove.mutate(k.id),
                        })
                      }
                    />
                  </div>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}
      <Pagination
        total={keys.data?.total ?? 0}
        limit={LIMIT}
        offset={offset}
        onOffset={setOffset}
      />
    </div>
  )
}
