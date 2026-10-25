import { Plus } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { PageHeader } from '@/components/ui/page'
import { Tabs } from '@/components/ui/tabs'
import { useInbox } from '@/features/compat/api'
import { GuardTab } from '@/features/compat/GuardTab'
import { InboxTab } from '@/features/compat/InboxTab'
import { ProfileCreateDrawer } from '@/features/compat/ProfileCreateDrawer'
import { ProfilesTab } from '@/features/compat/ProfilesTab'
import { ReferenceTab } from '@/features/compat/ReferenceTab'
import { SetDrawer, type SetTarget } from '@/features/compat/SetDrawer'
import { usePermission } from '@/hooks/use-auth'

type CompatTab = 'profiles' | 'inbox' | 'guard' | 'reference'

/// 适配规则：上游与终端之间的形状差异写成带版本、可回放、可审计的数据（设计见 docs/compat-rules.md）。
///
/// 渠道自己的「剥 / 注入字段」仍在渠道抽屉里；这里管**可复用**的那几层：适配档案（一类上游的处理方式，
/// 渠道的厂商字段选它）、全局护栏（对所有渠道生效的最后一层），以及待批准的草稿——AI 经 MCP 提议、
/// 或没有发布权限的人提交的规则都先落草稿，在收件箱里看过差异再批准。
export function CompatPage() {
  const { t } = useTranslation()
  const can = usePermission()
  const canReview = can('compat.publish')
  const canCreate = can('compat.profile.write')
  const inbox = useInbox(canReview)
  const [tab, setTab] = useState<CompatTab>('profiles')
  const [target, setTarget] = useState<SetTarget | null>(null)
  const [creating, setCreating] = useState(false)

  const tabs: { id: CompatTab; label: string; count?: number }[] = [
    { id: 'profiles', label: t('compat:tabProfiles') },
    ...(canReview ? [{ id: 'inbox' as const, label: t('compat:tabInbox'), count: inbox.data?.length }] : []),
    { id: 'guard', label: t('compat:tabGuard') },
    { id: 'reference', label: t('compat:tabReference') },
  ]
  const active = tabs.some((item) => item.id === tab) ? tab : 'profiles'

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <PageHeader
        title={t('compat:title')}
        description={t('compat:description')}
        action={
          active === 'profiles' && canCreate ? (
            <Button onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" />
              {t('compat:profileCreate')}
            </Button>
          ) : undefined
        }
      />
      <Tabs
        id="compat-tabs"
        variant="underline"
        ariaLabel={t('compat:title')}
        items={tabs.map((item) => ({ ...item, panelId: 'compat-panel' }))}
        active={active}
        onChange={(id) => setTab(id as CompatTab)}
      />
      <div id="compat-panel" role="tabpanel" aria-labelledby={`compat-tabs-${active}`} className="min-w-0">
        {active === 'profiles' && <ProfilesTab canCreate={canCreate} onCreate={() => setCreating(true)} onOpen={setTarget} />}
        {active === 'inbox' && <InboxTab onOpen={setTarget} onOpenGuard={() => setTab('guard')} />}
        {active === 'guard' && <GuardTab />}
        {active === 'reference' && <ReferenceTab />}
      </div>
      <ProfileCreateDrawer
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(profile) => {
          setCreating(false)
          setTarget({ scope: 'profile', id: profile.id, name: profile.name })
        }}
      />
      <SetDrawer target={target} onClose={() => setTarget(null)} />
    </div>
  )
}
