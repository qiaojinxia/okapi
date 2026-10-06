import { useTranslation } from 'react-i18next'
import { EmptyState } from '@/components/ui/state'
import { DefaultEgressCard } from '@/features/proxies/DefaultEgressCard'
import { ProbePolicyCard } from '@/features/proxies/ProbePolicyCard'
import { usePermission } from '@/hooks/use-auth'

/// 系统设置 →「出口代理」页签：全局默认出口与后台探测（IMPLEMENTATION §11.41）。代理与代理组本身在出口代理页管。
///
/// 两张卡按各自接口的权限显示：默认出口读要 channel.read、改要 channel.write 的全部范围（后端校验）；
/// 探测策略存在站点设置里，读写都要 settings.write。
export function EgressSettings() {
  const { t } = useTranslation()
  const can = usePermission()
  const showDefault = can('channel.read')
  const showProbe = can('settings.write')
  if (!showDefault && !showProbe) return <EmptyState title={t('errors:http_403')} />
  return (
    <div className="flex flex-col gap-4">
      {showDefault && <DefaultEgressCard />}
      {showProbe && <ProbePolicyCard />}
    </div>
  )
}
