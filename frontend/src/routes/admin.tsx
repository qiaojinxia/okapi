import { Outlet, createFileRoute, redirect } from '@tanstack/react-router'
import {
  BarChart3,
  Coins,
  Gift,
  KeyRound,
  LayoutDashboard,
  Server,
  Settings,
  User,
  Users,
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
      items: [{ to: '/admin/channels', label: t('admin:channels'), icon: Server }],
    },
    {
      title: t('admin:navPricing'),
      items: [
        { to: '/admin/pricing', label: t('admin:pricing'), icon: Coins },
        { to: '/admin/codes', label: t('admin:redeemNav'), icon: Gift },
      ],
    },
    {
      title: t('admin:navIdentity'),
      items: [
        { to: '/admin/users', label: t('admin:users'), icon: Users },
        { to: '/admin/keys', label: t('admin:keysNav'), icon: KeyRound },
      ],
    },
    {
      title: t('admin:navInsight'),
      items: [{ to: '/admin/stats', label: t('admin:statNav'), icon: BarChart3 }],
    },
    {
      title: t('admin:navSystem'),
      items: [{ to: '/admin/ops', label: t('admin:ops'), icon: Settings }],
    },
    { items: [{ to: '/portal', label: t('common:portal'), icon: User }] },
  ]
  return (
    <Shell nav={nav}>
      <Outlet />
    </Shell>
  )
}
