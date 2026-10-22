/// 偏好类的存储写入：配额满、隐私模式或存储被禁时 `setItem` 会抛。这类写入只是“记住这次的选择”，
/// 失败应当降级成“这次不记住”，而不是把一次点击变成未捕获异常（点哪炸哪）。
/// 登录凭证（`api.ts`）的写入不走这里：那是核心状态，写不进去应当让登录失败，而不是装作成功。
/// `value` 为 `null` 表示删除这一项。
export function rememberLocal(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key)
    else localStorage.setItem(key, value)
  } catch {
    // 存储不可用：只是不记住
  }
}

/// 存储读取：站点数据被禁用时，连 `localStorage` 这个属性都会抛 `SecurityError`。
/// 很多读取发生在渲染里（初始 state、`useSyncExternalStore` 的快照），一抛就是整页白屏；
/// 读不到就当没存过。
export function readLocal(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}
