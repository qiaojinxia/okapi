import { Search, X } from 'lucide-react'
import { useId, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { inputClass } from '@/components/ui/input'
import { cn } from '@/lib/utils'

export interface InputSuggestion {
  value: string
  label?: string
  description?: string
}

export interface AutocompleteInputProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'value' | 'onChange' | 'onSubmit' | 'list'> {
  value: string
  onChange: (value: string) => void
  options: readonly InputSuggestion[]
  onChoose?: (value: string) => void
  onSubmit?: () => void
  loading?: boolean
  error?: string
  search?: boolean
  inputClassName?: string
  /// 已选实体显示名称；编辑和选择仍通过 value 回传准确标识。
  displayValue?: string
  optionLabelFirst?: boolean
  emptyHint?: string
  moreHint?: string
}

const normalize = (value: string) => value.normalize('NFKC').trim().toLowerCase()

// 手输保持原值；只有显式选择才替换为目录中的准确 ID。
export function AutocompleteInput({
  value, onChange, options, onChoose, onSubmit, loading, error, search = false,
  displayValue, optionLabelFirst = false, emptyHint, moreHint,
  className, inputClassName, onKeyDown, onFocus, onBlur, disabled, readOnly, ...props
}: AutocompleteInputProps) {
  const { t } = useTranslation()
  const listId = useId()
  const hintId = useId()
  const root = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLInputElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const list = useRef<HTMLUListElement>(null)
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState<string | null>(null)
  const expanded = open && !disabled && !readOnly
  const matches = useMemo(() => {
    const query = normalize(value)
    const terms = query.split(/\s+/).filter(Boolean)
    const unique = [...new Map(options.map((option) => [option.value, option])).values()]
    const rank = (option: InputSuggestion) => {
      const id = normalize(option.value)
      return id === query ? 0 : id.startsWith(query) ? 1 : id.includes(query) ? 2 : 3
    }
    return unique.filter((option) => {
      const text = normalize(`${option.value} ${option.label ?? ''} ${option.description ?? ''}`)
      return terms.every((term) => text.includes(term))
    }).sort((a, b) => rank(a) - rank(b))
  }, [options, value])
  const visible = matches.slice(0, 40)
  const activeIndex = visible.findIndex((option) => option.value === active)

  // 原生 top layer 避开抽屉的 overflow 裁切；DOM 仍属于表单，焦点始终留在输入框。
  useLayoutEffect(() => {
    if (!expanded || !panel.current) return
    const element = panel.current
    let previousPosition = ''
    const position = () => {
      const bounds = input.current?.getBoundingClientRect()
      if (!bounds) return
      const viewport = window.visualViewport
      const top = viewport?.offsetTop ?? 0
      const bottom = top + (viewport?.height ?? window.innerHeight)
      const left = viewport?.offsetLeft ?? 0
      const width = Math.min(bounds.width, (viewport?.width ?? window.innerWidth) - 16)
      const x = Math.max(left + 8, Math.min(bounds.left, left + (viewport?.width ?? window.innerWidth) - width - 8))
      const below = bottom - bounds.bottom - 8
      const above = bounds.top - top - 8
      const upwards = below < 220 && above > below
      const height = Math.min(320, Math.max(0, upwards ? above : below))
      const signature = [x, width, bounds.top, bounds.bottom, height, upwards].join(',')
      if (signature === previousPosition) return
      previousPosition = signature
      Object.assign(element.style, {
        width: `${width}px`, left: `${x}px`, maxHeight: `${height}px`,
        top: upwards ? `${bounds.top - 4}px` : `${bounds.bottom + 4}px`,
        transform: upwards ? 'translateY(-100%)' : 'none',
      })
    }
    position()
    element.showPopover()
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false)
    }
    // 抽屉通过 transform 入场，ResizeObserver 不会报告位置变化。
    // 只在候选展开时跟随锚点；位置未变时不写样式，也覆盖滚动和软键盘变化。
    let frame = 0
    const follow = () => {
      position()
      frame = requestAnimationFrame(follow)
    }
    frame = requestAnimationFrame(follow)
    document.addEventListener('pointerdown', outside)
    return () => {
      if (element.matches(':popover-open')) element.hidePopover()
      cancelAnimationFrame(frame)
      document.removeEventListener('pointerdown', outside)
    }
  }, [expanded])

  useLayoutEffect(() => {
    const option = list.current?.children[activeIndex] as HTMLElement | undefined
    option?.scrollIntoView({ block: 'nearest' })
  }, [activeIndex])

  const choose = (option: InputSuggestion) => {
    setOpen(false)
    setActive(null)
    onChange(option.value)
    onChoose?.(option.value)
  }

  return (
    <div ref={root} className={cn('relative min-w-0', className)}>
      {search && <Search aria-hidden className="pointer-events-none absolute top-1/2 left-2.5 h-4 w-4 -translate-y-1/2 text-muted-foreground" />}
      <input
        {...props}
        ref={input}
        value={displayValue ?? value}
        disabled={disabled}
        readOnly={readOnly}
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={Boolean(expanded)}
        aria-controls={expanded ? listId : undefined}
        aria-activedescendant={expanded && activeIndex >= 0 ? `${listId}-${activeIndex}` : undefined}
        aria-describedby={[props['aria-describedby'], expanded ? hintId : undefined].filter(Boolean).join(' ') || undefined}
        autoComplete="off"
        className={cn(inputClass, 'h-9 w-full px-3', search && 'pr-11 pl-8 md:pr-9', inputClassName)}
        onChange={(event) => {
          onChange(event.target.value)
          setActive(null)
          setOpen(true)
        }}
        onFocus={(event) => { setOpen(true); onFocus?.(event) }}
        onBlur={(event) => { setOpen(false); setActive(null); onBlur?.(event) }}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) {
            // Escape 先交给中文输入法，不传到抽屉的关闭快捷键。
            event.stopPropagation()
            return
          }
          if (!disabled && !readOnly && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
            event.preventDefault()
            setOpen(true)
            const index = event.key === 'ArrowDown' ? (activeIndex + 1) % visible.length
              : activeIndex <= 0 ? visible.length - 1 : activeIndex - 1
            setActive(visible[index]?.value ?? null)
            return
          }
          if (expanded && event.key === 'Escape') {
            event.preventDefault()
            event.stopPropagation()
            setOpen(false)
            setActive(null)
            return
          }
          if (expanded && activeIndex >= 0 && event.key === 'Enter') {
            event.preventDefault()
            choose(visible[activeIndex])
            return
          }
          onKeyDown?.(event)
          if (event.key === 'Tab' || event.key === 'Enter') setOpen(false)
          if (event.key === 'Enter' && !event.defaultPrevented && onSubmit) {
            event.preventDefault()
            onSubmit()
          }
        }}
      />
      {search && value && !disabled && !readOnly && (
        <button type="button" aria-label={t('common:clear')} className="absolute inset-y-0 right-0 flex w-11 items-center justify-center rounded text-muted-foreground hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40 md:w-9"
          onClick={() => { onChange(''); setActive(null); input.current?.focus(); setOpen(true) }}>
          <X aria-hidden className="h-3.5 w-3.5" />
        </button>
      )}
      {expanded && (
        <div ref={panel} popover="manual" className={cn('fixed inset-auto m-0 overflow-auto overscroll-contain rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-popover', visible.length === 0 && 'pointer-events-none')}>
          <p className="px-2 py-1.5 text-xs text-muted-foreground" id={hintId}>
            {loading ? t('common:loading') : error ?? (visible.length ? t('common:suggestionsHint') : emptyHint ?? t('common:freeTextHint'))}
          </p>
          <ul ref={list} id={listId} role="listbox" aria-label={t('common:suggestions')} aria-busy={loading}>
            {visible.map((option, index) => (
              <li key={option.value} id={`${listId}-${index}`} role="option" aria-selected={index === activeIndex}
                className={cn('flex min-h-11 cursor-pointer flex-col gap-0.5 rounded-md px-2 py-2 text-sm hover:bg-accent', index === activeIndex && 'bg-accent')}
                onPointerDown={(event) => event.preventDefault()}
                onClick={() => choose(option)}>
                <span className={cn('break-all text-xs', optionLabelFirst ? 'font-medium' : 'font-mono')}>{optionLabelFirst ? option.label || option.value : option.value}</span>
                {(option.label || option.description) && <span className="break-words text-xs text-muted-foreground">{[optionLabelFirst ? `ID ${option.value}` : option.label, option.description].filter(Boolean).join(' · ')}</span>}
              </li>
            ))}
          </ul>
          {matches.length > visible.length && <p className="px-2 py-1.5 text-xs text-muted-foreground">{t('common:suggestionsMore', { n: matches.length })}</p>}
          {moreHint && <p className="px-2 py-1.5 text-xs text-muted-foreground">{moreHint}</p>}
        </div>
      )}
    </div>
  )
}
