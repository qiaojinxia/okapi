/// 内置角色三档（后端 assign_role 只接受 1 / 10 / 100）。
export const BUILTIN_ROLES = [1, 10, 100] as const


export interface RoleRow {
  id: number
  role_code: string
  display_name: string
}



export interface Overview {
  user: {
    id: number
    username: string
    role: number
    status: number
    balance_micro: number
    /// 个人计价系数（十进制字符串；1 = 站内标准价）。
    price_multiplier: string
  }
  groups: { code: string; priority: number }[]
  keys: { id: number; name: string; key_prefix: string; status: number; used_micro: number }[]
}



export function roleLabel(role: number, t: (k: string) => string): string {
  if (role >= 100) return t('admin:roleSuper')
  if (role >= 10) return t('admin:roleAdmin')
  return t('admin:roleUser')
}
