import { X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Input } from '@/components/ui/input'
import { cn } from '@/lib/utils'

interface TagInputProps {
  value: string[]
  onChange: (value: string[]) => void
  placeholder?: string
  id?: string
  className?: string
}

/// 字符串列表输入（chips）。
///
/// 用于模型列表、要剥离的请求字段这类"若干个短字符串"。此前这些值要么塞在
/// 逗号分隔的单行文本里（分隔符与空格全靠用户自觉），要么埋在 JSON 数组里。
/// chips 形态让每一项可见、可单独删除，也把分隔符问题彻底消掉。
export function TagInput({ value, onChange, placeholder, id, className }: TagInputProps) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState('')

  const commit = (raw: string) => {
    // 一次粘贴多个是常见操作（从别处复制模型清单），逗号/空白都当分隔符
    const parts = raw
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter((s) => s !== '')
    if (parts.length === 0) return
    const next = [...value]
    for (const p of parts) if (!next.includes(p)) next.push(p)
    onChange(next)
    setDraft('')
  }

  return (
    <div className={cn('flex flex-col gap-1.5', className)}>
      <div className="flex flex-wrap items-center gap-1.5">
        {value.map((tag) => (
          <span
            key={tag}
            className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-0.5 font-mono text-xs"
          >
            {tag}
            <button
              type="button"
              aria-label={t('common:removeTag', { tag })}
              className="text-muted-foreground hover:text-destructive"
              onClick={() => onChange(value.filter((v) => v !== tag))}
            >
              <X className="h-3 w-3" />
            </button>
          </span>
        ))}
      </div>
      <Input
        id={id}
        value={draft}
        placeholder={placeholder}
        className="font-mono text-xs"
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ',') {
            e.preventDefault()
            commit(draft)
          } else if (e.key === 'Backspace' && draft === '' && value.length > 0) {
            // 空输入框按退格删最后一项：chips 输入的通用手感
            onChange(value.slice(0, -1))
          }
        }}
        // 失焦也提交：用户填完直接去点保存时，不该丢掉没敲回车的那一项
        onBlur={() => commit(draft)}
      />
    </div>
  )
}
