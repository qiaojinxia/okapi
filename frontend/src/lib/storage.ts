/// 偏好类的存储写入：配额满、隐私模式或存储被禁时 `setItem` 会抛。这类写入只是“记住这次的选择”，
/// 失败应当降级成“这次不记住”，而不是把一次点击变成未捕获异常（点哪炸哪）。
/// 登录凭证（`api.ts`）不走这里：那是核心状态，写不进去应当让登录失败，而不是装作成功。
/// `value` 为 `null` 表示删除这一项。
export function rememberLocal(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key)
    else localStorage.setItem(key, value)
  } catch {
    // 存储不可用：只是不记住
  }
}
