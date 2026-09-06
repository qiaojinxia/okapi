/// 下单响应（`/api/me/topup` 与 `/api/me/subscriptions/checkout` 同形）。
export interface TopupResp {
  order_no: string
  gateway: string
  pay_url: string | null
  params?: Record<string, string>
}

/// epay 要求以表单 POST 带签名参数跳转；stripe 直接跳 checkout url。
export function gotoPayment(resp: TopupResp): void {
  if (resp.pay_url === null) return
  if (resp.params === undefined) {
    window.location.href = resp.pay_url
    return
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
}
