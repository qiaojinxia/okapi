export interface TeamRow {
  team_id: number
  name: string
  role: string
  member_count: number
  monthly_spend_limit_micro: number | null
  balance_micro: number
}

/// 团内角色三档（后端存 owner / admin / member 字符串）。未知值原样返回，避免新角色把列表打空。
export function teamRoleLabel(role: string, t: (k: string) => string): string {
  if (role === 'owner') return t('team:roleOwner')
  if (role === 'admin') return t('team:roleAdmin')
  if (role === 'member') return t('team:roleMember')
  return role
}



export interface MemberRow {
  member_user_id: number
  username: string
  role: string
  monthly_spend_limit_micro: number | null
  total_spend_micro: number
  month_spend_micro: number
}



export interface UsageResp {
  team_id: number
  balance_micro: number
  members: MemberRow[]
}
