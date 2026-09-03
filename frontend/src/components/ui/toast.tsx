import { AlertTriangle, CheckCircle2, Info, X, XCircle } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

export type ToastTone = 'success' | 'error' | 'info' | 'warning'

export interface ToastItem {
  id: number
  tone: ToastTone
  title: string
  description?: string
  /// 毫秒；0 = 不自动消失（需要用户处理的错误）。
  duration: number
}

interface ToastInput {
  title: string
  description?: string
  tone?: ToastTone
  duration?: number
}

/// 模块级消息队列：mutation 回调、工具函数里都能直接 `toast.success()`，
/// 不必把 setMsg 一层层传下去。`<Toaster />` 挂在路由根，订阅并渲染。
///
/// 为什么需要：此前每页各自一个 `msg` 状态，渲染成一行 12px 灰字塞在工具栏下方——
/// 保存成功与保存失败长得一样，且看完不会自己消失，下一次操作前得先分辨这是新消息
/// 还是上次遗留的。
const listeners = new Set<(items: ToastItem[]) => void>()
let items: ToastItem[] = []
let seq = 0
const MAX_VISIBLE = 5

function emit() {
  for (const l of listeners) l(items)
}

function push(input: ToastInput): number {
  const tone = input.tone ?? 'info'
  const id = ++seq
  const item: ToastItem = {
    id,
    tone,
    title: input.title,
    description: input.description,
    // 错误多停 2s：读错误码比读"已保存"慢
    duration: input.duration ?? (tone === 'error' ? 6_000 : 3_600),
  }
  items = [...items, item].slice(-MAX_VISIBLE)
  emit()
  return id
}

export function dismissToast(id: number) {
  items = items.filter((t) => t.id !== id)
  emit()
}

export const toast = Object.assign(
  (title: string, opts?: Omit<ToastInput, 'title'>) => push({ title, ...opts }),
  {
    success: (title: string, description?: string) =>
      push({ title, description, tone: 'success' }),
    error: (title: string, description?: string) => push({ title, description, tone: 'error' }),
    info: (title: string, description?: string) => push({ title, description, tone: 'info' }),
    warning: (title: string, description?: string) =>
      push({ title, description, tone: 'warning' }),
  },
)

const TONE = {
  success: { icon: CheckCircle2, cls: 'text-success' },
  error: { icon: XCircle, cls: 'text-destructive' },
  warning: { icon: AlertTriangle, cls: 'text-warning' },
  info: { icon: Info, cls: 'text-primary' },
} as const

export function Toaster() {
  const [list, setList] = useState<ToastItem[]>(items)
  useEffect(() => {
    listeners.add(setList)
    return () => {
      listeners.delete(setList)
    }
  }, [])
  if (list.length === 0) return null
  return (
    <div
      className="pointer-events-none fixed inset-x-0 bottom-0 z-[70] flex flex-col items-center gap-2 p-4 sm:items-end sm:p-6"
      aria-live="polite"
    >
      {list.map((t) => (
        <ToastCard key={t.id} item={t} />
      ))}
    </div>
  )
}

function ToastCard({ item }: { item: ToastItem }) {
  const { t } = useTranslation()
  const [paused, setPaused] = useState(false)
  const Icon = TONE[item.tone].icon

  // 悬停暂停计时：正要读错误详情时消息滑走是最恼人的交互
  useEffect(() => {
    if (item.duration === 0 || paused) return undefined
    const timer = window.setTimeout(() => dismissToast(item.id), item.duration)
    return () => window.clearTimeout(timer)
  }, [item.id, item.duration, paused])

  return (
    <div
      role={item.tone === 'error' ? 'alert' : 'status'}
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      className={cn(
        'pointer-events-auto flex w-full max-w-sm items-start gap-3 rounded-lg border border-border bg-popover p-3.5 pr-2.5 text-sm shadow-popover',
        'animate-toast-in',
      )}
    >
      <Icon className={cn('mt-0.5 h-4 w-4 shrink-0', TONE[item.tone].cls)} />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="font-medium leading-5 break-words">{item.title}</span>
        {item.description !== undefined && (
          <span className="text-xs leading-5 text-muted-foreground break-words">
            {item.description}
          </span>
        )}
      </div>
      <button
        type="button"
        aria-label={t('common:close')}
        className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        onClick={() => dismissToast(item.id)}
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  )
}
