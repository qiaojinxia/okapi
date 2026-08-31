import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { Overview, RoleRow } from '@/features/users/types'
import { Badge } from '@/components/ui/badge'
import { BalanceSection } from '@/features/users/BalanceSection'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { ErrorState } from '@/components/ui/state'
import { GroupsSection } from '@/features/users/GroupsSection'
import { RoleSection } from '@/features/users/RoleSection'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { roleLabel } from '@/features/users/types'
import { useConfirm } from '@/components/ui/confirm'

/// 单个用户的管理抽屉：状态、角色、余额、分组四类动作分段呈现。
///
/// 每段独立提交（后端也是独立端点，审计语义不同），故不设统一"保存"按钮——
/// 一个大保存会让人误以为改了余额也要点它才生效。
export function UserDrawer({ userId, onClose }: { userId: number; onClose: () => void }) {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const { confirm, dialog } = useConfirm()

  const overview = useQuery({
    queryKey: qk.userOverview(userId),
    queryFn: () => apiFetch<Overview>(`/admin/users/${userId}/overview`),
  })
  const roles = useQuery({
    queryKey: qk.adminRoles,
    queryFn: () => apiFetch<{ data: RoleRow[] }>('/admin/roles'),
  })

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: qk.userOverview(userId) })
    void queryClient.invalidateQueries({ queryKey: ['admin', 'users'] })
  }

  const ov = overview.data
  const banned = ov?.user.status === 2

  // 统一管理动作端点：封禁/删除会连带吊销该用户令牌并刷新鉴权缓存
  const manage = useMutation({
    mutationFn: (action: 'ban' | 'unban' | 'promote' | 'demote' | 'delete') =>
      apiFetch(`/admin/users/${userId}/manage`, { method: 'POST', body: { action } }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:usersSelected', { id: userId })}
      description={t('admin:userDrawerDesc')}
      footer={
        <>
          {msg !== null && <span className="mr-auto text-xs text-muted-foreground">{msg}</span>}
          <Button variant="ghost" onClick={onClose}>
            {t('common:close')}
          </Button>
        </>
      }
    >
      {dialog}

      <FieldGroup title={t('admin:userProfile')}>
        {overview.isError ? (
          <ErrorState message={describeError(overview.error)} />
        ) : (
          <div className="flex flex-wrap gap-2">
            <Badge variant="muted">
              {t('admin:username')} {ov?.user.username ?? '—'}
            </Badge>
            <Badge variant="muted">
              {t('common:balance')} {formatMoney(ov?.user.balance_micro ?? 0, i18n.language)}
            </Badge>
            <Badge variant="muted">
              {t('admin:usersRole')} {roleLabel(ov?.user.role ?? 1, t)}
            </Badge>
            {(ov?.groups ?? []).map((g) => (
              <Badge key={g.code} variant="muted">
                {t('portal:group')} {g.code}
              </Badge>
            ))}
            <Badge variant="muted">
              {t('portal:keys')} {(ov?.keys ?? []).length}
            </Badge>
            <Badge variant={banned ? 'destructive' : 'success'}>
              {banned ? t('common:disabled') : t('common:enabled')}
            </Badge>
          </div>
        )}
      </FieldGroup>

      <FieldGroup title={t('admin:userActions')} hint={t('admin:banHint')}>
        <div className="flex flex-wrap gap-2">
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              // 解封无风险，直接执行；封禁会吊销该用户全部令牌，需确认
              if (banned) {
                manage.mutate('unban')
                return
              }
              confirm({
                title: t('admin:ban'),
                description: t('common:confirmUserAction', { action: t('admin:ban') }),
                confirmLabel: t('admin:ban'),
                onConfirm: () => manage.mutate('ban'),
              })
            }}
          >
            {banned ? t('admin:unban') : t('admin:ban')}
          </Button>
          <Button size="sm" variant="outline" onClick={() => manage.mutate('promote')}>
            {t('admin:promote')}
          </Button>
          <Button size="sm" variant="outline" onClick={() => manage.mutate('demote')}>
            {t('admin:demote')}
          </Button>
          <Button
            size="sm"
            variant="destructive"
            onClick={() =>
              confirm({
                title: t('admin:softDelete'),
                description: t('common:confirmUserAction', { action: t('admin:softDelete') }),
                onConfirm: () => manage.mutate('delete'),
              })
            }
          >
            {t('admin:softDelete')}
          </Button>
        </div>
      </FieldGroup>

      <RoleSection userId={userId} roles={roles.data?.data ?? []} onMsg={setMsg} onDone={invalidate} />
      <BalanceSection userId={userId} onMsg={setMsg} onDone={invalidate} />
      <GroupsSection
        userId={userId}
        current={(ov?.groups ?? []).map((g) => g.code)}
        onMsg={setMsg}
        onDone={invalidate}
      />
    </Drawer>
  )
}
