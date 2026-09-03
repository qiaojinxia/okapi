import { useEffect, useState } from 'react'

/// 订阅媒体查询（如 `(min-width: 768px)`）。侧栏"折叠成图标栏"只在桌面端成立，
/// 移动端同一份状态要渲染成全宽抽屉，故需要在 JS 里知道当前断点。
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches)
  useEffect(() => {
    const mq = window.matchMedia(query)
    const onChange = () => setMatches(mq.matches)
    onChange()
    mq.addEventListener('change', onChange)
    return () => mq.removeEventListener('change', onChange)
  }, [query])
  return matches
}
