import { Outlet, createFileRoute, redirect } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Shell } from '@/components/layout'
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
  return (
    <Shell
      nav={[
        { to: '/portal', label: t('portal:dashboard') },
        { to: '/portal/logs', label: t('logs:title') },
        { to: '/portal/keys', label: t('portal:keys') },
        { to: '/portal/topup', label: t('portal:topupNav') },
        { to: '/portal/teams', label: t('team:nav') },
        { to: '/portal/aff', label: t('portal:affNav') },
        { to: '/pricing', label: t('pricing:title') },
        { to: '/admin', label: t('common:admin') },
      ]}
    >
      <Outlet />
    </Shell>
  )
}
