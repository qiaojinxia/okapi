import { Outlet, createFileRoute, redirect } from '@tanstack/react-router'
import {
  BarChart3,
  Boxes,
  Coins,
  Gift,
  KeyRound,
  Layers,
  LayoutDashboard,
  Package,
  Percent,
  Server,
  Settings,
  ShieldCheck,
  User,
  Users,
  Wrench,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Shell } from '@/components/layout'
import type { NavGroup } from '@/components/layout'
import { getKey } from '@/lib/api'

export const Route = createFileRoute('/admin')({
  beforeLoad: () => {
    if (getKey() === null) {
      throw redirect({ to: '/' })
    }
  },
  component: AdminLayout,
})

function AdminLayout() {
  const { t } = useTranslation()
  // 分组顺序即运维动线：先看总览 → 接入供应商 → 配模型价 → 管用户 → 看数 → 调系统
  const nav: NavGroup[] = [
    { items: [{ to: '/admin', label: t('admin:overview'), icon: LayoutDashboard }] },
    {
      title: t('admin:navSupply'),
      items: [
        { to: '/admin/channels', label: t('admin:channels'), icon: Server, permission: 'channel.read' },
        { to: '/admin/pools', label: t('admin:poolsTitle'), icon: Layers, permission: 'channel.read' },
      ],
    },
    {
      title: t('admin:navPricing'),
      items: [
        { to: '/admin/pricing', label: t('admin:modelListTitle'), icon: Coins, permission: 'pricing.read' },
        { to: '/admin/groups', label: t('admin:groupsTitle'), icon: Boxes, permission: 'pricing.read' },
        { to: '/admin/rules', label: t('admin:rulesTitle'), icon: Percent, permission: 'pricing.read' },
      ],
    },
    {
      title: t('admin:navGrowth'),
      items: [
        { to: '/admin/codes', label: t('admin:redeemNav'), icon: Gift, permission: 'pricing.write' },
        { to: '/admin/plans', label: t('admin:planListTitle'), icon: Package, permission: 'pricing.write' },
      ],
    },
    {
      title: t('admin:navIdentity'),
      items: [
        { to: '/admin/users', label: t('admin:users'), icon: Users, permission: 'user.read' },
        { to: '/admin/roles', label: t('admin:rolesTitle'), icon: ShieldCheck, permission: 'role.manage' },
        { to: '/admin/keys', label: t('admin:keysNav'), icon: KeyRound, permission: 'user.read' },
      ],
    },
    {
      title: t('admin:navInsight'),
      items: [{ to: '/admin/stats', label: t('admin:statNav'), icon: BarChart3, permission: 'billing.read' }],
    },
    {
      title: t('admin:navSystem'),
      items: [
        { to: '/admin/settings', label: t('admin:settingsTitle'), icon: Settings, permission: 'settings.read' },
        { to: '/admin/ops', label: t('admin:opsTitle'), icon: Wrench, permission: 'settings.write' },
      ],
    },
    { items: [{ to: '/portal', label: t('common:portal'), icon: User }] },
  ]
  return (
    <Shell nav={nav}>
      <Outlet />
    </Shell>
  )
}
