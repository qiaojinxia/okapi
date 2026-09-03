import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Users } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { TeamRow } from '@/features/teams/types'
import { ApiError, apiFetch } from '@/lib/api'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { toast } from '@/components/ui/toast'
import { TeamDetailCard } from '@/features/teams/TeamDetailCard'
import { TeamListCard } from '@/features/teams/TeamListCard'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// Team 层为 web session 鉴权（成员自助），与门户的 API key 单轨不同：
/// 用 API Key 方式登录的浏览器没有 session cookie，会 401。此处统一降级为提示，
/// 引导改用邮箱密码登录，而不是让页面看起来"没有团队"。
function sessionRequired(err: unknown): boolean {
  return err instanceof ApiError && err.status === 401
}

/// 团队页：列表为主体，新建走小抽屉，管理某个团队走大抽屉——
/// 此前"建团表单 + 列表 + 详情卡"纵向堆在一屏，选中一个团队后详情出现在列表下方，
/// 要往下滚才知道点击生效了。
export function TeamsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [active, setActive] = useState<number | null>(null)
  const [creating, setCreating] = useState(false)
  const [name, setName] = useState('')

  const teams = useQuery({
    queryKey: qk.myTeams,
    queryFn: () => apiFetch<{ data: TeamRow[] }>('/api/teams'),
    retry: false,
  })

  const create = useMutation({
    mutationFn: () =>
      apiFetch<{ team_id: number }>('/api/teams', {
        method: 'POST',
        body: { name: name.trim() },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      setName('')
      setCreating(false)
      void queryClient.invalidateQueries({ queryKey: qk.myTeams })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const locked = teams.isError && sessionRequired(teams.error)
  const activeTeam = (teams.data?.data ?? []).find((tm) => tm.team_id === active)

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('team:title')}
        description={t('team:hint')}
        icon={Users}
        action={
          !locked && (
            <Button onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" />
              {t('team:create')}
            </Button>
          )
        }
      />

      {locked ? (
        <Alert tone="warning" title={t('team:title')}>
          {t('team:sessionRequired')}
        </Alert>
      ) : (
        <TeamListCard
          teams={teams.data?.data ?? []}
          loading={teams.isPending}
          error={teams.isError ? describeError(teams.error) : null}
          onPick={setActive}
          onCreate={() => setCreating(true)}
        />
      )}

      <Drawer
        open={creating}
        onClose={() => setCreating(false)}
        title={t('team:create')}
        description={t('team:createHint')}
        footer={
          <>
            <Button variant="ghost" onClick={() => setCreating(false)}>
              {t('common:cancel')}
            </Button>
            <Button loading={create.isPending} disabled={name.trim() === ''} onClick={() => create.mutate()}>
              {t('team:create')}
            </Button>
          </>
        }
      >
        <form
          onSubmit={(e) => {
            e.preventDefault()
            if (name.trim() !== '') create.mutate()
          }}
        >
          <Field label={t('team:name')} htmlFor="tname">
            <Input id="tname" value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
        </form>
      </Drawer>

      {active !== null && (
        <Drawer
          open
          size="lg"
          onClose={() => setActive(null)}
          title={activeTeam ? activeTeam.name : t('team:detail', { id: active })}
          description={t('team:detail', { id: active })}
        >
          <TeamDetailCard teamId={active} />
        </Drawer>
      )}
    </div>
  )
}
