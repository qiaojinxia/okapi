import { Link, useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Languages, LogOut, Moon, Sun } from 'lucide-react'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { clearKey } from '@/lib/api'
import { switchLanguage } from '@/lib/i18n'
import { cn } from '@/lib/utils'

interface NavItem {
  to: string
  label: string
}

export function Shell({ nav, children }: { nav: NavItem[]; children: React.ReactNode }) {
  const { t, i18n } = useTranslation()
  const navigate = useNavigate()
  const [dark, setDark] = useState(() => document.documentElement.classList.contains('dark'))

  const toggleTheme = () => {
    document.documentElement.classList.toggle('dark')
    setDark((v) => !v)
  }
  const toggleLang = () => {
    switchLanguage(i18n.language === 'zh-CN' ? 'en' : 'zh-CN')
  }
  const logout = () => {
    clearKey()
    void navigate({ to: '/' })
  }

  return (
    <div className="flex min-h-screen">
      <aside className="flex w-52 shrink-0 flex-col border-r border-border bg-card p-4">
        <div className="mb-6 text-lg font-bold text-primary">{t('common:appName')}</div>
        <nav className="flex flex-col gap-1">
          {nav.map((item) => (
            <Link
              key={item.to}
              to={item.to}
              activeOptions={{ exact: true }}
              className={cn(
                'rounded-md px-3 py-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground',
              )}
              activeProps={{ className: 'bg-muted text-foreground font-medium' }}
            >
              {item.label}
            </Link>
          ))}
        </nav>
        <div className="mt-auto flex items-center gap-1">
          <Button variant="ghost" size="icon" onClick={toggleLang} aria-label={t('common:language')}>
            <Languages className="h-4 w-4" />
          </Button>
          <Button variant="ghost" size="icon" onClick={toggleTheme} aria-label={t('common:theme')}>
            {dark ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
          </Button>
          <Button variant="ghost" size="icon" onClick={logout} aria-label={t('common:logout')}>
            <LogOut className="h-4 w-4" />
          </Button>
        </div>
      </aside>
      <main className="flex-1 p-6">{children}</main>
    </div>
  )
}
