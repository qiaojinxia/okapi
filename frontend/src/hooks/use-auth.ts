import { useQuery } from '@tanstack/react-query'
import { apiFetch, getKey } from '@/lib/api'
import { qk } from '@/lib/query-keys'

export interface Me {
  user_id: number
  key_id: number
  group: string
  balance_micro: number
  /// 余额有效期（#1790-6）；null = 不过期。RFC3339。
  balance_expires_at: string | null
  /// 1=user 10=admin 100=super_admin（对齐 new-api）。
  role: number
  /// 生效权限点；`["*"]` = 全权，空数组 = 无管理权限。
  permissions: string[]
}

export function useMe() {
  return useQuery({
    queryKey: qk.me,
    queryFn: () => apiFetch<Me>('/api/me'),
    enabled: getKey() !== null,
    staleTime: 15_000,
  })
}

/// 按权限点判断某个入口是否该出现。
///
/// 与后端 `AuthedKey::has_permission` 同语义（`*` 通配全权）。前端只用它决定
/// **显示**，真正的拦截仍在后端——隐藏入口是体验，不是安全边界。
///
/// 数据未回来时返回 false（宁可少显示一瞬，也不要闪出一个马上消失的入口）。
export function usePermission(): (permission: string) => boolean {
  const me = useMe()
  const perms = me.data?.permissions
  return (permission: string) => {
    if (perms === undefined) return false
    return perms.includes('*') || perms.includes(permission)
  }
}
