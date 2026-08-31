export interface GroupListRow {
  group_code: string
  group_ratio: string | null
  description: string | null
  is_default: boolean
  user_count: number
  channel_count: number
  pool_code: string | null
}
