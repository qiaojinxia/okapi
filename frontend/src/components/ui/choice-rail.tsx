import { useLayoutEffect, useRef, useState } from 'react'
import type { ReactNode, RefObject } from 'react'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

// 滚动按钮位于选项组之外，浏览被折叠的选项不会改变当前选择。
export function ChoiceRail({ container, children, label, id, className }: {
  container: RefObject<HTMLDivElement | null>
  children: ReactNode
  label?: string
  id?: string
  className?: string
}) {
  const { t } = useTranslation()
  const root = useRef<HTMLDivElement>(null)
  const [scroll, setScroll] = useState({ overflow: false, back: false, forward: false })
  useLayoutEffect(() => {
    const node = container.current, outer = root.current
    if (!node || !outer) return
    const measure = () => {
      // 判断整组是否需要滚动时，使用未扣除按钮的宽度，避免变宽后按钮仍不消失。
      const overflow = node.scrollWidth + node.offsetWidth - node.clientWidth > outer.clientWidth + 1
      const next = { overflow, back: node.scrollLeft > 1, forward: node.scrollLeft + node.clientWidth < node.scrollWidth - 1 }
      setScroll((prev) => prev.overflow === next.overflow && prev.back === next.back && prev.forward === next.forward ? prev : next)
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    observer.observe(outer)
    for (const child of node.children) observer.observe(child)
    node.addEventListener('scroll', measure, { passive: true })
    return () => { observer.disconnect(); node.removeEventListener('scroll', measure) }
  }, [container, children])
  const move = (direction: number) => {
    const node = container.current
    if (node) node.scrollBy({ left: direction * Math.max(80, node.clientWidth * 0.8), behavior: 'instant' })
  }
  const name = label ?? t('common:options')
  return <div ref={root} id={id} className={cn('inline-flex min-w-0 max-w-full items-stretch gap-1', className)}>
    {scroll.overflow && <button type="button" aria-label={t('common:previousOptions', { name })} aria-controls={container.current?.id || undefined} disabled={!scroll.back} onClick={() => move(-1)} className="flex w-7 shrink-0 items-center justify-center rounded-md border border-border bg-card text-muted-foreground outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-primary/40 disabled:opacity-35">
      <ChevronLeft aria-hidden className="h-4 w-4" />
    </button>}
    {children}
    {scroll.overflow && <button type="button" aria-label={t('common:nextOptions', { name })} aria-controls={container.current?.id || undefined} disabled={!scroll.forward} onClick={() => move(1)} className="flex w-7 shrink-0 items-center justify-center rounded-md border border-border bg-card text-muted-foreground outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-primary/40 disabled:opacity-35">
      <ChevronRight aria-hidden className="h-4 w-4" />
    </button>}
  </div>
}

export function revealChoice(container: HTMLElement, item: HTMLElement) {
  const parent = container.getBoundingClientRect(), child = item.getBoundingClientRect()
  if (child.left < parent.left) container.scrollLeft += child.left - parent.left - 3
  else if (child.right > parent.right) container.scrollLeft += child.right - parent.right + 3
}
