const INT32_MIN = -2_147_483_648
const INT32_MAX = 2_147_483_647

/** 有符号 32 位整数（库里 integer 列）。空串返回 undefined；不合法（含输入中途的 "-"、小数、科学计数、越界）返回 null。 */
export function parseInt32(text: string): number | undefined | null {
  const trimmed = text.trim()
  if (trimmed === '') return undefined
  if (!/^-?\d+$/.test(trimmed)) return null
  const value = Number(trimmed)
  return value >= INT32_MIN && value <= INT32_MAX ? value : null
}

/** 非负整数，上限 `max`（缺省 JS 安全整数）。空串返回 undefined；不合法（字母、小数、负数、越界）返回 null——调用方据此拦下提交，不悄悄按 0 / 不限处理。 */
export function parseNonNegativeInt(text: string, max = Number.MAX_SAFE_INTEGER): number | undefined | null {
  const trimmed = text.trim()
  if (trimmed === '') return undefined
  if (!/^\d+$/.test(trimmed)) return null
  const value = Number(trimmed)
  return value <= max ? value : null
}
