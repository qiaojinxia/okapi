import { useState } from 'react'
import { Calendar } from './calendar'
import { DateTrigger } from './date-range'
import { Popover } from './popover'

/// 单个日期：按钮上写着当前日期，点开是月历，点一天就选定并收起（没有"确定"一步）。
/// `isDisabled` 标出月历里点不了的日子（个人中心只有那一年里的日子可选）。
export function DateField({ today, value, onChange, min, max, isDisabled, label, className }: {
  today: string
  value: string
  onChange: (day: string) => void
  min?: string
  max?: string
  isDisabled?: (day: string) => boolean
  /// 弹出框的可访问名称，也是按钮可访问名称的前缀（按钮上只显示日期本身）。
  label: string
  className?: string
}) {
  const [open, setOpen] = useState(false)
  return (
    <Popover
      open={open}
      onOpenChange={setOpen}
      label={label}
      trigger={<DateTrigger aria-label={`${label} ${value}`} className={className}>{value}</DateTrigger>}
    >
      {({ close }) => (
        <Calendar
          autoFocus
          today={today}
          mode="single"
          start={value}
          min={min}
          max={max}
          isDisabled={isDisabled}
          onPick={(day) => {
            onChange(day)
            close()
          }}
        />
      )}
    </Popover>
  )
}
