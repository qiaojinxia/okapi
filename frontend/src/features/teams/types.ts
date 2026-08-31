export interface TeamRow {
  team_id: number
  name: string
  role: string
  member_count: number
  monthly_spend_limit_micro: number | null
  balance_micro: number
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
