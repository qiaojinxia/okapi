import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, Info, Megaphone, X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

export interface Notice {
  title: string
  body: string
  level: 'info' | 'warning' | 'critical'
  updated_at: string
}

const DISMISS_KEY = 'okapi.notice.dismissed'

/// 站点公告横幅（维护通知 / 价格调整预告——中转站最常见的运营触达）。
///
/// 关闭按 `updated_at` 记忆：同一版公告关掉就不再弹，重新发布会再次出现——
/// 用户抱怨"价格变了没通知"时，站长至少能确认通知确实弹过。
/// critical 档不可关闭：停服级通知不该被一次误触永久隐藏。
export function NoticeBanner({ className }: { className?: string }) {
  const [dismissed, setDismissed] = useState(() => localStorage.getItem(DISMISS_KEY) ?? '')
  const q = useQuery({
    queryKey: qk.notice,
    queryFn: () => apiFetch<{ notice: Notice | null }>('/api/notice'),
    staleTime: 60_000,
    retry: false,
  })
  const n = q.data?.notice
  if (!n || (n.level !== 'critical' && dismissed === n.updated_at)) return null

  return <NoticeMessage notice={n} className={className} onDismiss={() => {
    localStorage.setItem(DISMISS_KEY, n.updated_at)
    setDismissed(n.updated_at)
  }} />
}

// 线上横幅与编辑预览共用展示；预览不读接口、不写已读记录，也不逐字播报。
export function NoticeMessage({ notice: n, className, onDismiss, announce = true }: {
  notice: Pick<Notice, 'title' | 'body' | 'level'>
  className?: string
  onDismiss?: () => void
  announce?: boolean
}) {
  const { t } = useTranslation()

  const tone = {
    info: { wrap: 'border-primary/30 bg-primary/5 text-foreground', icon: Info, iconCls: 'text-primary' },
    warning: { wrap: 'border-warning/40 bg-warning/10 text-foreground', icon: Megaphone, iconCls: 'text-warning' },
    critical: {
      wrap: 'border-destructive/40 bg-destructive/10 text-foreground',
      icon: AlertTriangle,
      iconCls: 'text-destructive',
    },
  }[n.level]
  const Icon = tone.icon

  return (
    <div
      role={announce ? n.level === 'critical' ? 'alert' : 'status' : undefined}
      className={cn('grid min-w-0 grid-cols-[1rem_minmax(0,1fr)_auto] items-start gap-x-3 gap-y-1 rounded-lg border px-4 py-3 text-sm md:flex', tone.wrap, className)}
    >
      <Icon aria-hidden className={cn('col-start-1 row-start-1 h-4 w-4 shrink-0 self-center md:mt-0.5 md:self-start', tone.iconCls)} />
      <div className="contents [overflow-wrap:anywhere] md:flex md:min-w-0 md:flex-1 md:flex-col md:gap-0.5">
        {n.title && <span className="col-start-2 row-start-1 min-w-0 self-center font-medium md:self-auto">{n.title}</span>}
        {/* 正文保留换行：公告常是几条要点，一坨横排读不下去 */}
        <p className="col-span-3 row-start-2 min-w-0 whitespace-pre-line text-muted-foreground">{n.body}</p>
      </div>
      {n.level !== 'critical' && onDismiss && (
        <button
          type="button"
          aria-label={t('common:close')}
          className="col-start-3 row-start-1 flex h-11 w-11 shrink-0 items-center justify-center rounded text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 md:h-7 md:w-7"
          onClick={onDismiss}
        >
          <X className="h-4 w-4" />
        </button>
      )}
    </div>
  )
}
