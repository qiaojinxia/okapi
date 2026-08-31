import { Link, useNavigate, useRouterState } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Languages, LogOut, Menu, Moon, Sun, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useState } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useMe, usePermission } from '@/hooks/use-auth'
import { apiFetch, clearKey } from '@/lib/api'
import { switchLanguage } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { cn } from '@/lib/utils'

export interface NavItem {
  to: string
  label: string
  icon?: LucideIcon
  /// 需要的权限点；缺省 = 人人可见。无权者该入口直接不出现——
  /// 让用户点进去吃 403 是把后端的拦截当成了交互设计。
  permission?: string
}

/// 导航分组：功能一多，平铺列表就找不着东西——按域分组是主流后台的通行做法。
/// `title` 省略表示不带小标题的顶层组（如"总览"）。
export interface NavGroup {
  title?: string
  items: NavItem[]
}

/// 三区布局：左侧分组导航 + 顶部栏（当前页标题 / 身份 / 工具）+ 内容区。
///
/// 侧栏在窄屏折叠为抽屉（`md` 断点）——中转后台常在手机上临时处置渠道故障，
/// 折叠比横向滚动可用得多；抽屉展开时铺一层遮罩，点击即关。
export function Shell({ nav: rawNav, children }: { nav: NavGroup[]; children: React.ReactNode }) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const can = usePermission()

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
    <div className="flex min-h-screen">
      {open && (
        <button
          type="button"
          aria-label={t('common:closeNav')}
          className="fixed inset-0 z-20 bg-black/40 md:hidden"
          onClick={() => setOpen(false)}
        />
      )}

      <aside
        className={cn(
          'fixed inset-y-0 left-0 z-30 flex w-56 shrink-0 flex-col border-r border-border bg-card',
          'transition-transform md:static md:translate-x-0',
          open ? 'translate-x-0' : '-translate-x-full',
        )}
      >
        <div className="flex h-14 items-center justify-between border-b border-border px-4">
          <span className="text-lg font-bold text-primary">{t('common:appName')}</span>
          <Button
            variant="ghost"
            size="icon"
            className="md:hidden"
            aria-label={t('common:closeNav')}
            onClick={() => setOpen(false)}
          >
            <X className="h-4 w-4" />
          </Button>
        </div>

        <nav className="flex flex-1 flex-col gap-4 overflow-y-auto p-3">
          {nav.map((group, gi) => (
            <div key={group.title ?? `g${gi}`} className="flex flex-col gap-1">
              {group.title !== undefined && (
                <span className="px-3 pb-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">
                  {group.title}
                </span>
              )}
              {group.items.map((item) => (
                <Link
                  key={item.to}
                  to={item.to}
                  activeOptions={{ exact: true }}
                  onClick={() => setOpen(false)}
                  className="flex items-center gap-2 rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
                  activeProps={{ className: 'bg-muted text-foreground font-medium' }}
                >
                  {item.icon && <item.icon className="h-4 w-4 shrink-0" />}
                  <span className="truncate">{item.label}</span>
                </Link>
              ))}
            </div>
          ))}
        </nav>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar title={current?.label ?? ''} onOpenNav={() => setOpen(true)} />
        <main className="flex-1 p-4 sm:p-6">{children}</main>
      </div>
    </div>
  )
}

/// 顶部栏：左侧当前页标题（窄屏兼作抽屉入口），右侧身份与全局开关。
///
/// 身份区展示余额与分组：中转后台最常被问的两个问题就是"我还有多少钱"和
/// "我在哪个价格组"，放在常驻位置省掉一次跳转。
function TopBar({ title, onOpenNav }: { title: string; onOpenNav: () => void }) {
  const { t, i18n } = useTranslation()
  const navigate = useNavigate()
  const me = useMe()
  const [dark, setDark] = useState(() => document.documentElement.classList.contains('dark'))

  const toggleTheme = () => {
    document.documentElement.classList.toggle('dark')
    setDark((v) => !v)
  }
  const toggleLang = () => {
    switchLanguage(i18n.language === 'zh-CN' ? 'en' : 'zh-CN')
  }
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

  return (
    <header className="flex h-14 shrink-0 items-center gap-3 border-b border-border bg-card px-4">
      <Button
        variant="ghost"
        size="icon"
        className="md:hidden"
        aria-label={t('common:openNav')}
        onClick={onOpenNav}
      >
        <Menu className="h-4 w-4" />
      </Button>
      <h1 className="min-w-0 flex-1 truncate text-sm font-medium">{title}</h1>

      {me.data && (
        <div className="hidden items-center gap-2 sm:flex">
          <Badge variant="muted">
            {t('common:balance')} {formatMoney(me.data.balance_micro, i18n.language)}
          </Badge>
          <Badge variant="muted">{me.data.group}</Badge>
        </div>
      )}
      <Button variant="ghost" size="icon" onClick={toggleLang} aria-label={t('common:language')}>
        <Languages className="h-4 w-4" />
      </Button>
      <Button variant="ghost" size="icon" onClick={toggleTheme} aria-label={t('common:theme')}>
        {dark ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
      </Button>
      <Button variant="ghost" size="icon" onClick={logout} aria-label={t('common:logout')}>
        <LogOut className="h-4 w-4" />
      </Button>
    </header>
  )
}
