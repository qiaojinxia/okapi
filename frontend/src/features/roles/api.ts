import { apiFetch } from '@/lib/api'
import type { Page } from '@/lib/query-keys'

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

/// 一页角色；不传 page 取全部（用户抽屉的角色下拉）。
export function fetchRoles(page?: Page): Promise<RolesPage> {
  return apiFetch<RolesPage>(page ? `/admin/roles?limit=${page.limit}&offset=${page.offset}` : '/admin/roles')
}
