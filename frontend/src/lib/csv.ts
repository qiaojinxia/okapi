import dayjs from 'dayjs'

/// 客户端生成 CSV 并触发下载（无后端往返）。
///
/// 只导出调用方已加载在内存里的行——全量导出属报表任务（可能几十万行），
/// 不该由浏览器一次拉完。含分隔符/引号/换行的字段按 RFC 4180 加引号；
/// 前置 BOM 让 Excel 直接按 UTF-8 打开（否则中文模型名/key 名乱码）。
export function downloadCsv(baseName: string, head: string[], rows: unknown[][]) {
  const cell = (v: unknown) => {
    const s = v === null || v === undefined ? '' : String(v)
    return /[",\n]/.test(s) ? `"${s.replaceAll('"', '""')}"` : s
  }
  const body = [head.join(','), ...rows.map((r) => r.map(cell).join(','))].join('\n')
  const blob = new Blob([`\uFEFF${body}`], { type: 'text/csv;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `${baseName}-${dayjs().format('YYYYMMDD-HHmm')}.csv`
  a.click()
  URL.revokeObjectURL(url)
}

/// micro-USD → 六位小数 USD 字符串。导出表的读者是人和 Excel，不是程序。
export function microToUsd(micro: number): string {
  return (micro / 1_000_000).toFixed(6)
}
