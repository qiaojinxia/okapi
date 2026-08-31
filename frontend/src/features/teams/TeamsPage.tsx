import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { TeamRow } from '@/features/teams/types'
import { ApiError, apiFetch } from '@/lib/api'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
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



export function TeamsPage() {
  const { t } = useTranslation()
  const [active, setActive] = useState<number | null>(null)

  const teams = useQuery({
    queryKey: qk.myTeams,
    queryFn: () => apiFetch<{ data: TeamRow[] }>('/api/teams'),
    retry: false,
  })

  if (teams.isError && sessionRequired(teams.error)) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>{t('team:title')}</CardTitle>
        </CardHeader>
        <CardContent className="text-sm text-muted-foreground">
          {t('team:sessionRequired')}
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="flex flex-col gap-4">
      <TeamListCard
        teams={teams.data?.data ?? []}
        error={teams.isError ? describeError(teams.error) : null}
        active={active}
        onPick={setActive}
      />
      {active !== null && <TeamDetailCard teamId={active} />}
    </div>
  )
}
