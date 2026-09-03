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
  const { t } = useTranslation()
  const [dismissed, setDismissed] = useState(() => localStorage.getItem(DISMISS_KEY) ?? '')
  const q = useQuery({
    queryKey: qk.notice,
    queryFn: () => apiFetch<{ notice: Notice | null }>('/api/notice'),
    staleTime: 60_000,
    retry: false,
  })
  const n = q.data?.notice
  if (!n || (n.level !== 'critical' && dismissed === n.updated_at)) return null

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
      role={n.level === 'critical' ? 'alert' : 'status'}
      className={cn('flex items-start gap-3 rounded-lg border px-4 py-3 text-sm', tone.wrap, className)}
    >
      <Icon className={cn('mt-0.5 h-4 w-4 shrink-0', tone.iconCls)} />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        {n.title && <span className="font-medium">{n.title}</span>}
        {/* 正文保留换行：公告常是几条要点，一坨横排读不下去 */}
        <p className="whitespace-pre-line text-muted-foreground">{n.body}</p>
      </div>
      {n.level !== 'critical' && (
        <button
          type="button"
          aria-label={t('common:close')}
          className="rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
          onClick={() => {
            localStorage.setItem(DISMISS_KEY, n.updated_at)
            setDismissed(n.updated_at)
          }}
        >
          <X className="h-4 w-4" />
        </button>
      )}
    </div>
  )
}
