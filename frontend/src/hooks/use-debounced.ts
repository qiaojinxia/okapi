import { useEffect, useState } from 'react'

/// 值稳定 `ms` 毫秒之后才跟上：输入框每敲一个字符就发一次请求不划算。
export function useDebounced<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value)
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), ms)
    return () => clearTimeout(timer)
  }, [value, ms])
  return settled
}
