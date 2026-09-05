import { Link, useNavigate } from '@tanstack/react-router'
import { Search } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { SearchInput } from '@/components/ui/search-input'
import { Tooltip } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

export interface NavItem {
  to: string
  label: string
  icon?: LucideIcon
  permission?: string
}

export interface NavGroup {
  title?: string
  items: NavItem[]
}

// nav 已在 Shell 按权限裁剪；检索名称、分组和路径不会暴露无权访问的入口。
export function SidebarNav({ nav, rail, pathname, onExpand, onNavigate }: {
  nav: NavGroup[]
  rail: boolean
  pathname: string
  onExpand: () => void
  onNavigate: () => void
}) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const [query, setQuery] = useState('')
  const root = useRef<HTMLDivElement>(null)
  const focusAfterExpand = useRef(false)
  const hintId = useId()
  const currentTo = nav.flatMap((group) => group.items).find((item) => item.to === pathname)?.to
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean)
  const filtered = nav.map((group) => ({
    ...group,
    items: group.items.filter((item) => {
      const text = `${group.title ?? ''} ${item.label} ${item.to}`.toLocaleLowerCase()
      return terms.every((term) => text.includes(term))
    }),
  })).filter((group) => group.items.length > 0)
  const finish = () => {
    setQuery('')
    onNavigate()
  }

  useEffect(() => {
    if (rail) setQuery('')
    if (!rail && focusAfterExpand.current) {
      focusAfterExpand.current = false
      root.current?.querySelector<HTMLInputElement>('input')?.focus({ preventScroll: true })
    }
  }, [rail])

  useEffect(() => { setQuery('') }, [pathname])

  useEffect(() => {
    if (query.trim() !== '') return
    const list = root.current?.querySelector('nav')
    const current = list?.querySelector('[aria-current=page]')
    if (!list || !current) return
    // 只移动导航自己的滚动区；深链和换页后当前入口保持可见。
    const bounds = list.getBoundingClientRect()
    const rect = current.getBoundingClientRect()
    if (rect.top < bounds.top) list.scrollTop += rect.top - bounds.top
    else if (rect.bottom > bounds.bottom) list.scrollTop += rect.bottom - bounds.bottom
  }, [currentTo, rail, query])

  return (
    <div ref={root} className="flex min-h-0 flex-1 flex-col">
      <div className={cn('shrink-0 pt-3', rail ? 'px-2.5' : 'px-3')}>
        {rail ? (
          <Tooltip content={t('common:searchNav')} className="w-full">
            <Button
              variant="ghost"
              size="icon"
              className="w-full"
              aria-label={t('common:searchNav')}
              onClick={() => {
                focusAfterExpand.current = true
                onExpand()
              }}
            >
              <Search aria-hidden className="h-4 w-4" />
            </Button>
          </Tooltip>
        ) : (
          <SearchInput
            value={query}
            onChange={setQuery}
            aria-label={t('common:searchNav')}
            placeholder={t('common:searchNavPlaceholder')}
            aria-describedby={terms.length > 0 ? hintId : undefined}
            inputClassName="h-11 md:h-9"
            onSubmit={() => {
              const first = filtered[0]?.items[0]
              if (terms.length === 0 || !first) return
              void navigate({ to: first.to })
              finish()
            }}
            onKeyDown={(e) => {
              if (e.key === 'Escape' && query !== '') {
                e.preventDefault()
                e.stopPropagation()
                setQuery('')
              }
            }}
          />
        )}
        {terms.length > 0 && (
          <p id={hintId} className="pt-2 text-[11px] text-muted-foreground">{t('common:searchNavHint')}</p>
        )}
      </div>
      <nav
        aria-label={t('common:navigation')}
        className={cn('flex min-h-0 flex-1 flex-col gap-4 overflow-x-hidden overflow-y-auto overscroll-contain py-3', rail ? 'px-2.5' : 'px-3')}
      >
        {filtered.map((group, gi) => (
          <div key={group.title ?? `g${gi}`} className="flex flex-col gap-0.5">
            {group.title !== undefined && (rail ? (
              <span className="mx-2 mb-1 h-px bg-sidebar-border" aria-hidden />
            ) : (
              <span className="mb-1 px-3 text-[11px] font-semibold tracking-wider text-muted-foreground/80 uppercase">{group.title}</span>
            ))}
            {group.items.map((item) => <NavLink key={item.to} item={item} rail={rail} onClick={finish} />)}
          </div>
        ))}
        {filtered.length === 0 && (
          <p role="status" className="px-3 py-6 text-sm leading-6 text-muted-foreground">{t('common:searchNavEmpty')}</p>
        )}
      </nav>
    </div>
  )
}

export function NavLink({ item, rail, onClick }: { item: NavItem; rail: boolean; onClick?: () => void }) {
  return (
    <Tooltip content={rail ? item.label : ''} className="w-full">
      <Link
        to={item.to}
        aria-label={item.label}
        onClick={onClick}
        activeOptions={{ exact: true }}
        className={cn(
          'relative flex min-h-11 w-full items-center gap-2.5 rounded-md py-2 text-sm text-sidebar-foreground/85 transition-colors outline-none md:min-h-9',
          'hover:bg-sidebar-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/60',
          rail ? 'justify-center px-0' : 'px-3',
        )}
        activeProps={{
          'aria-current': 'page',
          className: 'bg-sidebar-accent font-medium text-foreground before:absolute before:top-2 before:bottom-2 before:left-0 before:w-0.5 before:rounded-full before:bg-primary',
        }}
      >
        {item.icon ? <item.icon aria-hidden className="h-4 w-4 shrink-0 opacity-90" /> : <span className="h-4 w-4 shrink-0" />}
        {!rail && <span className="truncate">{item.label}</span>}
      </Link>
    </Tooltip>
  )
}
