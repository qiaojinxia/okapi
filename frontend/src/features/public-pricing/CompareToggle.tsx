import { Check, Scale } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { PricingModel } from './types'
import { cn } from '@/lib/utils'

/// 同时对比的模型数上限：再多，抽屉里的并排列就窄到读不了价格。路由的 `compareList` 与此一致。
export const MAX_COMPARE = 4

/// 卡片 / 表格行上的"加入对比"开关。满员时未选中的开关禁用并说明原因；含逗号的 ID 无法进入逗号分隔的 URL，同样禁用。
export function CompareToggle({ model, compared, full, onToggle, className }: {
  model: PricingModel; compared: boolean; full: boolean; onToggle: () => void; className?: string
}) {
  const { t } = useTranslation()
  const name = model.display_name || model.model
  const blocked = !compared && (full || model.model.includes(','))
  return <button type="button" aria-pressed={compared} disabled={blocked} onClick={onToggle}
    aria-label={t(compared ? 'catalog:compareRemoveFor' : 'catalog:compareAddFor', { model: name })}
    title={blocked ? t('catalog:compareLimit', { n: MAX_COMPARE }) : t(compared ? 'catalog:compareRemove' : 'catalog:compareAdd')}
    className={cn('flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-not-allowed disabled:opacity-40',
      compared ? 'border-primary/40 bg-primary/10 text-primary' : 'border-border bg-card text-muted-foreground hover:bg-muted hover:text-foreground', className)}>
    {compared ? <Check aria-hidden className="h-4 w-4" /> : <Scale aria-hidden className="h-4 w-4" />}
  </button>
}
