import { apiFetch, ApiError } from '@/lib/api'

/// 选择器要的是完整目录，与列表页的分页无关：先按接口缺省取第一页（不带 total 的老接口一次就回全部），
/// 不够再按 200 一页翻到 total 为止。后端忽略 offset（翻回重复的行）、给出不合法的 total、或还没到 total
/// 就回了空页时报错，而不是死循环或静默交出重复 / 残缺的目录。
export async function loadAll<T>(base: string, id: (row: T) => string | number): Promise<T[]> {
  const data: T[] = []
  const seen = new Set<string | number>()
  let total: number | undefined
  for (;;) {
    const page = await apiFetch<{ data: T[]; total?: number }>(data.length === 0 ? base : `${base}?limit=200&offset=${data.length}`)
    total = page.total ?? total
    if (total !== undefined && (!Number.isSafeInteger(total) || total < 0)) {
      throw new ApiError(502, 'internal_error')
    }
    for (const row of page.data) {
      if (seen.has(id(row))) throw new ApiError(502, 'internal_error')
      seen.add(id(row))
      data.push(row)
    }
    if (total === undefined || data.length >= total) return data
    if (page.data.length === 0) throw new ApiError(502, 'internal_error')
  }
}
