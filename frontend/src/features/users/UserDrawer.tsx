import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ScrollText } from 'lucide-react'
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
import { UsageSection } from '@/features/users/UsageSection'
import { Tabs } from '@/components/ui/tabs'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { roleLabel } from '@/features/users/types'
import { useConfirm } from '@/components/ui/confirm'

const USER_TABS = ['usage', 'actions', 'role', 'balance', 'groups'] as const
type UserTab = (typeof USER_TABS)[number]

/// 单个用户的管理抽屉：概览常驻（改任何东西前都该先看清对象），
/// 用量 / 状态动作 / 角色 / 余额 / 分组五页签——此前多段纵排，
/// 调余额要先滚过封禁按钮，误触面太大。
/// "用量"放首签且为落地签：处理用户的第一步永远是看他的行为
/// （花多少、用什么、上次动过什么账），动作签在其后。
///
/// 每段独立提交（后端也是独立端点，审计语义不同），故不设统一"保存"按钮——
/// 一个大保存会让人误以为改了余额也要点它才生效。
export function UserDrawer({ userId, onClose }: { userId: number; onClose: () => void }) {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [tab, setTab] = useState<UserTab>('usage')
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
    // 调完余额切回"用量"签，最近变动里就该看到刚那一笔
    void queryClient.invalidateQueries({ queryKey: qk.userUsage(userId) })
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
            {/* 处理单个用户时的下一步几乎总是"看他最近调了什么"——直达其明细 */}
            <Link
              to="/admin/logs"
              search={{ user_id: userId, hours: 168 }}
              className="inline-flex items-center gap-1 text-xs text-primary underline decoration-dotted"
            >
              <ScrollText className="h-3 w-3" />
              {t('admin:userViewLogs')}
            </Link>
          </div>
        )}
      </FieldGroup>

      <Tabs
        className="mb-4"
        items={USER_TABS.map((id) => ({
          id,
          label: t(
            (
              {
                usage: 'admin:userUsageTab',
                actions: 'admin:userActions',
                role: 'admin:usersRole',
                balance: 'common:balance',
                // 页签用短名；userGroups 是表单字段标签（带"逗号分隔，首个优先"说明），
                // 当页签名会把页签栏挤成两行
                groups: 'admin:userGroupsTab',
              } as const
            )[id],
          ),
        }))}
        active={tab}
        onChange={(id) => setTab(id as UserTab)}
      />

      {tab === 'usage' && <UsageSection userId={userId} />}
      {tab === 'actions' && (
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
      )}

      {tab === 'role' && (
        <RoleSection
          userId={userId}
          roles={roles.data?.data ?? []}
          onMsg={setMsg}
          onDone={invalidate}
        />
      )}
      {tab === 'balance' && <BalanceSection userId={userId} onMsg={setMsg} onDone={invalidate} />}
      {tab === 'groups' && (
        <GroupsSection
          userId={userId}
          current={(ov?.groups ?? []).map((g) => g.code)}
          onMsg={setMsg}
          onDone={invalidate}
        />
      )}
    </Drawer>
  )
}
