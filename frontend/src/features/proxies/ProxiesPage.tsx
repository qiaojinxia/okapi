import { Link } from '@tanstack/react-router'
import { Settings } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { buttonVariants } from '@/components/ui/button'
import { PageHeader } from '@/components/ui/page'
import { Tabs } from '@/components/ui/tabs'
import { usePermission } from '@/hooks/use-auth'
import { TrafficPanel } from './TrafficPanel'
import { GroupList } from './GroupList'
import { ProxyList } from './ProxyList'

type Tab = 'proxies' | 'groups' | 'traffic'

/// 出口代理页（IMPLEMENTATION §11.41）。
///
/// 代理是一等资源：渠道按「继承全局默认 / 直连 / 单个代理 / 代理组」绑定出口（在渠道抽屉里选），
/// 不挂在渠道池上——一个渠道可以同时在多个池里，按池绑出口会让同一个账号从多个 IP 出去。
/// 本页管代理本身与代理组；全局默认出口和后台探测是站点级配置，在系统设置的「出口代理」页签。
export function ProxiesPage() {
  const { t } = useTranslation()
  const can = usePermission()
  const [tab, setTab] = useState<Tab>('proxies')
  const tabs = (
    <Tabs
      ariaLabel={t('admin:proxiesTitle')}
      items={[
        { id: 'proxies', label: t('admin:proxiesTab') },
        { id: 'groups', label: t('admin:proxyGroupsTab') },
        { id: 'traffic', label: t('admin:proxyTrafficTab') },
      ]}
      active={tab}
      onChange={(id) => setTab(id as Tab)}
    />
  )
  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:proxiesTitle')}
        description={t('admin:proxiesDesc')}
        action={can('settings.read') ? (
          <Link to="/admin/settings" search={{ tab: 'egress' }} className={buttonVariants({ variant: 'outline' })}>
            <Settings aria-hidden className="h-4 w-4" />
            {t('admin:egressSettingsLink')}
          </Link>
        ) : undefined}
      />
      {tab === 'proxies' ? (
        <ProxyList tabs={tabs} />
      ) : tab === 'groups' ? (
        <GroupList tabs={tabs} />
      ) : (
        <TrafficPanel tabs={tabs} />
      )}
    </div>
  )
}
