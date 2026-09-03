import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { KeyRound, Pencil, Plus, Power, PowerOff, Trash2 } from 'lucide-react'
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
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { Select } from '@/components/ui/select'
import { formatCount, formatMoney, formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface KeyRow {
  id: number
  name: string
  key_prefix: string
  status: number
  used_micro: number
  rpm_limit: number | null
  created_at: string
  amount_micro: number
  requests: number
  /// 这把 key 钉住的分组；null = 跟随用户分组。
  group_override: string | null
}

interface SelectableGroup {
  code: string
  ratio: string
  description: string | null
  source: 'assigned' | 'self_select' | 'default'
}

type Editor =
  | { mode: 'create' }
  | { mode: 'rename'; id: number; name: string; group: string | null }

/// 门户密钥页：列表 + 自助新建（new-api 令牌页的"添加令牌"）。
///
/// 新建走 `/auth/keys`（会话鉴权，与 Team/TOTP 同轨）：邮箱密码登录的用户在这里
/// 直接建；API Key 登录的浏览器没有 session 会 401——降级为"请改用邮箱密码登录"
/// 而不是哑按钮。明文只在生成时返回一次，页面上给一次复制机会，离开即不可再取。
///
/// 新建与改名都走抽屉：此前改名表单出现在表格上方、与被改的那一行相隔半屏，
/// 看不出改的是哪一把。
export function PortalKeysPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [editor, setEditor] = useState<Editor | null>(null)
  const [draft, setDraft] = useState('')
  // 档位：'' = 跟随用户分组
  const [group, setGroup] = useState('')
  const [minted, setMinted] = useState<{ name: string; api_key: string } | null>(null)
  const [sessionMsg, setSessionMsg] = useState<string | null>(null)
  const { confirm, dialog } = useConfirm()

  const keys = useQuery({
    queryKey: qk.keys,
    queryFn: () => apiFetch<{ data: KeyRow[] }>('/api/me/keys'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.keys })

  // 可选档位（管理员分配的 ∪ 站点开放自选的 ∪ 默认组）：只有一个可选时不显示选择器
  const groups = useQuery({
    queryKey: qk.myGroups,
    queryFn: () => apiFetch<{ current: string; data: SelectableGroup[] }>('/api/me/groups'),
    staleTime: 60_000,
  })
  const selectable = groups.data?.data ?? []
  const showGroupPicker = selectable.length > 1

  const openEditor = (e: Editor) => {
    setEditor(e)
    setDraft(e.mode === 'rename' ? e.name : '')
    setGroup(e.mode === 'rename' ? (e.group ?? '') : '')
  }

  const create = useMutation({
    mutationFn: (arg: { name: string; group: string }) =>
      apiFetch<{ key_id: number; api_key: string }>('/auth/keys', {
        method: 'POST',
        body: { name: arg.name, group_code: arg.group === '' ? undefined : arg.group },
      }),
    onSuccess: (r, arg) => {
      setMinted({ name: arg.name, api_key: r.api_key })
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
    if (name === '' || editor === null) return
    if (editor.mode === 'create') create.mutate({ name, group })
    else patch.mutate({ id: editor.id, body: { name, group_code: group === '' ? null : group } })
  }

  const groupLabel = (g: SelectableGroup) =>
    `${g.code} · ×${formatRatio(g.ratio)}${g.description ? ` · ${g.description}` : ''}`

  const rows = keys.data?.data ?? []
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('portal:keys')}
        description={t('portal:keysDesc')}
        icon={KeyRound}
        meta={
          keys.data && (
            <Badge variant="muted">{t('common:resultCount', { n: rows.length })}</Badge>
          )
        }
        action={
          <Button onClick={() => openEditor({ mode: 'create' })}>
            <Plus className="h-4 w-4" />
            {t('portal:keyCreate')}
          </Button>
        }
      />
      {dialog}

      {sessionMsg !== null && (
        <Alert tone="warning" onClose={() => setSessionMsg(null)}>
          {sessionMsg}
        </Alert>
      )}

      {/* 明文只此一次：醒目框 + 复制；刷新或离开就没了，文案把话说死 */}
      {minted !== null && (
        <Alert
          tone="warning"
          title={t('portal:keyMinted', { name: minted.name })}
          action={
            <Button size="sm" variant="outline" onClick={() => setMinted(null)}>
              {t('portal:keyMintedDone')}
            </Button>
          }
        >
          <div className="mt-2 flex items-center gap-2 rounded-md border border-border bg-card p-2">
            <code className="min-w-0 flex-1 font-mono text-xs break-all text-foreground">
              {minted.api_key}
            </code>
            <CopyButton value={minted.api_key} />
          </div>
          <span className="mt-1.5 block text-xs">{t('portal:keyMintedHint')}</span>
        </Alert>
      )}

      {keys.isError ? (
        <ErrorState message={describeError(keys.error)} onRetry={() => void keys.refetch()} />
      ) : keys.isPending ? (
        <TableSkeleton rows={3} cols={8} />
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
        <Table>
          <THead>
            <Tr>
              <Th>{t('portal:keyName')}</Th>
              <Th>{t('portal:keyPrefix')}</Th>
              <Th>{t('common:status')}</Th>
              <Th numeric>{t('portal:keyUsed')}</Th>
              <Th numeric>{t('portal:keyRequests')}</Th>
              <Th numeric>{t('portal:keyRpm')}</Th>
              <Th>{t('portal:keyCreated')}</Th>
              <Th className="text-right">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((k) => (
              <Tr key={k.id}>
                <Td className="max-w-56 font-medium" title={k.name}>
                  <div className="flex flex-col leading-tight">
                    <span className="truncate">{k.name}</span>
                    {k.group_override && (
                      <span className="text-xs font-normal text-muted-foreground">
                        {t('portal:keyGroupPinned', { group: k.group_override })}
                      </span>
                    )}
                  </div>
                </Td>
                <Td className="whitespace-nowrap">
                  <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{k.key_prefix}…</code>
                </Td>
                <Td>
                  <Badge dot variant={k.status === 1 ? 'success' : 'muted'}>
                    {k.status === 1 ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td numeric className="whitespace-nowrap">
                  {formatMoney(k.amount_micro || k.used_micro, locale)}
                </Td>
                <Td numeric>{formatCount(k.requests, locale)}</Td>
                <Td numeric className="text-muted-foreground">
                  {k.rpm_limit ?? '—'}
                </Td>
                <Td className="whitespace-nowrap text-xs text-muted-foreground">
                  {dayjs(k.created_at).format('YYYY-MM-DD')}
                </Td>
                <Td>
                  <div className="flex items-center justify-end gap-0.5">
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
                        openEditor({ mode: 'rename', id: k.id, name: k.name, group: k.group_override })
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
            ))}
          </TBody>
        </Table>
      )}

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
              disabled={draft.trim() === ''}
              onClick={submitEditor}
            >
              {editor?.mode === 'rename' ? t('common:save') : t('common:create')}
            </Button>
          </>
        }
      >
        <form
          onSubmit={(e) => {
            e.preventDefault()
            submitEditor()
          }}
        >
          <Field label={t('portal:keyName')} htmlFor="key-name" hint={t('portal:keyNameHint')}>
            <Input id="key-name" value={draft} onChange={(e) => setDraft(e.target.value)} />
          </Field>
          {/* 档位自选（new-api 令牌分组的对应物）：价随组走，可选集合由站长划定 */}
          {showGroupPicker && (
            <Field label={t('portal:keyGroup')} htmlFor="key-group" hint={t('portal:keyGroupHint')}>
              <Select
                id="key-group"
                value={group}
                onChange={setGroup}
                placeholder={t('portal:keyGroupFollow', { group: groups.data?.current ?? '' })}
                options={selectable.map((g) => ({ value: g.code, label: groupLabel(g) }))}
              />
            </Field>
          )}
        </form>
      </Drawer>
    </div>
  )
}
