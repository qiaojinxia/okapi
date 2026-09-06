import { Outlet, createFileRoute, redirect, useRouterState } from '@tanstack/react-router'
import {
  FileText,
  Gift,
  KeyRound,
  LayoutDashboard,
  Package,
  Receipt,
  ShieldCheck,
  Sliders,
  Tags,
  Users,
  Wallet,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Shell } from '@/components/layout'
import type { NavGroup } from '@/components/layout'
import { GuideDrawer } from '@/features/portal-guide/GuideDrawer'
import { GuideContext } from '@/features/portal-guide/guide-state'
import type { GuideRequest } from '@/features/portal-guide/guide-state'
import { getKey } from '@/lib/api'
import { useMe } from '@/hooks/use-auth'

export const Route = createFileRoute('/portal')({
  beforeLoad: () => {
    if (getKey() === null) {
      throw redirect({ to: '/' })
    }
  },
  component: PortalLayout,
})

function PortalLayout() {
  const { t } = useTranslation()
  const me = useMe()
  // 引导抽屉挂在外壳层：顶栏 ?、总览卡、密钥页都能打开同一个实例；
  // 点了抽屉里的深链（管理密钥 / 用量日志）即关，不然抽屉会盖在目标页上
  const [guide, setGuide] = useState<GuideRequest | null>(null)
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  useEffect(() => { setGuide(null) }, [pathname])
  // 从接入到排查的常用入口相邻；账单、账号各成一组。
  const nav: NavGroup[] = [
    { items: [{ to: '/portal', label: t('portal:dashboard'), icon: LayoutDashboard }] },
    {
      title: t('portal:navUsage'),
      items: [
        { to: '/portal/keys', label: t('portal:keys'), icon: KeyRound },
        { to: '/pricing', label: t('pricing:title'), icon: Tags },
        { to: '/portal/logs', label: t('logs:title'), icon: FileText },
      ],
    },
    {
      title: t('portal:navBilling'),
      items: [
        { to: '/portal/topup', label: t('portal:topupNav'), icon: Wallet },
        { to: '/portal/plans', label: t('portal:plansNav'), icon: Package },
        { to: '/portal/ledger', label: t('portal:ledgerNav'), icon: Receipt },
        { to: '/portal/aff', label: t('portal:affNav'), icon: Gift },
      ],
    },
    {
      title: t('portal:navAccount'),
      items: [
        { to: '/portal/teams', label: t('team:nav'), icon: Users },
        { to: '/portal/security', label: t('security:nav'), icon: ShieldCheck },
      ],
    },
  ]
  return (
    <GuideContext.Provider value={{ open: (req) => setGuide(req ?? {}) }}>
      <Shell
        nav={nav}
        workspace={me.data?.permissions.length ? { to: '/admin', label: t('common:admin'), icon: Sliders } : undefined}
        onHelp={() => setGuide({})}
      >
        <Outlet />
      </Shell>
      <GuideDrawer open={guide !== null} apiKey={guide?.apiKey} onClose={() => setGuide(null)} />
    </GuideContext.Provider>
  )
}
