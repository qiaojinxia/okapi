import { Check, Rocket, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { GUIDE_STEPS, dismissGuide, useGuide, useGuideDismissed, useGuideProgress } from './guide-state'
import type { GuideStep } from './guide-state'
import { Button } from '@/components/ui/button'
import { useMe } from '@/hooks/use-auth'
import { cn } from '@/lib/utils'

/// 总览页顶部的"快速开始"卡：只在还有步骤没完成、且没被关掉时出现。
///
/// 新用户登录后看到的总览是六张全零的 KPI——他需要的不是数据而是"下一步做什么"。
/// 全部完成的老用户从不看到它（不必再点一次关闭）；关闭按用户记忆。
export function GettingStartedCard() {
  const { t } = useTranslation()
  const me = useMe()
  const guide = useGuide()
  const dismissed = useGuideDismissed()
  const progress = useGuideProgress()
  if (dismissed || progress.loading || progress.completed >= GUIDE_STEPS.length) return null

  const labels: Record<GuideStep, string> = {
    key: t('portal:guideStepKeyTitle'),
    model: t('portal:guideStepModelTitle'),
    connect: t('portal:guideStepConnectTitle'),
    call: t('portal:guideStepCallTitle'),
  }
  const current = GUIDE_STEPS.find((step) => !progress.done[step])

  return (
    <section
      aria-label={t('portal:guideTitle')}
      className="relative overflow-hidden rounded-xl border border-primary/25 bg-linear-to-br from-primary/8 via-card to-card p-5 shadow-card animate-fade-in"
    >
      <button
        type="button"
        aria-label={t('portal:guideSkip')}
        className="absolute top-3 right-3 rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        onClick={() => me.data && dismissGuide(me.data.user_id)}
      >
        <X className="h-4 w-4" />
      </button>
      <div className="flex flex-wrap items-start justify-between gap-4 pr-8">
        <div className="flex min-w-0 items-start gap-3">
          <span className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/12 text-primary">
            <Rocket className="h-4.5 w-4.5" />
          </span>
          <div className="flex min-w-0 flex-col gap-1">
            <h2 className="text-base font-semibold">{t('portal:guideTitle')}</h2>
            <p className="max-w-2xl text-sm leading-5 text-muted-foreground">{t('portal:guideCardDesc')}</p>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button onClick={() => guide.open()}>{t('portal:guideOpen')}</Button>
        </div>
      </div>

      <ol className="mt-4 grid gap-2 sm:grid-cols-2 xl:grid-cols-4" aria-label={t('portal:guideSteps')}>
        {GUIDE_STEPS.map((step, index) => {
          const done = progress.done[step]
          return (
            <li key={step}>
              <button
                type="button"
                onClick={() => guide.open()}
                className={cn(
                  'flex min-h-11 w-full items-center gap-2.5 rounded-lg border px-3 text-left text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary/40',
                  done ? 'border-success/30 bg-success/8 text-muted-foreground'
                    : current === step ? 'border-primary/40 bg-card font-medium shadow-card hover:border-primary'
                    : 'border-border bg-card/60 text-muted-foreground hover:bg-card',
                )}
              >
                <span
                  aria-hidden
                  className={cn(
                    'flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold',
                    done ? 'bg-success/15 text-success' : current === step ? 'bg-primary/12 text-primary' : 'bg-muted',
                  )}
                >
                  {done ? <Check className="h-3.5 w-3.5" /> : index + 1}
                </span>
                <span className="truncate">{labels[step]}</span>
              </button>
            </li>
          )
        })}
      </ol>

      <div className="mt-3 flex items-center gap-3">
        <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted" role="progressbar" aria-valuemin={0} aria-valuemax={GUIDE_STEPS.length} aria-valuenow={progress.completed} aria-label={t('portal:guideProgress', { done: progress.completed, total: GUIDE_STEPS.length })}>
          <div className="h-full rounded-full bg-primary transition-[width] duration-300" style={{ width: `${(progress.completed / GUIDE_STEPS.length) * 100}%` }} />
        </div>
        <span className="text-xs tabular-nums text-muted-foreground">{t('portal:guideProgress', { done: progress.completed, total: GUIDE_STEPS.length })}</span>
      </div>
    </section>
  )
}
