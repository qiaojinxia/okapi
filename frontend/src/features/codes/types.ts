export interface CreateResp {
  batch_id: string
  codes: string[]
}

/// 兑换码状态（与后端 redemption_codes.status 一致）。
export const CODE_STATUS = { unused: 1, used: 2, disabled: 3 } as const

/// 列表状态筛选的合法取值（URL `?status=`）；空 = 全部。
export const CODE_STATUS_FILTERS = ['1', '2', '3'] as const
export type CodeStatusFilter = (typeof CODE_STATUS_FILTERS)[number]

/// `/admin/codes` 的 search params。
export interface CodesSearch {
  page?: number
  limit?: number
  status?: CodeStatusFilter
}
