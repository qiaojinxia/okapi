/// 内置角色三档（后端 assign_role 只接受 1 / 10 / 100）。
export const BUILTIN_ROLES = [1, 10, 100] as const

/// 角色名：顶栏、个人中心、用户列表与抽屉共用。
export function roleLabel(role: number, t: (k: string) => string): string {
  if (role >= 100) return t('common:roleSuper')
  if (role >= 10) return t('common:roleAdmin')
  return t('common:roleUser')
}
