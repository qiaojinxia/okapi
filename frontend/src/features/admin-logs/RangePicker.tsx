import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Field } from '@/components/ui/field'
import { DateTrigger } from '@/components/ui/date-range'
import { Input } from '@/components/ui/input'
import { Popover } from '@/components/ui/popover'
import { Segmented } from '@/components/ui/segmented'
import { validRange } from './search'
import type { Draft } from './search'

/// 时间窗：四档相对预设（1h 排障、24h 日常、7d 周报、30d 月度）+ 绝对起止
/// （对账"某一天的账"）。两者互斥：填了起止预设全部熄灭；点预设清空起止。
/// 起止不做输入即查——两个时间要一起填完才有意义，回车或点"应用"才提交。
export function RangePicker({
  draft,
  onPreset,
  onRange,
  onApplyRange,
}: {
  draft: Draft
  onPreset: (h: number) => void
  onRange: (from: string, to: string) => void
  onApplyRange: () => void
}) {
  const { t } = useTranslation()
  const options = [
    { h: 1, label: t('admin:logsHours1') },
    { h: 24, label: t('admin:logsHours24') },
    { h: 168, label: t('admin:logsHours168') },
    { h: 720, label: t('admin:logsHours720') },
  ]
  const absolute = draft.from !== ''
  const [open, setOpen] = useState(false)
  // 打开时记下当时的起止；没点「应用」就关掉（Esc / 点外面 / 取消）就退回去，别留半截输入
  const snapshot = useRef({ from: draft.from, to: draft.to })
  const applied = useRef(false)
  const change = (next: boolean) => {
    if (next) {
      snapshot.current = { from: draft.from, to: draft.to }
      applied.current = false
    } else if (!applied.current) {
      onRange(snapshot.current.from, snapshot.current.to)
    }
    setOpen(next)
  }
  const valid = validRange(draft)
  return (
    <section aria-label={t('admin:logsRange')} className="flex min-w-0 flex-1 basis-96 flex-wrap items-center gap-x-3 gap-y-2">
      <Segmented
        size="sm"
        ariaLabel={t('admin:logsRange')}
        // 绝对区间生效时没有预设被选中：传一个不存在的值让全部熄灭
        value={absolute ? -1 : draft.hours}
        onChange={(h) => onPreset(h)}
        options={options.map((o) => ({ value: o.h, label: o.label }))}
      />
      <Popover
        open={open}
        onOpenChange={change}
        label={t('admin:logsCustomRange')}
        trigger={
          <DateTrigger active={absolute} className="h-11 md:h-9">
            {absolute ? `${draft.from.replace('T', ' ')} — ${draft.to.replace('T', ' ') || t('admin:logsUntilNow')}` : t('admin:logsCustomRange')}
          </DateTrigger>
        }
      >
        {({ close }) => {
          const submit = () => {
            if (!valid) return
            applied.current = true
            onApplyRange()
            close()
          }
          const onKey = (e: React.KeyboardEvent) => {
            if (e.key === 'Enter' && !e.nativeEvent.isComposing) {
              e.preventDefault()
              submit()
            }
          }
          return (
            <div className="grid w-[min(34rem,calc(100vw-2.5rem))] gap-3">
              <div className="grid min-w-0 gap-3 sm:grid-cols-2">
                <Field label={t('admin:logsFrom')} htmlFor="logs-from">
                  <Input
                    id="logs-from"
                    type="datetime-local" step="0.001"
                    className="h-11 min-w-0 w-full md:h-9"
                    aria-label={t('admin:logsFrom')}
                    value={draft.from}
                    onChange={(e) => onRange(e.target.value, draft.to)}
                    onKeyDown={onKey}
                  />
                </Field>
                <Field label={t('admin:logsTo')} htmlFor="logs-to">
                  <Input
                    id="logs-to"
                    type="datetime-local" step="0.001"
                    className="h-11 min-w-0 w-full md:h-9"
                    aria-label={t('admin:logsTo')}
                    value={draft.to}
                    onChange={(e) => onRange(draft.from, e.target.value)}
                    onKeyDown={onKey}
                  />
                </Field>
              </div>
              <p className="text-xs text-muted-foreground">{t('admin:logsLocalTime')}</p>
              {(draft.from || draft.to) && !valid && <p role="alert" className="text-xs text-destructive">{t('admin:logsInvalidRange')}</p>}
              <div className="flex items-center justify-end gap-2">
                <Button variant="outline" size="sm" onClick={close}>{t('common:cancel')}</Button>
                <Button size="sm" disabled={!valid} onClick={submit}>{t('admin:logsApplyRange')}</Button>
              </div>
            </div>
          )
        }}
      </Popover>
    </section>
  )
}
