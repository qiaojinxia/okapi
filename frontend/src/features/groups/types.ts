export interface GroupListRow {
  group_code: string
  group_ratio: string | null
  description: string | null
  is_default: boolean
  user_count: number
  channel_count: number
  /// 分组必有池（缺省 default）。
  pool_code: string
  /// 用户可在门户为自己的 key 自选此分组。
  self_select: boolean
  /// 分组内每用户每分钟 / 每小时请求上限；null = 不限。
  rpm_limit: number | null
  rph_limit: number | null
}
