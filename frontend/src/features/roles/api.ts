import { queryOptions } from '@tanstack/react-query'
import { apiFetch } from '@/lib/api'
import { loadAll } from '@/lib/load-all'
import { qk, type Page } from '@/lib/query-keys'

// `/admin/roles` 的接口层：角色页与用户抽屉的角色下拉共用这一份行类型（后端 console admin/roles.rs）。

export interface RoleRow {
  id: number
  role_code: string
  display_name: string
  /// 权限点列表（JSON 数组）；编辑抽屉自己解析。
  permissions: unknown
}

export interface RolesPage {
  data: RoleRow[]
  total: number
}

/// 一页角色（角色页）。
export function fetchRoles(page: Page): Promise<RolesPage> {
  return apiFetch<RolesPage>(`/admin/roles?limit=${page.limit}&offset=${page.offset}`)
}

/// 全部角色（用户抽屉的角色下拉）。不带参数的 `/admin/roles` 只回第一页（后端缺省 20 条），
/// 此前下拉就只列得出前 20 个角色。
export const roleOptions = () => queryOptions({
  queryKey: qk.adminRoleOptions,
  queryFn: async (): Promise<RolesPage> => {
    const data = await loadAll<RoleRow>('/admin/roles', (role) => role.id)
    return { data, total: data.length }
  },
})
