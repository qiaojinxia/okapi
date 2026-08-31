import { Outlet, createFileRoute, redirect } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Shell } from '@/components/layout'
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
  return (
    <Shell
      nav={[
        { to: '/admin', label: t('admin:overview') },
        { to: '/admin/channels', label: t('admin:channels') },
        { to: '/admin/pricing', label: t('admin:pricing') },
        { to: '/admin/users', label: t('admin:users') },
        { to: '/admin/keys', label: t('admin:keysNav') },
        { to: '/admin/codes', label: t('admin:redeemNav') },
        { to: '/admin/stats', label: t('admin:statNav') },
        { to: '/admin/ops', label: t('admin:ops') },
        { to: '/portal', label: t('common:portal') },
      ]}
    >
      <Outlet />
    </Shell>
  )
}
