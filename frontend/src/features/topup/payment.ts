/// 下单响应（`/api/me/topup` 与 `/api/me/subscriptions/checkout` 同形）。
export interface TopupResp {
  order_no: string
  gateway: string
  pay_url: string | null
  params?: Record<string, string>
}

/// 支付地址只认 http(s)。它来自网关配置 / 支付服务商的应答，被误配或被改写成 `javascript:` 之类时，
/// 直接赋给 `location.href`（或当表单 action）会在本站源下执行脚本。
export function isHttpUrl(url: string): boolean {
  try {
    const { protocol } = new URL(url, window.location.href)
    return protocol === 'https:' || protocol === 'http:'
  } catch {
    return false
  }
}

/// epay 要求以表单 POST 带签名参数跳转；stripe 直接跳 checkout url。
/// 返回是否真的发起了跳转：没有地址、或地址不是 http(s) 时什么都不做，由调用方给出提示。
export function gotoPayment(resp: TopupResp): boolean {
  if (resp.pay_url === null || !isHttpUrl(resp.pay_url)) return false
  if (resp.params === undefined) {
    window.location.href = resp.pay_url
    return true
  }
  const form = document.createElement('form')
  form.method = 'POST'
  form.action = resp.pay_url
  for (const [name, value] of Object.entries(resp.params)) {
    const field = document.createElement('input')
    field.type = 'hidden'
    field.name = name
    field.value = value
    form.append(field)
  }
  document.body.append(form)
  form.submit()
  return true
}
