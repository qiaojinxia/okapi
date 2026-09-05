import { Outlet, createFileRoute, redirect } from '@tanstack/react-router'
import {
  FileText,
  Gift,
  KeyRound,
  LayoutDashboard,
  Receipt,
  ShieldCheck,
  Sliders,
  Tags,
  Users,
  Wallet,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Shell } from '@/components/layout'
import type { NavGroup } from '@/components/layout'
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
    <Shell
      nav={nav}
      workspace={me.data?.permissions.length ? { to: '/admin', label: t('common:admin'), icon: Sliders } : undefined}
    >
      <Outlet />
    </Shell>
  )
}
