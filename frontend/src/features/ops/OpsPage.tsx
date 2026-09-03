import { Archive, Inbox, Scale, Undo2, Wrench } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { DlqCard } from '@/features/ops/DlqCard'
import { PageHeader } from '@/components/ui/page'
import { ReconciliationCard } from '@/features/ops/ReconciliationCard'
import { RefundCard } from '@/features/ops/RefundCard'
import { RetentionCard } from '@/features/ops/RetentionCard'
import { Tabs } from '@/components/ui/tabs'

const OPS_TABS = ['refund', 'dlq', 'reconciliation', 'retention'] as const
type OpsTab = (typeof OPS_TABS)[number]

/// 运维操作页：会改动或删除既有数据的动作都在这里（退款冲销、死信处置、留存裁剪）。
/// 纯配置项在 /admin/settings，看数在 /admin/stats——放一起会让人误点。
/// 四个动作各自页签：都是不可逆操作，同屏堆多个提交按钮正是误点的温床。
export function OpsPage() {
  const { t } = useTranslation()
  const [tab, setTab] = useState<OpsTab>('refund')

  const items = [
    { id: 'refund', label: t('admin:refundTitle'), icon: Undo2 },
    { id: 'dlq', label: t('admin:dlqTitle'), icon: Inbox },
    { id: 'reconciliation', label: t('admin:reconciliation'), icon: Scale },
    { id: 'retention', label: t('admin:retention'), icon: Archive },
  ]

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('admin:opsTitle')} description={t('admin:opsDesc')} icon={Wrench} />
      <Tabs variant="underline" items={items} active={tab} onChange={(id) => setTab(id as OpsTab)} />
      {tab === 'refund' && <RefundCard />}
      {tab === 'dlq' && <DlqCard />}
      {tab === 'reconciliation' && <ReconciliationCard />}
      {tab === 'retention' && <RetentionCard />}
    </div>
  )
}
