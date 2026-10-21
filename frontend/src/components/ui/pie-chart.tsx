import { cn } from '@/lib/utils'

export interface PieSlice {
  key: string
  /// 绝对值（占比由所有扇区之和决定）。0 或负数不画。
  value: number
  color: string
}

/// 外半径：viewBox 为 100，留 4 个单位给悬停时的外推。
const OUTER = 46
/// 悬停扇区沿平分线外推的距离。
const NUDGE = 2.4

const point = (radius: number, angle: number) => `${(radius * Math.cos(angle)).toFixed(3)} ${(radius * Math.sin(angle)).toFixed(3)}`

/// 一块扇区的轮廓：外弧顺时针、内弧逆时针；占满整圈时退化成两个同心圆（even-odd 镂空），
/// 单段弧的起终点重合会画不出来。
function arc(start: number, end: number, inner: number): string {
  if (end - start >= Math.PI * 2 - 1e-4) {
    const ring = (radius: number, sweep: 0 | 1) => `M ${radius} 0 A ${radius} ${radius} 0 1 ${sweep} ${-radius} 0 A ${radius} ${radius} 0 1 ${sweep} ${radius} 0 Z`
    return `${ring(OUTER, 1)} ${ring(inner, 0)}`
  }
  const large = end - start > Math.PI ? 1 : 0
  return `M ${point(OUTER, start)} A ${OUTER} ${OUTER} 0 ${large} 1 ${point(OUTER, end)} L ${point(inner, end)} A ${inner} ${inner} 0 ${large} 0 ${point(inner, start)} Z`
}

/// 扇形图：SVG 扇区 + 中心可放文字（children）。
///
/// 从 12 点钟方向顺时针排；扇区之间留一道卡片色的细缝。`active` 与图例联动：该扇区外推、其余变淡，
/// 悬停扇区时通过 `onActiveChange` 通知图例。尺寸由父级给（`className`，正方形）。
/// 数据全为 0 时只画一圈灰底，不伪造占比。
export function PieChart({ slices, active = null, onActiveChange, hole = 0.56, label, decorative = false, slot = 'pie-chart', className, children }: {
  slices: PieSlice[]
  active?: string | null
  onActiveChange?: (key: string | null) => void
  /// 中心空洞占外半径的比例。
  hole?: number
  label?: string
  /// 语义由外层负责（外层已有 role / 提示）时不再自带 `role="img"`。
  decorative?: boolean
  /// 根元素的 `data-slot`：同一个页面里有几张扇形图时，让调用方能各自定位。
  slot?: string
  className?: string
  children?: React.ReactNode
}) {
  const drawn = slices.filter((slice) => slice.value > 0)
  const total = drawn.reduce((sum, slice) => sum + slice.value, 0)
  const inner = OUTER * hole
  let cursor = -Math.PI / 2
  const sectors = drawn.map((slice) => {
    const span = total > 0 ? (slice.value / total) * Math.PI * 2 : 0
    const start = cursor
    cursor += span
    return { ...slice, start, end: cursor, share: slice.value / total }
  })
  return (
    <div data-slot={slot} role={decorative ? undefined : 'img'} aria-label={decorative ? undefined : label} className={cn('@container relative aspect-square', className)}>
      <svg aria-hidden viewBox="-50 -50 100 100" className="size-full">
        <circle r={(OUTER + inner) / 2} fill="none" strokeWidth={OUTER - inner} className="stroke-muted" />
        {sectors.map((sector) => {
          const middle = (sector.start + sector.end) / 2
          const isActive = active === sector.key
          return (
            <path key={sector.key} data-slot="pie-slice" data-key={sector.key} data-share-bp={Math.round(sector.share * 10_000)}
              d={arc(sector.start, sector.end, inner)} fillRule="evenodd" fill={sector.color}
              stroke="var(--color-card)" strokeWidth={sectors.length > 1 ? 1.2 : 0} strokeLinejoin="round"
              className="transition-[transform,opacity] duration-150 motion-reduce:transition-none"
              style={{ opacity: active != null && !isActive ? 0.4 : 1, transform: isActive ? `translate(${(Math.cos(middle) * NUDGE).toFixed(2)}px, ${(Math.sin(middle) * NUDGE).toFixed(2)}px)` : undefined }}
              onMouseEnter={onActiveChange ? () => onActiveChange(sector.key) : undefined}
              onMouseLeave={onActiveChange ? () => onActiveChange(null) : undefined} />
          )
        })}
      </svg>
      {children && (
        <div aria-hidden className="pointer-events-none absolute inset-0 flex items-center justify-center">
          {/* 文字限在空洞内（留一点边距），避免压到扇区 */}
          <div className="flex min-w-0 flex-col items-center justify-center text-center" style={{ width: `${hole * 92 - 3}%` }}>{children}</div>
        </div>
      )}
    </div>
  )
}
