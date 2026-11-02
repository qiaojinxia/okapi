export interface Overview {
  user: {
    id: number
    username: string
    role: number
    status: number
    balance_micro: number
    /// 个人计价系数（十进制字符串；1 = 站内标准价）。
    price_multiplier: string
  }
  groups: { code: string; priority: number }[]
  keys: { id: number; name: string; key_prefix: string; status: number; used_micro: number }[]
}


