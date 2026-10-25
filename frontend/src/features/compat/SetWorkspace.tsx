import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { Tabs } from '@/components/ui/tabs'
import { useSetOverview } from '@/features/compat/api'
import { Blueprint } from '@/features/compat/Blueprint'
import { useRulesEditor } from '@/features/compat/editor'
import { HistoryPanel } from '@/features/compat/HistoryPanel'
import { useReplay } from '@/features/compat/replay'
import { ReplayPanel } from '@/features/compat/ReplayPanel'
import { RulesWorkbench } from '@/features/compat/RulesWorkbench'
import type { SetScope } from '@/features/compat/types'
import { usePermission } from '@/hooks/use-auth'
import { describeError } from '@/lib/i18n'

type WorkspaceTab = 'rules' | 'blueprint' | 'replay' | 'history' | 'settings'

/// 一个规则集的工作区：规则（编辑与审批）/ 蓝图 / 试跑 / 历史（档案还有设置）。
/// 档案在抽屉里用，全局护栏直接铺在页面里；渠道层的审阅也从收件箱进到这里。
export function SetWorkspace({
  scope,
  id,
  extendsVendor,
  settings,
  idPrefix,
}: {
  scope: SetScope
  id: number
  /// 试跑时叠在下面的内置厂商（档案的 `extends`）。
  extendsVendor?: string | null
  /// 档案的「设置」页签内容；没有就不出这个页签。
  settings?: React.ReactNode
  idPrefix: string
}) {
  const { t } = useTranslation()
  const can = usePermission()
  const overview = useSetOverview(scope, id)
  const editor = useRulesEditor(overview.data)
  const replay = useReplay(
    {
      ...(scope === 'channel' ? { channelId: id } : {}),
      ...(extendsVendor === undefined || extendsVendor === null ? {} : { vendor: extendsVendor }),
    },
    editor.rules,
  )
  const [tab, setTab] = useState<WorkspaceTab>('rules')
  const canWrite =
    scope === 'channel' ? can('channel.write') : scope === 'profile' ? can('compat.profile.write') : can('compat.guard.write')
  const canPublish = can('compat.publish')

  const tabs: { id: WorkspaceTab; label: string }[] = [
    { id: 'rules', label: t('compat:tabRules') },
    { id: 'blueprint', label: t('compat:tabBlueprint') },
    { id: 'replay', label: t('compat:tabReplay') },
    { id: 'history', label: t('compat:tabHistory') },
    ...(settings === undefined ? [] : [{ id: 'settings' as const, label: t('compat:tabSettings') }]),
  ]

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <Tabs
        id={`${idPrefix}-tabs`}
        ariaLabel={t('compat:workspaceTabs')}
        items={tabs.map((item) => ({ ...item, panelId: `${idPrefix}-panel` }))}
        active={tab}
        onChange={(next) => setTab(next as WorkspaceTab)}
      />
      <div id={`${idPrefix}-panel`} role="tabpanel" aria-labelledby={`${idPrefix}-tabs-${tab}`} className="min-w-0">
        {tab === 'settings' ? (
          settings
        ) : overview.isError ? (
          <ErrorState message={describeError(overview.error)} onRetry={() => void overview.refetch()} />
        ) : overview.isPending ? (
          <div className="flex flex-col gap-3">
            <Skeleton className="h-5 w-48" />
            <Skeleton className="h-64 w-full" />
          </div>
        ) : tab === 'rules' ? (
          <RulesWorkbench
            scope={scope}
            id={id}
            overview={overview.data}
            editor={editor}
            canWrite={canWrite}
            canPublish={canPublish}
          />
        ) : tab === 'blueprint' ? (
          <Blueprint rules={editor.rules} replay={replay} />
        ) : tab === 'replay' ? (
          <ReplayPanel replay={replay} />
        ) : (
          <HistoryPanel scope={scope} id={id} canPublish={canPublish} />
        )}
      </div>
    </div>
  )
}
