// 路由 search 解析小工具，给各路由的 `validateSearch` 用。
//
// 路由器默认按 JSON 解析每个值：`?user_id=42` 到这里已是 number、`?errors_only=true`
// 已是 boolean，`?q=123` 也会变成 number 123；但手敲 URL 或旧链接可能是字符串，两种都认。
// 非法值一律回 undefined（= 未设置），不抛错——地址栏里一个错字不该把整页打成错误态。

/// 非空文本（去首尾空白，截到 `max`）。数字 / 布尔来自 JSON 解析的"像数字的搜索词"，还原成字符串。
export function text(v: unknown, max = 256): string | undefined {
  if (typeof v !== 'string' && typeof v !== 'number' && typeof v !== 'boolean') return undefined
  const s = String(v).trim()
  return s === '' ? undefined : s.slice(0, max)
}

/// 正整数。
export function posInt(v: unknown): number | undefined {
  const n = typeof v === 'number' ? v : typeof v === 'string' ? Number(v) : Number.NaN
  return Number.isSafeInteger(n) && n > 0 ? n : undefined
}

/// 开关：只认 true / 'true'，其余视为未设置（false 不写进地址）。
export function flag(v: unknown): true | undefined {
  return v === true || v === 'true' ? true : undefined
}

/// 枚举：值必须在 `allowed` 内；数字形式的枚举值（`?status=1` 解析成 1）按字符串比对。
export function oneOf<T extends string>(v: unknown, allowed: readonly T[]): T | undefined {
  const s = typeof v === 'string' || typeof v === 'number' ? String(v) : undefined
  return s !== undefined && (allowed as readonly string[]).includes(s) ? (s as T) : undefined
}
