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
  // 按用户关心的三件事分组：用得怎么样 / 钱和额度 / 账号与协作
  const nav: NavGroup[] = [
    {
      items: [
        { to: '/portal', label: t('portal:dashboard'), icon: LayoutDashboard },
        { to: '/portal/logs', label: t('logs:title'), icon: FileText },
      ],
    },
    {
      title: t('portal:navBilling'),
      items: [
        { to: '/portal/topup', label: t('portal:topupNav'), icon: Wallet },
        { to: '/portal/ledger', label: t('portal:ledgerNav'), icon: Receipt },
        { to: '/portal/aff', label: t('portal:affNav'), icon: Gift },
        { to: '/pricing', label: t('pricing:title'), icon: Tags },
      ],
    },
    {
      title: t('portal:navAccount'),
      items: [
        { to: '/portal/keys', label: t('portal:keys'), icon: KeyRound },
        { to: '/portal/teams', label: t('team:nav'), icon: Users },
        { to: '/portal/security', label: t('security:nav'), icon: ShieldCheck },
      ],
    },
    { items: [{ to: '/admin', label: t('common:admin'), icon: Sliders }] },
  ]
  return (
    <Shell nav={nav}>
      <Outlet />
    </Shell>
  )
}
