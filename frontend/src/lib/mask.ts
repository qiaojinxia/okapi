/// 订阅账号（OAuth 登录的邮箱）在界面上一律遮住中间部分，截图 / 录屏 / 共享屏幕时不会带出完整账号。
///
/// 遮法：邮箱只遮 @ 前面的用户名，域名原样留着（看得出是哪家邮箱、方便区分账号）；用户名留头尾各一两个字符，
/// 中间固定三个星号——不管原来多长都是三个，长度本身也不泄漏。
/// 例：`laird_phelpsdev@example.com` → `la***ev@example.com`，`bob.lee@x.com` → `b***e@x.com`。
///
/// 只用于**展示**：编辑框里的值、提交给接口的值、导出的 CSV 都保持原样。

// 用户名字符集里带 *：遮过的邮箱（la***ev@…）整个匹配上，原样放过，不会被再遮一次
const EMAIL = /[A-Za-z0-9._%+*-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+/g
const MASK = '***'

/// 按字符（不是 UTF-16 单元）遮住中间：8 个字符以上留头尾各 2 个，4–7 个留各 1 个，更短只留头 1 个。
function hideMiddle(value: string): string {
  if (value.includes(MASK)) return value
  const chars = [...value]
  if (chars.length === 0) return ''
  if (chars.length <= 3) return `${chars[0]}${MASK}`
  const keep = chars.length >= 8 ? 2 : 1
  return `${chars.slice(0, keep).join('')}${MASK}${chars.slice(-keep).join('')}`
}

/// 一段文字里出现的邮箱都遮住，其余原样。渠道名常常就是订阅账号的邮箱，
/// 但也可能是 `openai-main` 这样的普通名字——后者不是账号，不动。
export function maskEmails(text: string): string {
  return text.replace(EMAIL, (email) => {
    const at = email.lastIndexOf('@')
    return `${hideMiddle(email.slice(0, at))}${email.slice(at)}`
  })
}

/// 渠道名：只有「识别出它是订阅账号」才遮中间——名字本身是邮箱，或者等于这条渠道某把 key 的账号标识
/// （账号不一定是邮箱）。`openai-main` 这样的普通渠道名原样显示。
export function maskChannelName(name: string, accounts: readonly (string | null | undefined)[] = []): string {
  const same = name.trim().toLowerCase()
  if (same !== '' && accounts.some((account) => typeof account === 'string' && account.trim().toLowerCase() === same)) return maskAccount(name)
  return maskEmails(name)
}

/// 已知是订阅账号标识的字符串（`account_label`）：邮箱按邮箱遮，不是邮箱的（账号 ID / 用户名）整串遮中间。
export function maskAccount(label: string): string {
  const masked = maskEmails(label)
  return masked !== label ? masked : hideMiddle(label)
}
