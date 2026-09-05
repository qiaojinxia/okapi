import { Link, useNavigate, useRouterState } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import {
  ChevronRight,
  Languages,
  LogOut,
  Menu as MenuIcon,
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Sun,
  X,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { BrandLockup } from '@/components/brand'
import { NoticeBanner } from '@/components/notice-banner'
import { NavLink, SidebarNav } from '@/components/sidebar-nav'
import type { NavGroup, NavItem } from '@/components/sidebar-nav'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Menu, MenuItem, MenuLabel } from '@/components/ui/menu'
import { Tooltip } from '@/components/ui/tooltip'
import { useMe, usePermission } from '@/hooks/use-auth'
import { useMediaQuery } from '@/hooks/use-media-query'
import { useModalFocus } from '@/hooks/use-modal-focus'
import { apiFetch, clearKey } from '@/lib/api'
import { switchLanguage } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { useTheme } from '@/lib/theme'
import { cn } from '@/lib/utils'
import { roleLabel } from '@/features/users/types'

export type { NavItem, NavGroup } from '@/components/sidebar-nav'

const SIDEBAR_STORAGE = 'okapi.sidebar'

function loadCollapsed(): boolean {
  return localStorage.getItem(SIDEBAR_STORAGE) === 'rail'
}

/// 三区布局：左侧分组导航 + 顶部栏（当前页标题 / 身份 / 工具）+ 内容区。
///
/// 侧栏在窄屏折叠为抽屉（`md` 断点）——中转后台常在手机上临时处置渠道故障，
/// 折叠比横向滚动可用得多；抽屉展开时铺一层遮罩，点击即关。
/// 桌面端可收成图标栏（记忆到 localStorage）：宽表格的页面（日志/渠道）多出的
/// 190px 正好少一次横向滚动。
export function Shell({ nav: rawNav, workspace, children }: {
  nav: NavGroup[]
  workspace?: NavItem
  children: React.ReactNode
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [collapsedPref, setCollapsedPref] = useState(loadCollapsed)
  const desktop = useMediaQuery('(min-width: 768px)')
  // 图标栏只是桌面形态；移动端同一状态渲染成全宽抽屉
  const rail = collapsedPref && desktop
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const can = usePermission()
  const panel = useRef<HTMLElement>(null)
  const mobileOpen = open && !desktop
  useModalFocus(mobileOpen, panel)

  useEffect(() => {
    if (!mobileOpen) return
    const previous = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    return () => { document.body.style.overflow = previous }
  }, [mobileOpen])

  const toggleCollapsed = () => {
    setCollapsedPref((v) => {
      localStorage.setItem(SIDEBAR_STORAGE, v ? 'full' : 'rail')
      return !v
    })
  }

  // 换页即关移动端抽屉
  useEffect(() => {
    setOpen(false)
  }, [pathname, desktop])

  // 按权限裁剪导航；整组被裁空时连小标题一起去掉，避免留下空标题
  const nav = rawNav
    .map((g) => ({
      ...g,
      items: g.items.filter((i) => i.permission === undefined || can(i.permission)),
    }))
    .filter((g) => g.items.length > 0)

  // 当前页标题：取最长匹配的导航项，避免 /admin 抢走 /admin/channels
  const current = nav
    .flatMap((g) => g.items)
    .filter((i) => pathname === i.to || pathname.startsWith(`${i.to}/`))
    .sort((a, b) => b.to.length - a.to.length)[0]

  return (
    <div className="flex min-h-screen bg-background">
      <a
        href="#main-content"
        className="sr-only focus:fixed focus:top-2 focus:left-2 focus:z-50 focus:w-auto focus:rounded-md focus:bg-primary focus:p-3 focus:text-primary-foreground focus:not-sr-only"
        inert={mobileOpen}
      >
        {t('common:skipToContent')}
      </a>
      {mobileOpen && (
        <div
          aria-hidden
          className="fixed inset-0 z-30 bg-black/40 backdrop-blur-[2px] animate-fade-in md:hidden"
          onClick={() => setOpen(false)}
        />
      )}

      <aside
        id="app-navigation"
        ref={panel}
        role={mobileOpen ? 'dialog' : undefined}
        aria-modal={mobileOpen || undefined}
        aria-label={t('common:navigation')}
        aria-hidden={!desktop && !open || undefined}
        inert={!desktop && !open}
        onKeyDown={(e) => {
          if (e.key === 'Escape' && mobileOpen) {
            e.preventDefault()
            e.stopPropagation()
            setOpen(false)
          }
        }}
        className={cn(
          'fixed inset-y-0 left-0 z-30 flex shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground',
          'transition-[transform,width] duration-200 ease-out md:sticky md:top-0 md:h-screen md:translate-x-0',
          rail ? 'w-[68px]' : 'w-72 max-w-[calc(100vw-3rem)] md:w-64',
          open ? 'translate-x-0 shadow-drawer' : '-translate-x-full',
        )}
      >
        <div
          className={cn(
            'flex h-14 shrink-0 items-center border-b border-sidebar-border',
            rail ? 'justify-center' : 'justify-between pr-2 pl-4',
          )}
        >
          <Link
            to={nav[0]?.items[0]?.to ?? '/'}
            onClick={() => setOpen(false)}
            className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
          >
            <BrandLockup compact={rail} />
          </Link>
          <Button
            variant="ghost"
            size="icon"
            className="h-11 w-11 md:hidden"
            aria-label={t('common:closeNav')}
            onClick={() => setOpen(false)}
          >
            <X className="h-4 w-4" />
          </Button>
        </div>

        <SidebarNav
          pathname={pathname}
          nav={nav}
          rail={rail}
          onExpand={toggleCollapsed}
          onNavigate={() => setOpen(false)}
        />

        {workspace && (workspace.permission === undefined || can(workspace.permission)) && (
          <div className={cn('shrink-0 border-t border-sidebar-border py-2', rail ? 'px-2.5' : 'px-3')}>
            {!rail && <p className="px-3 pb-1 text-[11px] text-muted-foreground">{t('common:switchWorkspace')}</p>}
            <NavLink item={workspace} rail={rail} onClick={() => setOpen(false)} />
          </div>
        )}
        <SidebarFooter rail={rail} onNavigate={() => setOpen(false)} />
      </aside>

      <div className="flex min-w-0 flex-1 flex-col" inert={mobileOpen}>
        <TopBar
          title={pathname === '/portal/profile' ? t('profile:title') : current?.label ?? ''}
          navOpen={mobileOpen}
          onOpenNav={() => setOpen(true)}
          collapsed={rail}
          onToggleNav={toggleCollapsed}
        />
        <main id="main-content" tabIndex={-1} className="flex-1 scroll-mt-16 px-4 py-5 outline-none sm:px-6 lg:px-8">
          <div className="mx-auto flex w-full max-w-[1600px] flex-col gap-4">
            {/* 站点公告置于所有页面内容之上：换页不丢，关掉即记住 */}
            <NoticeBanner />
            <div key={pathname} className="animate-fade-up">
              {children}
            </div>
          </div>
        </main>
      </div>
    </div>
  )
}

/// 侧栏最底部的身份卡即个人中心入口；折叠与移动端保留相同可访问名称。
function SidebarFooter({ rail, onNavigate }: { rail: boolean; onNavigate: () => void }) {
  const { t } = useTranslation()
  const me = useMe()
  if (!me.data) return null
  const role = roleLabel(me.data.role, t)
  const initial = role.slice(0, 1).toUpperCase()
  return (
    <div className="shrink-0 border-t border-sidebar-border p-2">
      <Tooltip content={rail ? t('profile:title') : ''} className="flex w-full">
        <Link
          to="/portal/profile"
          aria-label={t('profile:title')}
          onClick={onNavigate}
          activeProps={{ className: 'bg-primary/10 text-primary', 'aria-current': 'page' }}
          className={cn('flex min-h-12 w-full items-center gap-3 rounded-lg outline-none transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-primary/40', rail ? 'justify-center' : 'px-2 py-2')}
        >
          <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-primary/12 text-xs font-semibold text-primary">
            {initial}
          </span>
          {!rail && (
            <div className="flex min-w-0 flex-col">
              <span className="truncate text-sm font-medium">{t('profile:title')}</span>
              <span className="truncate text-xs text-muted-foreground">
                {t('common:userId', { id: me.data.user_id })} · {me.data.group}
              </span>
            </div>
          )}
          {!rail && <ChevronRight className="ml-auto h-4 w-4 shrink-0 text-muted-foreground" />}
        </Link>
      </Tooltip>
    </div>
  )
}

/// 顶部栏：左侧当前页标题（窄屏兼作抽屉入口），右侧身份与全局开关。
///
/// 身份区展示余额与分组：中转后台最常被问的两个问题就是"我还有多少钱"和
/// "我在哪个价格组"，放在常驻位置省掉一次跳转。
/// 侧栏收放也在这里：窄屏是抽屉菜单，桌面是收起/展开切换——同一位置一个按钮
/// 只随断点换职责，比塞在侧栏底部更符合"控制不在它所控制的东西里面"的直觉。
function TopBar({ title, navOpen, onOpenNav, collapsed, onToggleNav }: {
  title: string
  navOpen: boolean
  onOpenNav: () => void
  collapsed: boolean
  onToggleNav: () => void
}) {
  const { t, i18n } = useTranslation()
  const navigate = useNavigate()
  const me = useMe()
  const theme = useTheme()

  // 邮箱密码登录会在服务端建 Redis session（Team / TOTP 等页面靠它鉴权）。
  // 只清本地 key 会留下有效 session，共享设备上下一个人仍能操作那些页面——
  // 故先请服务端清 session，再清本地。服务端清理失败不阻塞本地登出。
  const logout = () => {
    void apiFetch('/auth/logout', { method: 'POST', body: {} })
      .catch(() => undefined)
      .finally(() => {
        clearKey()
        void navigate({ to: '/' })
      })
  }

  const ThemeIcon =
    theme.preference === 'system' ? Monitor : theme.resolved === 'dark' ? Moon : Sun
  const check = (on: boolean) => (on ? '✓' : undefined)

  return (
    <header className="sticky top-0 z-20 flex h-14 shrink-0 items-center gap-2 border-b border-border bg-background/85 px-4 backdrop-blur-md sm:px-6 lg:px-8">
      <Button
        variant="ghost"
        size="icon"
        className="-ml-2 h-11 w-11 md:hidden"
        aria-label={t('common:openNav')}
        aria-controls="app-navigation"
        aria-expanded={navOpen}
        onClick={onOpenNav}
      >
        <MenuIcon className="h-4.5 w-4.5" />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className="-ml-2 hidden h-9 w-9 md:inline-flex"
        aria-label={collapsed ? t('common:expandNav') : t('common:collapseNav')}
        aria-controls="app-navigation"
        onClick={onToggleNav}
      >
        {collapsed ? <PanelLeftOpen className="h-4 w-4" /> : <PanelLeftClose className="h-4 w-4" />}
      </Button>
      <h1 className="min-w-0 flex-1 truncate text-sm font-semibold">{title}</h1>

      {me.data && (
        <div className="hidden items-center gap-1.5 sm:flex">
          <Badge variant="outline" className="h-7 bg-card px-2.5 font-medium tabular-nums">
            {t('common:balance')} {formatMoney(me.data.balance_micro, i18n.language)}
          </Badge>
          <Badge variant="muted" className="h-7 px-2.5">
            {me.data.group}
          </Badge>
        </div>
      )}

      <span className="mx-1 hidden h-5 w-px bg-border sm:block" aria-hidden />

      <Menu
        align="end"
        className="min-w-44"
        trigger={
          <Button variant="ghost" size="icon" className="h-11 w-11 md:h-9 md:w-9" aria-label={t('common:language')}>
            <Languages className="h-4 w-4" />
          </Button>
        }
      >
        <MenuLabel>{t('common:language')}</MenuLabel>
        <MenuItem onSelect={() => switchLanguage('zh-CN')} trailing={check(i18n.language === 'zh-CN')}>
          {t('common:langZh')}
        </MenuItem>
        <MenuItem onSelect={() => switchLanguage('en')} trailing={check(i18n.language === 'en')}>
          {t('common:langEn')}
        </MenuItem>
      </Menu>

      <Menu
        align="end"
        className="min-w-44"
        trigger={
          <Button variant="ghost" size="icon" className="h-11 w-11 md:h-9 md:w-9" aria-label={t('common:theme')}>
            <ThemeIcon className="h-4 w-4" />
          </Button>
        }
      >
        <MenuLabel>{t('common:theme')}</MenuLabel>
        <MenuItem icon={Sun} onSelect={() => theme.setPreference('light')} trailing={check(theme.preference === 'light')}>
          {t('common:themeLight')}
        </MenuItem>
        <MenuItem icon={Moon} onSelect={() => theme.setPreference('dark')} trailing={check(theme.preference === 'dark')}>
          {t('common:themeDark')}
        </MenuItem>
        <MenuItem icon={Monitor} onSelect={() => theme.setPreference('system')} trailing={check(theme.preference === 'system')}>
          {t('common:themeSystem')}
        </MenuItem>
      </Menu>

      <Tooltip content={t('common:logout')} side="bottom">
        <Button variant="ghost" size="icon" className="h-11 w-11 md:h-9 md:w-9" onClick={logout} aria-label={t('common:logout')}>
          <LogOut className="h-4 w-4" />
        </Button>
      </Tooltip>
    </header>
  )
}
