import { useId } from 'react'
import { cn } from '@/lib/utils'

/// 迷你面积折线：只传逐日数值，尺寸由父容器给（拉伸铺满，线宽不随拉伸变形）。
/// 不足两个点或全为 0 时不画——空图只会让数字卡显得有内容缺失。
export function Sparkline({ values, color = 'var(--color-primary)', className }: { values: number[]; color?: string; className?: string }) {
  const id = useId().replaceAll(':', '')
  const max = Math.max(0, ...values)
  if (values.length < 2 || max <= 0) return null
  const x = (i: number) => (i / (values.length - 1)) * 100
  // viewBox 高 32：上下各留 4，最高点不贴边
  const y = (value: number) => 28 - (Math.max(0, value) / max) * 24
  const line = values.map((value, i) => `${x(i).toFixed(2)},${y(value).toFixed(2)}`).join(' ')
  return (
    <svg aria-hidden data-slot="sparkline" viewBox="0 0 100 32" preserveAspectRatio="none" className={cn('overflow-visible', className)}>
      <defs>
        <linearGradient id={id} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor={color} stopOpacity="0.28" />
          <stop offset="1" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      <polygon points={`0,32 ${line} 100,32`} fill={`url(#${id})`} />
      <polyline points={line} fill="none" stroke={color} strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
    </svg>
  )
}
