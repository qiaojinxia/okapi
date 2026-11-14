import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, ChevronDown, Info, Megaphone, X } from 'lucide-react'
import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
import { createPortal } from 'react-dom'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Tooltip } from '@/components/ui/tooltip'
import { useModalFocus } from '@/hooks/use-modal-focus'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { readLocal, rememberLocal } from '@/lib/storage'
import { cn } from '@/lib/utils'

export type NoticeLevel = 'info' | 'warning' | 'critical'
export interface Notice {
  id: string
  title: string
  body: string
  level: NoticeLevel
  /// 没看过时要不要弹窗；不弹窗的只在入口上亮红点
  popup: boolean
  /// 排序依据，也是「看过哪一版」的锚点：站长改过内容再发布，看过的人会再次看到未读
  updated_at: string
}

/// 已发布的公告，新的在前。接口不通或形状不对都当成没有公告——公告是附加信息，不能把页面带崩。
export function useNotices(): Notice[] {
  const query = useQuery({
    queryKey: qk.notices,
    queryFn: () => apiFetch<{ data?: unknown }>('/api/notices'),
    staleTime: 60_000,
    retry: false,
  })
  const data: unknown = query.data?.data
  if (!Array.isArray(data)) return EMPTY
  return data.filter((item): item is Notice => typeof item === 'object' && item !== null
    && typeof (item as Notice).id === 'string' && typeof (item as Notice).body === 'string'
    && typeof (item as Notice).updated_at === 'string')
}
const EMPTY: Notice[] = []

// —— 看过哪些：记在这台浏览器上（公告不登录也能看，没有账号可挂）。键是公告 id，值是看过的那一版的 updated_at ——
const READ_KEY = 'okapi.notices.read'
/// 改版前只有一条公告，关掉横幅时记的是它的 updated_at；那一条搬进列表后 updated_at 没变，认这个记录就不会再提醒一次
const LEGACY_DISMISSED_KEY = 'okapi.notice.dismissed'
const LEGACY = '@legacy'
type Marks = Readonly<Record<string, string>>

let marks: Marks | null = null
const listeners = new Set<() => void>()

function loadMarks(): Marks {
  const loaded: Record<string, string> = {}
  try {
    const stored: unknown = JSON.parse(readLocal(READ_KEY) ?? '{}')
    if (stored && typeof stored === 'object' && !Array.isArray(stored)) {
      for (const [id, at] of Object.entries(stored)) if (typeof at === 'string') loaded[id] = at
    }
  } catch {
    // 存的不是 JSON：当没看过
  }
  const legacy = readLocal(LEGACY_DISMISSED_KEY)
  if (legacy) loaded[LEGACY] = legacy
  return loaded
}
const snapshot = () => (marks ??= loadMarks())
function subscribe(listener: () => void) {
  // 另一个标签页里看过了，这边的红点也跟着灭
  const onStorage = (event: StorageEvent) => {
    if (event.key !== null && event.key !== READ_KEY) return
    marks = null
    listener()
  }
  listeners.add(listener)
  window.addEventListener('storage', onStorage)
  return () => {
    listeners.delete(listener)
    window.removeEventListener('storage', onStorage)
  }
}

const isUnread = (notice: Notice, seen: Marks) => seen[notice.id] !== notice.updated_at && seen[LEGACY] !== notice.updated_at

/// 把这几条记成看过。顺手丢掉已经不在列表里的公告的记录，这份记录不会越攒越多。
function markRead(read: Notice[], all: Notice[]) {
  const current = snapshot()
  const next: Record<string, string> = {}
  for (const notice of all) if (current[notice.id] !== undefined) next[notice.id] = current[notice.id]!
  if (current[LEGACY] !== undefined) next[LEGACY] = current[LEGACY]
  for (const notice of read) next[notice.id] = notice.updated_at
  marks = next
  const { [LEGACY]: _legacy, ...stored } = next
  rememberLocal(READ_KEY, JSON.stringify(stored))
  listeners.forEach((listener) => listener())
}

const TONE = {
  info: { wrap: 'border-primary/30 bg-primary/5', icon: Info, iconCls: 'text-primary' },
  warning: { wrap: 'border-warning/40 bg-warning/10', icon: Megaphone, iconCls: 'text-warning' },
  critical: { wrap: 'border-destructive/40 bg-destructive/10', icon: AlertTriangle, iconCls: 'text-destructive' },
} as const
const tone = (level: string) => TONE[level as NoticeLevel] ?? TONE.info

/// 站点公告的入口：顶栏上一个喇叭按钮，有没看过的公告就亮红点；点开是公告列表，逐条展开才算看过。
/// 设了弹窗的公告，没看过时进站先弹出来。没有已发布的公告时入口不显示。
/// 登录页同样有（维护通知对还没登录的人也成立）。
export function NoticeCenter() {
  const { t } = useTranslation()
  const notices = useNotices()
  const seen = useSyncExternalStore(subscribe, snapshot)
  const [open, setOpen] = useState(false)
  const unread = notices.filter((notice) => isUnread(notice, seen))
  if (notices.length === 0) return null
  const label = unread.length > 0 ? t('common:noticesUnread', { count: unread.length }) : t('common:notices')
  return <>
    <Tooltip content={t('common:notices')} side="bottom">
      <Button variant="ghost" size="icon" className="relative h-11 w-11 md:h-9 md:w-9" aria-label={label} onClick={() => setOpen(true)}>
        <Megaphone className="h-4 w-4" />
        {unread.length > 0 && <span data-slot="notice-dot" aria-hidden className="absolute top-2.5 right-2.5 h-2 w-2 rounded-full bg-destructive ring-2 ring-background md:top-1.5 md:right-1.5" />}
      </Button>
    </Tooltip>
    <Drawer open={open} onClose={() => setOpen(false)} title={t('common:notices')}
      description={unread.length > 0 ? t('common:noticesUnreadHint', { count: unread.length }) : t('common:noticesAllRead')}
      footer={unread.length > 0 ? <Button variant="outline" onClick={() => markRead(unread, notices)}>{t('common:noticeMarkAllRead')}</Button> : undefined}>
      <ul className="flex flex-col gap-3">
        {notices.map((notice) => <li key={notice.id}>
          <NoticeItem notice={notice} unread={isUnread(notice, seen)} onOpen={() => markRead([notice], notices)} />
        </li>)}
      </ul>
    </Drawer>
    {/* 列表开着的时候不叠弹窗：正在看的人不需要再被打断 */}
    {!open && <NoticePopup pending={unread.filter((notice) => notice.popup)} onRead={(notice) => markRead([notice], notices)} />}
  </>
}

function useNoticeTime() {
  const { i18n } = useTranslation()
  return (at: string) => {
    const date = new Date(at)
    return Number.isFinite(date.getTime()) ? date.toLocaleString(i18n.language, { dateStyle: 'medium', timeStyle: 'short' }) : ''
  }
}

/// 列表里的一条：收起时是标题、时间和两行摘要；点开看全文，点开过才算看过（红点随之消失）。
function NoticeItem({ notice, unread, onOpen }: { notice: Notice; unread: boolean; onOpen: () => void }) {
  const { t } = useTranslation()
  const time = useNoticeTime()
  const [open, setOpen] = useState(false)
  const look = tone(notice.level)
  const Icon = look.icon
  return (
    <article data-slot="notice-item" data-unread={unread || undefined} className={cn('rounded-xl border', look.wrap)}>
      <button type="button" aria-expanded={open} className="flex w-full items-start gap-3 rounded-xl p-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
        onClick={() => {
          setOpen(!open)
          if (!open && unread) onOpen()
        }}>
        <Icon aria-hidden className={cn('mt-0.5 h-4 w-4 shrink-0', look.iconCls)} />
        <span className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="flex items-center gap-2">
            <span className={cn('min-w-0 text-sm [overflow-wrap:anywhere]', unread ? 'font-semibold' : 'font-medium')}>{notice.title || t('common:noticeUntitled')}</span>
            {unread && <span data-slot="notice-unread" role="img" aria-label={t('common:noticeUnread')} className="h-2 w-2 shrink-0 rounded-full bg-destructive" />}
          </span>
          <span className="text-xs text-muted-foreground tabular-nums">{time(notice.updated_at)}</span>
          {!open && <span className="line-clamp-2 text-xs leading-5 text-muted-foreground [overflow-wrap:anywhere]">{notice.body}</span>}
        </span>
        <ChevronDown aria-hidden className={cn('mt-0.5 h-4 w-4 shrink-0 text-muted-foreground transition-transform', open && 'rotate-180')} />
      </button>
      {/* 正文保留换行：公告常是几条要点，一坨横排读不下去 */}
      {open && <p className="px-3 pb-3 pl-10 text-sm leading-6 whitespace-pre-line [overflow-wrap:anywhere]">{notice.body}</p>}
    </article>
  )
}

const popupButton = (root: HTMLElement) => root.querySelector<HTMLElement>('button[data-primary]')

/// 弹窗：没看过、又设了弹窗的公告一条一条弹，关掉（按钮、Esc、点遮罩都算）就记成看过，接着弹下一条。
function NoticePopup({ pending, onRead }: { pending: Notice[]; onRead: (notice: Notice) => void }) {
  const { t } = useTranslation()
  const time = useNoticeTime()
  const panel = useRef<HTMLDivElement>(null)
  const notice = pending[0]
  const close = () => {
    if (notice) onRead(notice)
  }
  // 关闭回调每次渲染都换新的公告：用 ref 传给焦点钩子，免得它跟着重新进场
  const closeRef = useRef(close)
  useEffect(() => { closeRef.current = close })
  useModalFocus(notice !== undefined, panel, popupButton, () => closeRef.current())
  if (!notice) return null
  const look = tone(notice.level)
  const Icon = look.icon
  const title = notice.title || t('common:noticeUntitled')
  return createPortal(
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4">
      <button type="button" aria-label={t('common:close')} className="absolute inset-0 bg-black/50 backdrop-blur-[2px] animate-fade-in" onClick={close} />
      <div ref={panel} role="dialog" aria-modal="true" aria-label={title} data-slot="notice-popup"
        className="relative z-10 flex max-h-[min(36rem,calc(100dvh-2rem))] w-full max-w-lg flex-col gap-4 rounded-xl border border-border bg-card p-5 shadow-popover animate-zoom-in">
        <div className="flex items-start gap-3">
          <span className={cn('flex h-9 w-9 shrink-0 items-center justify-center rounded-full border', look.wrap)}>
            <Icon aria-hidden className={cn('h-4 w-4', look.iconCls)} />
          </span>
          <div className="flex min-w-0 flex-1 flex-col gap-0.5 pt-0.5">
            <h2 className="text-[15px] leading-6 font-semibold [overflow-wrap:anywhere]">{title}</h2>
            <p className="text-xs text-muted-foreground tabular-nums">{time(notice.updated_at)}</p>
          </div>
          <Button size="icon" variant="ghost" className="-mt-1 -mr-2 h-10 w-10 shrink-0" aria-label={t('common:close')} onClick={close}>
            <X className="h-4 w-4" />
          </Button>
        </div>
        <p className="min-h-0 overflow-y-auto overscroll-contain text-sm leading-6 whitespace-pre-line [overflow-wrap:anywhere]">{notice.body}</p>
        <div className="flex flex-wrap items-center justify-end gap-3">
          {pending.length > 1 && <span className="mr-auto text-xs text-muted-foreground">{t('common:noticeMorePending', { count: pending.length - 1 })}</span>}
          <Button data-primary className="min-h-11 px-5 sm:min-h-9" onClick={close}>{t('common:noticeGotIt')}</Button>
        </div>
      </div>
    </div>,
    document.body,
  )
}

/// 一条公告的展示（后台编辑时的预览用）：图标、标题和保留换行的正文，颜色随级别。
export function NoticeMessage({ notice, className }: { notice: Pick<Notice, 'title' | 'body' | 'level'>; className?: string }) {
  const look = tone(notice.level)
  const Icon = look.icon
  return (
    <div className={cn('flex min-w-0 items-start gap-3 rounded-lg border px-4 py-3 text-sm', look.wrap, className)}>
      <Icon aria-hidden className={cn('mt-0.5 h-4 w-4 shrink-0', look.iconCls)} />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5 [overflow-wrap:anywhere]">
        {notice.title && <span className="font-medium">{notice.title}</span>}
        <p className="whitespace-pre-line text-muted-foreground">{notice.body}</p>
      </div>
    </div>
  )
}
