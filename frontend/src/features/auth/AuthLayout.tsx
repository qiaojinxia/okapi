import { Link } from '@tanstack/react-router'
import { CheckCircle2, Moon, Sun } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { BrandLockup, BrandMark } from '@/components/brand'
import { NoticeBanner } from '@/components/notice-banner'
import { Button } from '@/components/ui/button'
import { useTheme } from '@/lib/theme'

/// 认证页外壳：大屏左侧品牌面（渐变 + 网格 + 三条卖点），右侧表单卡；
/// 窄屏收成单列，品牌只剩顶部一行。
///
/// 此前登录页是一张裸卡片漂在灰底中央，与"开源里最好看"的定位（DESIGN §9.1）
/// 不相称；品牌面不是装饰——它回答了未登录访客的第一个问题"这是什么站"。
export function AuthLayout({
  title,
  subtitle,
  children,
  footer,
}: {
  title: string
  subtitle?: string
  children: React.ReactNode
  footer?: React.ReactNode
}) {
  const { t } = useTranslation()
  const theme = useTheme()
  const points = [t('auth:heroPoint1'), t('auth:heroPoint2'), t('auth:heroPoint3')]

  return (
    <div className="grid min-h-screen bg-background lg:grid-cols-[minmax(0,5fr)_minmax(0,6fr)]">
      <aside className="relative hidden overflow-hidden border-r border-border bg-sidebar lg:flex lg:flex-col lg:justify-between lg:p-12">
        <div className="pointer-events-none absolute inset-0 bg-grid opacity-60 [mask-image:radial-gradient(ellipse_at_top_left,black,transparent_75%)]" />
        <div className="pointer-events-none absolute -top-32 -left-32 h-96 w-96 rounded-full bg-primary/20 blur-3xl" />
        <div className="pointer-events-none absolute -right-24 bottom-0 h-80 w-80 rounded-full bg-chart-4/15 blur-3xl" />

        <BrandLockup size="lg" className="relative" />

        <div className="relative flex max-w-md flex-col gap-6">
          <h2 className="text-3xl font-semibold tracking-tight text-balance">
            {t('auth:heroTitle')}
          </h2>
          <p className="text-base leading-7 text-muted-foreground">{t('auth:heroSubtitle')}</p>
          <ul className="flex flex-col gap-3">
            {points.map((p) => (
              <li key={p} className="flex items-start gap-2.5 text-sm">
                <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-primary" />
                <span>{p}</span>
              </li>
            ))}
          </ul>
        </div>

        <p className="relative text-xs text-muted-foreground">
          <Link to="/pricing" className="underline decoration-dotted underline-offset-4 hover:text-foreground">
            {t('pricing:title')}
          </Link>
        </p>
      </aside>

      <div className="flex flex-col">
        <header className="flex h-14 items-center justify-between px-5 lg:justify-end">
          <span className="lg:hidden">
            <BrandLockup />
          </span>
          <Button
            variant="ghost"
            size="icon"
            className="h-9 w-9"
            aria-label={t('common:theme')}
            onClick={theme.toggle}
          >
            {theme.resolved === 'dark' ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
          </Button>
        </header>

        <main className="flex flex-1 flex-col items-center justify-center gap-4 px-4 pb-12">
          {/* 公告在登录前就要看得到：停服/维护通知对还没登录的人同样成立 */}
          <NoticeBanner className="w-full max-w-md" />
          <div className="w-full max-w-md animate-fade-up">
            <div className="mb-6 flex flex-col gap-1.5">
              <BrandMark className="mb-3 hidden h-10 w-10 lg:block" />
              <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
              {subtitle !== undefined && (
                <p className="text-sm text-muted-foreground">{subtitle}</p>
              )}
            </div>
            {children}
          </div>
          {/* 大屏时同样的链接已在品牌面底部，只在单列时显示，避免一页两个同名链接 */}
          {footer !== undefined && (
            <div className="w-full max-w-md text-center text-xs text-muted-foreground lg:hidden">
              {footer}
            </div>
          )}
        </main>
      </div>
    </div>
  )
}
