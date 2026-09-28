import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { BookOpen, KeyRound, Pencil, Plus, Power, PowerOff, RotateCw, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { CopyButton } from '@/components/ui/copy-button'
import { Drawer } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { IconButton } from '@/components/ui/icon-button'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { useGuide } from '@/features/portal-guide/guide-state'
import { usePagination } from '@/hooks/use-pagination'
import { useMe } from '@/hooks/use-auth'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { Select } from '@/components/ui/select'
import { TagInput } from '@/components/ui/tag-input'
import { formatCount, formatMoney, formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { KeyUsageDrawer } from './KeyUsageDrawer'
import { KeyUsageSparkline } from './KeyUsageSparkline'
import type { KeyTokenTrend } from './KeyUsageSparkline'
import { KeyCopyButton } from './KeyCopyButton'
import type { KeyCopyStatus } from './KeyCopyButton'
import { emptyKeyLimits, KeyLimitsFields, keyLimitsError, keyQuotaMicro, keyQuotaText } from './KeyLimitsFields'
import type { KeyLimitsDraft } from './KeyLimitsFields'

interface KeyRow {
  id: number
  name: string
  key_prefix: string
  copy_status?: KeyCopyStatus
  status: number
  used_micro: number
  quota_mode?: number
  quota_micro?: number | null
  expires_at?: string | null
  model_allowlist?: string[] | null
  rpm_limit: number | null
  created_at: string
  amount_micro: number
  requests: number
  usage_trend?: KeyTokenTrend | null
  /// 这把 key 钉住的分组；null = 跟随用户分组。
  group_override: string | null
  /// 数据面来源 IP 白名单（地址 / CIDR）；null = 不限。
  ip_allowlist: string[] | null
}

interface SelectableGroup {
  code: string
  ratio: string
  description: string | null
  source: 'assigned' | 'self_select' | 'default'
}

type Editor =
  | { mode: 'create' }
  | { mode: 'rename'; id: number; name: string; group: string | null; ips: string[]; limits?: Pick<KeyRow, 'quota_micro' | 'expires_at' | 'model_allowlist'> }

/// 门户密钥页：列表 + 自助新建（new-api 令牌页的"添加令牌"）。
///
/// 新建走 `/auth/keys`（会话鉴权，与 Team/TOTP 同轨）：邮箱密码登录的用户在这里
/// 直接建；API Key 登录的浏览器没有 session 会 401——降级为"请改用邮箱密码登录"
/// 而不是哑按钮。已加密保存的密钥可经账号会话按需复制；历史哈希不能还原。
///
/// 新建与改名都走抽屉：此前改名表单出现在表格上方、与被改的那一行相隔半屏，
/// 看不出改的是哪一把。
export function PortalKeysPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const me = useMe()
  const [editor, setEditor] = useState<Editor | null>(null)
  const [usageKeyId, setUsageKeyId] = useState<number | null>(null)
  const [draft, setDraft] = useState('')
  const [limits, setLimits] = useState<KeyLimitsDraft>(emptyKeyLimits)
  const limitsError = keyLimitsError(limits)
  // 分组：'' = 跟随用户分组
  const [group, setGroup] = useState('')
  // IP 白名单草稿：空 = 不限
  const [ips, setIps] = useState<string[]>([])
  const [minted, setMinted] = useState<{ name: string; api_key: string; copy_available?: boolean } | null>(null)
  const [sessionMsg, setSessionMsg] = useState<string | null>(null)
  const { confirm, dialog } = useConfirm()
  const guide = useGuide()
  const pager = usePagination()

  const keys = useQuery({
    queryKey: [...qk.keys, pager.offset, pager.limit],
    queryFn: () =>
      apiFetch<{ data: KeyRow[]; total: number; key_limits_supported?: boolean }>(
        `/api/me/keys?limit=${pager.limit}&offset=${pager.offset}`,
      ),
    placeholderData: keepPreviousData,
    refetchInterval: 30_000,
    refetchIntervalInBackground: false,
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.keys })

  // 可选分组由系统配置与用户权限确定。
  const groups = useQuery({
    queryKey: qk.myGroups,
    queryFn: () => apiFetch<{ current: string; data: SelectableGroup[] }>('/api/me/groups'),
    staleTime: 60_000,
  })
  const selectable = groups.data?.data ?? []
  const showGroupPicker = selectable.length > 0 || group !== ''

  const openEditor = (e: Editor) => {
    setEditor(e)
    setDraft(e.mode === 'rename' ? e.name : '')
    setGroup(e.mode === 'rename' ? (e.group ?? '') : '')
    setIps(e.mode === 'rename' ? e.ips : [])
    setLimits(e.mode === 'rename' ? {
      expires: e.limits?.expires_at ? dayjs(e.limits.expires_at).format('YYYY-MM-DDTHH:mm:ss') : '',
      quota: keyQuotaText(e.limits?.quota_micro),
      models: e.limits?.model_allowlist ?? [],
    } : emptyKeyLimits())
  }

  const create = useMutation({
    mutationFn: (arg: { name: string; group: string; ips: string[]; restrictions: Record<string, unknown> }) =>
      apiFetch<{ key_id: number; api_key: string; copy_available?: boolean }>('/auth/keys', {
        method: 'POST',
        body: {
          ...arg.restrictions,
          name: arg.name,
          group_code: arg.group === '' ? undefined : arg.group,
          ip_allowlist: arg.ips.length === 0 ? undefined : arg.ips,
        },
      }),
    onSuccess: (r, arg) => {
      setMinted({ name: arg.name, api_key: r.api_key, copy_available: r.copy_available })
      setEditor(null)
      setSessionMsg(null)
      invalidate()
    },
    onError: (err) => {
      if (err instanceof ApiError && err.status === 401) {
        setEditor(null)
        setSessionMsg(t('portal:keysSessionRequired'))
        return
      }
      toast.error(describeError(err))
    },
  })

  const patch = useMutation({
    mutationFn: (arg: { id: number; body: Record<string, unknown> }) =>
      apiFetch(`/api/me/keys/${arg.id}`, { method: 'PATCH', body: arg.body }),
    onSuccess: () => {
      toast.success(t('common:saved'))
      setEditor(null)
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (id: number) => apiFetch(`/api/me/keys/${id}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const submitEditor = () => {
    const name = draft.trim()
    if (name === '' || editor === null || limitsError || create.isPending || patch.isPending) return
    const old = editor.mode === 'rename' ? editor.limits : undefined
    const restrictions = {
      expires_at: limits.expires ? new Date(limits.expires).toISOString() : old?.expires_at !== undefined ? null : undefined,
      quota_micro: keys.data?.key_limits_supported ? keyQuotaMicro(limits.quota) : undefined,
      model_allowlist: limits.models.length ? limits.models : old?.model_allowlist !== undefined ? null : undefined,
    }
    if (editor.mode === 'create') create.mutate({ name, group, ips, restrictions })
    else
      patch.mutate({
        id: editor.id,
        body: {
          ...restrictions,
          name,
          group_code: group === '' ? null : group,
          ip_allowlist: ips.length === 0 ? null : ips,
        },
      })
  }

  const groupLabel = (g: SelectableGroup) =>
    `${g.code} · ×${formatRatio(g.ratio)}${g.description ? ` · ${g.description}` : ''}`

  const rows = keys.data?.data ?? []
  const usageKey = rows.find((key) => key.id === usageKeyId)
  return (
    <div className="list-page">
      <PageHeader
        title={t('portal:keys')}
        description={t('portal:keysDesc')}
        icon={KeyRound}
        meta={
          keys.data && (
            <Badge variant="muted">{t('common:resultCount', { n: keys.data.total })}</Badge>
          )
        }
        action={
          <>
            <Button variant="outline" loading={keys.isFetching} onClick={() => void keys.refetch()}>
              {!keys.isFetching && <RotateCw className="h-4 w-4" />}
              {t('common:refresh')}
            </Button>
            {/* 密钥页是用户找"怎么用"的第一站：接入指南与新建并列 */}
            <Button variant="outline" onClick={() => guide.open()}>
              <BookOpen className="h-4 w-4" />
              {t('portal:keysGuideAction')}
            </Button>
            <Button onClick={() => openEditor({ mode: 'create' })}>
              <Plus className="h-4 w-4" />
              {t('portal:keyCreate')}
            </Button>
          </>
        }
      />
      {dialog}

      {rows.some((key) => key.usage_trend === undefined) && (
        <Alert tone="warning">{t('portal:keyTrendUpgradeHint')}</Alert>
      )}

      {sessionMsg !== null && (
        <Alert tone="warning" onClose={() => setSessionMsg(null)}>
          {sessionMsg}
        </Alert>
      )}

      {/* 创建后即时复制；是否可再次复制以服务端加密保存结果为准。 */}
      {minted !== null && (
        <Alert
          tone="warning"
          title={t('portal:keyMinted', { name: minted.name })}
          action={
            <div className="flex shrink-0 flex-wrap items-center gap-2">
              <Button size="sm" variant="outline" onClick={() => guide.open({ apiKey: minted.api_key })}>
                <BookOpen className="h-3.5 w-3.5" />
                {t('portal:keyMintedGuide')}
              </Button>
              <Button size="sm" variant="outline" onClick={() => setMinted(null)}>
                {t('portal:keyMintedDone')}
              </Button>
            </div>
          }
        >
          <div className="mt-2 flex items-center gap-2 rounded-md border border-border bg-card p-2">
            <code className="min-w-0 flex-1 font-mono text-xs break-all text-foreground">
              {minted.api_key}
            </code>
            <CopyButton value={minted.api_key} />
          </div>
          <span className="mt-1.5 block text-xs">{t(minted.copy_available ? 'portal:keyMintedSavedHint' : 'portal:keyMintedHint')}</span>
        </Alert>
      )}

      {keys.isError ? (
        <ErrorState message={describeError(keys.error)} onRetry={() => void keys.refetch()} />
      ) : keys.isPending ? (
        <TableSkeleton rows={3} cols={10} />
      ) : rows.length === 0 ? (
        <EmptyState
          icon={KeyRound}
          hint={t('portal:keysEmptyHint')}
          action={
            <Button onClick={() => openEditor({ mode: 'create' })}>
              <Plus className="h-4 w-4" />
              {t('portal:keyCreate')}
            </Button>
          }
        />
      ) : (
        <Table stickyHeader stickyFirstColumn
          className="min-w-[1100px] table-fixed [&_th]:px-2 [&_td]:px-2 [&_thead_th]:bg-muted/60 [&_thead_th]:text-foreground/70 [&_thead_tr>:first-child:not([colspan])]:bg-muted/60 [&_tbody_tr:hover>td:first-child:not([colspan])]:bg-[color-mix(in_srgb,var(--card)_60%,var(--accent))]"
          scrollResetKey={`${pager.offset}:${pager.limit}`} aria-label={t('portal:keys')}>
          <colgroup>
            {/* Explicit widths share extra desktop space; the name no longer absorbs it all. */}
            <col className="w-40" />
            <col className="w-[132px]" />
            <col className="w-22" />
            <col className="w-[104px]" />
            <col className="w-[108px]" />
            <col className="w-20" />
            <col className="w-20" />
            <col className="w-24" />
            <col className="w-[136px]" />
            <col className="w-[152px] md:w-[116px]" />
          </colgroup>
          <THead>
            <Tr>
              <Th>{t('portal:keyToken')}</Th>
              <Th>{t('portal:keyName')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('portal:keyValidityColumn')}</Th>
              <Th>{t('portal:keySpendQuotaColumn')}</Th>
              <Th>{t('portal:keyTotalRequestsColumn')}</Th>
              <Th>{t('portal:keyRpm')}</Th>
              <Th>{t('portal:keyCreated')}</Th>
              <Th>{t('portal:keyTrendTitle')}</Th>
              <Th className="text-center">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((k) => {
              const metadata = [
                `#${k.id}`,
                k.group_override && t('portal:keyGroupPinned', { group: k.group_override }),
                k.model_allowlist?.length && t('portal:keyModelsLimited', { n: k.model_allowlist.length }),
                k.ip_allowlist?.length && t('portal:keyIpPinned', { n: k.ip_allowlist.length }),
              ].filter(Boolean).join(' · ')
              const metadataHint = [metadata, k.model_allowlist?.join(', '), k.ip_allowlist?.join(', ')].filter(Boolean).join('\n')
              return (
              <Tr key={k.id}>
                <Td className="whitespace-nowrap">
                  <div className="flex min-w-0 items-center gap-1.5">
                    <code className="truncate rounded bg-muted/70 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">{k.key_prefix}…</code>
                    <KeyCopyButton id={k.id} name={k.name} status={k.copy_status} hasSession={me.data?.has_web_session === true} />
                  </div>
                </Td>
                <Td className="text-left font-medium" title={k.name}>
                  <div className="flex min-w-0 flex-col leading-tight">
                    <button
                      type="button"
                      className="inline-flex h-6 min-w-0 max-w-full items-center self-start rounded text-left text-foreground outline-none hover:text-primary hover:underline hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-primary/40"
                      aria-label={t('portal:keyUsageOpen', { name: k.name, id: k.id })}
                      aria-haspopup="dialog"
                      onClick={() => setUsageKeyId(k.id)}
                    >
                      <span className="truncate">{k.name}</span>
                    </button>
                    <span className="block h-4 min-w-0 max-w-full truncate text-[11px] leading-4 font-normal text-muted-foreground" title={metadataHint}>{metadata}</span>
                  </div>
                </Td>
                <Td>
                  <Badge dot variant={k.expires_at && dayjs(k.expires_at).isBefore(dayjs()) ? 'destructive' : k.status === 1 ? 'success' : 'muted'}>
                    {k.expires_at && dayjs(k.expires_at).isBefore(dayjs()) ? t('portal:keyExpired') : k.status === 1 ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td className="whitespace-nowrap text-xs text-muted-foreground" title={k.expires_at ? dayjs(k.expires_at).format('YYYY-MM-DD HH:mm:ss') : undefined}>
                  {k.expires_at ? <time dateTime={k.expires_at} className="block leading-4">
                    <span className="block">{dayjs(k.expires_at).format('YYYY-MM-DD')}</span>
                    <span className="block">{dayjs(k.expires_at).format('HH:mm')}</span>
                  </time> : t('portal:keyNoExpiry')}
                </Td>
                <Td className="text-left tabular-nums whitespace-nowrap">
                  <span className="block h-6 truncate leading-6 font-medium" title={formatMoney(k.used_micro, locale)}>{formatMoney(k.used_micro, locale)}</span>
                  <span className="block h-4 truncate text-[11px] leading-4 text-muted-foreground" title={k.quota_mode === 1 && k.quota_micro != null ? formatMoney(k.quota_micro, locale) : undefined}>{k.quota_mode === 1 && k.quota_micro != null ? t('portal:keyQuotaTotal', { amount: formatMoney(k.quota_micro, locale) }) : t('portal:keyUnlimited')}</span>
                </Td>
                <Td className="text-left tabular-nums"><span className="block truncate" title={formatCount(k.requests, locale)}>{formatCount(k.requests, locale)}</span></Td>
                <Td className="text-left tabular-nums text-muted-foreground">
                  <span className="block truncate" title={String(k.rpm_limit ?? '—')}>{k.rpm_limit ?? '—'}</span>
                </Td>
                <Td className="whitespace-nowrap text-xs text-muted-foreground">
                  {dayjs(k.created_at).format('YYYY-MM-DD')}
                </Td>
                <Td>
                  <KeyUsageSparkline trend={k.usage_trend} name={k.name} id={k.id} onClick={() => setUsageKeyId(k.id)} />
                </Td>
                <Td>
                  <div className="flex items-center justify-center gap-0.5">
                    <IconButton
                      icon={k.status === 1 ? PowerOff : Power}
                      label={k.status === 1 ? t('admin:keyDisable') : t('admin:keyEnable')}
                      onClick={() =>
                        patch.mutate({ id: k.id, body: { status: k.status === 1 ? 2 : 1 } })
                      }
                    />
                    <IconButton
                      icon={Pencil}
                      label={t('common:edit')}
                      onClick={() =>
                        openEditor({
                          mode: 'rename',
                          id: k.id,
                          name: k.name,
                          group: k.group_override,
                          ips: k.ip_allowlist ?? [],
                          limits: k,
                        })
                      }
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: k.name }),
                          description: t('common:confirmKeyDelete'),
                          requireText: k.name,
                          onConfirm: () => remove.mutate(k.id),
                        })
                      }
                    />
                  </div>
                </Td>
              </Tr>
              )
            })}
          </TBody>
        </Table>
      )}

      <Pagination {...pager} total={keys.data?.total} />

      {usageKey && <KeyUsageDrawer key={usageKey.id} apiKey={usageKey} onClose={() => setUsageKeyId(null)} />}

      <Drawer
        open={editor !== null}
        onClose={() => setEditor(null)}
        title={editor?.mode === 'rename' ? t('portal:keyRename') : t('portal:keyCreate')}
        description={editor?.mode === 'rename' ? undefined : t('portal:keyCreateHint')}
        footer={
          <>
            <Button variant="ghost" onClick={() => setEditor(null)}>
              {t('common:cancel')}
            </Button>
            <Button
              loading={create.isPending || patch.isPending}
              disabled={draft.trim() === '' || !!limitsError}
              onClick={submitEditor}
            >
              {editor?.mode === 'rename' ? t('common:save') : t('common:create')}
            </Button>
          </>
        }
      >
        <form
          className="flex flex-col gap-5"
          onSubmit={(e) => {
            e.preventDefault()
            submitEditor()
          }}
        >
          <Field label={t('portal:keyName')} htmlFor="key-name" hint={t('portal:keyNameHint')}>
            <Input id="key-name" value={draft} onChange={(e) => setDraft(e.target.value)} />
          </Field>
          {/* 分组选择：价随组走，可选集合由站长划定 */}
          {showGroupPicker && (
            <Field label={t('portal:keyGroup')} htmlFor="key-group" hint={t('portal:keyGroupHint')}>
              <Select
                id="key-group"
                className="w-full"
                value={group}
                onChange={setGroup}
                placeholder={t('portal:keyGroupFollow', { group: groups.data?.current ?? '' })}
                options={selectable.map((g) => ({ value: g.code, label: groupLabel(g) }))}
              />
            </Field>
          )}
          <KeyLimitsFields value={limits} onChange={setLimits} group={group || groups.data?.current || ''} quotaSupported={keys.data?.key_limits_supported === true} />
          {limitsError && <p role="alert" className="text-sm text-destructive">{t(limitsError)}</p>}
          {/* 来源 IP 白名单：只约束 /v1 调用，门户登录不受限（否则会把自己锁在门外） */}
          <Field label={t('portal:keyIp')} htmlFor="key-ips" hint={t('portal:keyIpHint')}>
            <TagInput
              id="key-ips"
              value={ips}
              onChange={setIps}
              placeholder={t('portal:keyIpPlaceholder')}
            />
          </Field>
        </form>
      </Drawer>
    </div>
  )
}
