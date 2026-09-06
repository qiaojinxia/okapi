import { Link } from '@tanstack/react-router'
import { Activity, ArrowRight, Check, KeyRound, Plug, Tags } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { ConnectPanel } from './ConnectPanel'
import { GUIDE_STEPS, dismissGuide, useGuideProgress } from './guide-state'
import type { GuideStep } from './guide-state'
import { Button, buttonVariants } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { useMe } from '@/hooks/use-auth'
import { cn } from '@/lib/utils'

/// 接入指南抽屉：四步竖排（拿到密钥 → 选模型 → 配置客户端 → 首次调用）。
///
/// 不做遮罩式导览（Sub2API 的 driver.js 方案）：那种导览靠页面元素锚点，页面一改就断，
/// 且只能"看一遍"。这里每一步说清"为什么 + 去哪"，第 3 步直接就是可复制的配置——
/// 用户来这里的目的是把工具接上，不是看一遍界面。
export function GuideDrawer({ open, apiKey, onClose }: { open: boolean; apiKey?: string; onClose: () => void }) {
  const { t } = useTranslation()
  return (
    <Drawer
      open={open}
      onClose={onClose}
      title={t('portal:guideTitle')}
      description={t('portal:guideDesc')}
      size="lg"
      footer={<GuideFooter onClose={onClose} />}
    >
      <GuideSteps apiKey={apiKey} />
    </Drawer>
  )
}

function GuideFooter({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation()
  const me = useMe()
  return (
    <>
      <Button variant="ghost" onClick={onClose}>{t('portal:guideLater')}</Button>
      <Button
        onClick={() => {
          if (me.data) dismissGuide(me.data.user_id)
          onClose()
        }}
      >
        {t('portal:guideFinish')}
      </Button>
    </>
  )
}

interface StepSpec {
  id: GuideStep
  icon: LucideIcon
  title: string
  desc: string
  /// 已完成时替代描述的一句回执（"已有 2 把密钥"）。
  status?: string
  actions?: React.ReactNode
  body?: React.ReactNode
}

function GuideSteps({ apiKey }: { apiKey?: string }) {
  const { t } = useTranslation()
  const me = useMe()
  const progress = useGuideProgress()
  const group = me.data?.group ?? ''
  const current = GUIDE_STEPS.find((step) => !progress.done[step])
  const link = (variant: 'outline' | 'ghost' = 'outline') => buttonVariants({ size: 'sm', variant })
  // 从"密钥已创建"进来的人要的就是配置：直接停在第 3 步，不必先滚过前两步
  const connect = useRef<HTMLLIElement>(null)
  useEffect(() => {
    if (apiKey !== undefined) connect.current?.scrollIntoView({ block: 'start' })
  }, [apiKey])
  const steps: StepSpec[] = [
    {
      id: 'key',
      icon: KeyRound,
      title: t('portal:guideStepKeyTitle'),
      desc: t('portal:guideStepKeyDesc'),
      status: progress.done.key ? t('portal:guideStepKeyDone', { n: progress.keyCount }) : undefined,
      actions: (
        <Link to="/portal/keys" className={link()}>
          {t('portal:guideStepKeyAction')}<ArrowRight className="h-3.5 w-3.5" />
        </Link>
      ),
    },
    {
      id: 'model',
      icon: Tags,
      title: t('portal:guideStepModelTitle'),
      desc: t('portal:guideStepModelDesc', { group }),
      actions: (
        <Link to="/pricing" search={{ available: true, group: group || undefined }} className={link()}>
          {t('portal:guideStepModelAction')}<ArrowRight className="h-3.5 w-3.5" />
        </Link>
      ),
    },
    {
      id: 'connect',
      icon: Plug,
      title: t('portal:guideStepConnectTitle'),
      desc: t('portal:guideStepConnectDesc'),
      body: <ConnectPanel apiKey={apiKey} group={group} />,
    },
    {
      id: 'call',
      icon: Activity,
      title: t('portal:guideStepCallTitle'),
      desc: t('portal:guideStepCallDesc'),
      status: progress.done.call ? t('portal:guideStepCallDone') : undefined,
      actions: (
        <>
          <Link to="/portal/logs" className={link()}>
            {t('portal:guideStepCallAction')}<ArrowRight className="h-3.5 w-3.5" />
          </Link>
          <Link to="/portal/ledger" className={link('ghost')}>{t('portal:ledgerNav')}</Link>
        </>
      ),
    },
  ]
  return (
    <ol className="flex flex-col" aria-label={t('portal:guideSteps')}>
      {steps.map((step, index) => (
        <li
          key={step.id}
          ref={step.id === 'connect' ? connect : undefined}
          className={cn(
            'relative flex gap-4 pb-7 last:pb-0',
            // 竖线连到下一步；最后一步不画
            index < steps.length - 1 && 'before:absolute before:top-9 before:bottom-1 before:left-[15px] before:w-px before:bg-border',
          )}
        >
          <StepMarker index={index + 1} done={progress.done[step.id]} current={current === step.id} icon={step.icon} />
          <div className="flex min-w-0 flex-1 flex-col gap-2 pt-1">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="text-sm font-semibold">{step.title}</h3>
              {step.status !== undefined && (
                <span className="inline-flex items-center gap-1 rounded-full bg-success/12 px-2 py-0.5 text-xs font-medium text-success">
                  <Check className="h-3 w-3" />{step.status}
                </span>
              )}
            </div>
            <p className="text-xs leading-5 text-muted-foreground">{step.desc}</p>
            {step.actions !== undefined && <div className="flex flex-wrap items-center gap-2">{step.actions}</div>}
            {step.body !== undefined && <div className="mt-1">{step.body}</div>}
          </div>
        </li>
      ))}
    </ol>
  )
}

/// 步骤圆点：完成打勾，当前步主色，其余灰。
function StepMarker({ index, done, current, icon: Icon }: { index: number; done: boolean; current: boolean; icon: LucideIcon }) {
  return (
    <span
      aria-hidden
      className={cn(
        'flex h-8 w-8 shrink-0 items-center justify-center rounded-full border text-xs font-semibold',
        done ? 'border-success/40 bg-success/12 text-success'
          : current ? 'border-primary bg-primary/10 text-primary'
          : 'border-border bg-card text-muted-foreground',
      )}
    >
      {done ? <Check className="h-4 w-4" /> : current ? <Icon className="h-4 w-4" /> : index}
    </span>
  )
}
