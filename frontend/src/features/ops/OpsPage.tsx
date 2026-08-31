import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/components/ui/page'
import { ReconciliationCard } from '@/features/ops/ReconciliationCard'
import { RefundCard } from '@/features/ops/RefundCard'
import { RetentionCard } from '@/features/ops/RetentionCard'

/// 运维操作页：会改动或删除既有数据的动作都在这里（退款冲销、留存裁剪）。
/// 纯配置项在 /admin/settings，看数在 /admin/stats——放一起会让人误点。
export function OpsPage() {
  const { t } = useTranslation()
  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('admin:opsTitle')} description={t('admin:opsDesc')} />
      <RefundCard />
      <ReconciliationCard />
      <RetentionCard />
    </div>
  )
}
