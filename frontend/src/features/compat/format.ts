import dayjs from 'dayjs'
import type { TFunction } from 'i18next'
import { ApiError } from '@/lib/api'
import { describeError } from '@/lib/i18n'

export const fmtAt = (iso: string | null | undefined): string =>
  iso === null || iso === undefined ? '—' : dayjs(iso).format('YYYY-MM-DD HH:mm')

/// 后端对「缺哪个权限点」只回 `permission_denied` + param；适配规则里有两个权限点的缺失值得点名：
/// 发布草稿要 `compat.publish`，新增敏感写入要 `compat.sensitive`。
export function describeCompatError(err: unknown, t: TFunction): string {
  if (err instanceof ApiError && err.code === 'permission_denied') {
    if (err.param === 'compat.sensitive') return t('compat:needSensitive')
    if (err.param === 'compat.publish') return t('compat:needPublish')
  }
  return describeError(err)
}

/// `builtin:deepseek` → `deepseek`；`custom:acme` 原样（档案）。
export const bareVendor = (id: string | null | undefined): string => (id ?? '').replace(/^builtin:/, '')
