import { X } from 'lucide-react'
import { useId, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Input } from '@/components/ui/input'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import type { InputSuggestion } from '@/components/ui/autocomplete-input'
import { cn } from '@/lib/utils'

interface TagInputProps extends Pick<React.InputHTMLAttributes<HTMLInputElement>, 'aria-invalid' | 'aria-describedby'> {
  value: string[]
  onChange: (value: string[]) => void
  placeholder?: string
  id?: string
  className?: string
  suggestions?: readonly (string | InputSuggestion)[]
}

const splitTags = (raw: string) => [...new Set(raw.split(/[,，\s]+/).map((part) => part.trim()).filter(Boolean))]

/// 字符串列表输入（chips）。
///
/// 用于模型列表、要剥离的请求字段这类"若干个短字符串"。此前这些值要么塞在
/// 逗号分隔的单行文本里（分隔符与空格全靠用户自觉），要么埋在 JSON 数组里。
/// chips 形态让每一项可见、可单独删除，也把分隔符问题彻底消掉。
export function TagInput({ value, onChange, placeholder, id, className, suggestions, 'aria-invalid': invalid, 'aria-describedby': describedBy }: TagInputProps) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState('')
  const [activeTag, setActiveTag] = useState<string | null>(null)
  const root = useRef<HTMLDivElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const pasteCaret = useRef<number | null>(null)
  const summaryId = useId()
  const selected = [...new Set(value)]
  const pending = splitTags(draft).filter((tag) => !selected.includes(tag))
  const displayed = [...selected, ...pending]
  const tabEntry = activeTag !== null && displayed.includes(activeTag) ? activeTag : displayed[0]
  const focusInput = () => root.current?.querySelector<HTMLInputElement>('input')?.focus({ preventScroll: true })

  useLayoutEffect(() => {
    if (pasteCaret.current === null) return
    const input = root.current?.querySelector<HTMLInputElement>('input')
    if (input === document.activeElement) input?.setSelectionRange(pasteCaret.current, pasteCaret.current)
    pasteCaret.current = null
  }, [draft])

  const commit = (raw: string) => {
    // 一次粘贴多个是常见操作（从别处复制模型清单），逗号/空白都当分隔符
    const next = [...new Set([...value, ...splitTags(raw)])]
    if (next.length !== value.length || next.some((tag, index) => tag !== value[index])) onChange(next)
    setDraft('')
  }

  const remove = (tag: string) => {
    if (value.includes(tag)) onChange(value.filter((item) => item !== tag))
    // 同时清除草稿里的重复项，防止删除后又在失焦时加回来。
    setDraft(splitTags(draft).filter((item) => item !== tag).join(', '))
    focusInput()
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return
    if (e.key === 'Enter' || e.key === ',' || e.key === '，') {
      e.preventDefault()
      commit(draft)
    } else if (e.key === 'Backspace' && draft === '' && selected.length > 0) {
      e.preventDefault()
      onChange(selected.slice(0, -1))
    }
  }
  const inputProps = {
    id, value: draft, placeholder, onKeyDown,
    'aria-invalid': invalid,
    'aria-describedby': [describedBy, displayed.length > 0 ? summaryId : undefined].filter(Boolean).join(' ') || undefined,
    onPaste: (event: React.ClipboardEvent<HTMLInputElement>) => {
      const text = event.clipboardData.getData('text/plain')
      if (!/[\r\n]/.test(text)) return
      // 单行 input 会直接丢弃换行；先替换分隔符，避免两行标识被拼接。
      event.preventDefault()
      const input = event.currentTarget
      const pasted = text.replace(/\r\n|\r|\n/g, ', ')
      const start = input.selectionStart ?? draft.length
      const end = input.selectionEnd ?? start
      const next = draft.slice(0, start) + pasted + draft.slice(end)
      const caret = start + pasted.length
      if (next === draft) input.setSelectionRange(caret, caret)
      else { pasteCaret.current = caret; setDraft(next) }
    },
    // 直接保存时也收下尚未按回车的自定义值。
    onBlur: () => commit(draft),
  }

  return (
    <div ref={root} className={cn('flex min-w-0 flex-col gap-1.5', className)}>
      {suggestions ? (
        <AutocompleteInput {...inputProps} inputClassName="h-11 font-mono text-xs md:h-9"
          onChange={setDraft} onChoose={commit}
          options={suggestions.map((item) => typeof item === 'string' ? { value: item } : item).filter((item) => !value.includes(item.value))}
        />
      ) : <Input {...inputProps} className="h-11 font-mono text-xs md:h-9" onChange={(e) => setDraft(e.target.value)} />}
      {/* 草稿先占据与确认后相同的位置；失焦确认只改变状态，不移动保存等后续操作。 */}
      {displayed.length > 0 && <>
        <p id={summaryId} role="status" className="truncate text-xs leading-5 text-muted-foreground">
          {t('common:tagsAdded', { count: selected.length })}
          {pending.length > 0 && <span className="text-primary">{' · '}{t('common:tagsPending', { count: pending.length })}</span>}
        </p>
        <div ref={list} role="group" aria-label={t('common:tagKeyboardHint')} className="flex max-h-36 flex-wrap items-center gap-1.5 overflow-y-auto overscroll-contain">
          {displayed.map((tag) => {
            const isPending = !value.includes(tag)
            return (
              <span
                key={tag}
                className={cn('inline-flex min-w-0 max-w-full items-center gap-1 rounded-md border py-0 pr-1 pl-2 font-mono text-xs md:py-1', isPending ? 'border-dashed border-primary/40 bg-primary/5' : 'border-border bg-muted/60')}
              >
                <span className="min-w-0 break-all">{tag}</span>
                <button
                  type="button"
                  aria-label={t(isPending ? 'common:discardTag' : 'common:removeTag', { tag })}
                  tabIndex={tabEntry === tag ? 0 : -1}
                  className="flex h-11 w-11 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive focus-visible:ring-2 focus-visible:ring-primary/40 md:h-6 md:w-6"
                  // 删除只处理这一项，不让失焦把输入框里其他待确认的内容一并加入。
                  onPointerDown={(event) => event.preventDefault()}
                  onFocus={() => setActiveTag(tag)}
                  onKeyDown={(event) => {
                    if (event.altKey || event.ctrlKey || event.metaKey) return
                    if (event.key === 'Delete' || event.key === 'Backspace') {
                      event.preventDefault()
                      remove(tag)
                    } else if (event.key === 'Escape') {
                      event.preventDefault()
                      event.stopPropagation()
                      focusInput()
                    } else {
                      const index = displayed.indexOf(tag)
                      const next = event.key === 'Home' ? 0 : event.key === 'End' ? displayed.length - 1
                        : event.key === 'ArrowRight' ? (index + 1) % displayed.length
                          : event.key === 'ArrowLeft' ? (index - 1 + displayed.length) % displayed.length : null
                      if (next !== null) {
                        event.preventDefault()
                        list.current?.querySelectorAll<HTMLButtonElement>('button')[next]?.focus()
                      }
                    }
                  }}
                  onClick={() => remove(tag)}
                >
                  <X aria-hidden className="h-3 w-3" />
                </button>
              </span>
            )
          })}
        </div>
      </>}
    </div>
  )
}
