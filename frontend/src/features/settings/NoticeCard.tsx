import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Megaphone, Pencil, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { NoticeMessage } from '@/components/notice-center'
import type { NoticeLevel } from '@/components/notice-center'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { useConfirm } from '@/components/ui/confirm'
import { Drawer } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { ActionGroup, IconButton } from '@/components/ui/icon-button'
import { Input, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { commitSetting, invalidateSettingViews } from '@/lib/invalidate'
import { qk } from '@/lib/query-keys'

const KEY = 'site_notices'
/// 与后端一致（crates/okapi-console/src/notices.rs）：最多 50 条，标题 80 字、正文 4000 字
const MAX_NOTICES = 50
const MAX_TITLE = 80
const MAX_BODY = 4000
const LEVELS: readonly NoticeLevel[] = ['info', 'warning', 'critical']

/// 存在 settings.site_notices 里的一条。高级设置里手加的其它字段原样留着。
interface StoredNotice {
  id: string
  title: string
  body: string
  level: NoticeLevel
  popup: boolean
  enabled: boolean
  updated_at: string
  [extra: string]: unknown
}

function readNotices(value: unknown): StoredNotice[] | null {
  if (value === null || value === undefined) return []
  if (!Array.isArray(value)) return null
  const list: StoredNotice[] = []
  for (const item of value as unknown[]) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) return null
    const raw = item as Record<string, unknown>
    if (typeof raw.id !== 'string' || raw.id === '' || typeof raw.updated_at !== 'string') return null
    for (const key of ['title', 'body']) if (raw[key] !== undefined && typeof raw[key] !== 'string') return null
    for (const key of ['popup', 'enabled']) if (raw[key] !== undefined && typeof raw[key] !== 'boolean') return null
    if (raw.level !== undefined && !LEVELS.includes(raw.level as NoticeLevel)) return null
    list.push({ title: '', body: '', level: 'info', popup: false, enabled: false, ...raw } as StoredNotice)
  }
  return list
}

/// `crypto.randomUUID` 只在安全上下文里有（站点按 IP 走 http 时没有），`getRandomValues` 到处都有。
function newNoticeId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(6))
  return `n-${Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')}`
}

const published = (notice: StoredNotice) => notice.enabled && notice.body.trim() !== ''

/// 站点公告：一个列表。每条可以单独发布 / 下线，可以设成弹窗；用户那边是顶栏的公告入口，没看过的亮红点。
export function NoticeCard() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()
  /// 正在编辑的那条；`'new'` 是新建
  const [editing, setEditing] = useState<StoredNotice | 'new' | null>(null)
  const current = useQuery({
    queryKey: qk.setting(KEY),
    queryFn: () => apiFetch<{ value: unknown }>(`/admin/settings/${KEY}`),
    retry: false,
  })
  const loaded = current.isSuccess ? readNotices(current.data.value) : null
  // 新的在前，和用户看到的顺序一致
  const notices = [...(loaded ?? [])].sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
  const save = useMutation({
    mutationFn: ({ next }: { next: StoredNotice[]; done: string }) =>
      apiFetch('/admin/settings', { method: 'POST', body: { key: KEY, value: next } }),
    onSuccess: async (_response, { next, done }) => {
      await commitSetting(queryClient, KEY, next)
      setEditing(null)
      toast.success(done)
      void invalidateSettingViews(queryClient, KEY)
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const time = (at: string) => {
    const date = new Date(at)
    return Number.isFinite(date.getTime()) ? date.toLocaleString(i18n.language, { dateStyle: 'medium', timeStyle: 'short' }) : at
  }
  const submit = (notice: StoredNotice) => {
    if (!loaded) return
    const exists = loaded.some((item) => item.id === notice.id)
    save.mutate({
      next: exists ? loaded.map((item) => (item.id === notice.id ? notice : item)) : [notice, ...loaded],
      done: t(published(notice) ? 'admin:noticeSaved' : 'admin:noticeSavedHidden'),
    })
  }
  const full = (loaded?.length ?? 0) >= MAX_NOTICES

  return <Card className="min-w-0">
    <CardHeader className="gap-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <CardTitle>{t('admin:noticeTitle')}</CardTitle>
          {loaded && loaded.length > 0 && <Badge variant="muted">{t('admin:noticeCount', { published: loaded.filter(published).length, total: loaded.length })}</Badge>}
        </div>
        <Button size="sm" disabled={!loaded || full || save.isPending} onClick={() => setEditing('new')}>
          <Plus aria-hidden className="h-4 w-4" />{t('admin:noticeNew')}
        </Button>
      </div>
      <p className="text-xs leading-5 text-muted-foreground">{t('admin:noticeHint')}</p>
      {full && <p className="text-xs leading-5 text-warning">{t('admin:noticeLimit', { max: MAX_NOTICES })}</p>}
    </CardHeader>
    <CardContent>
      {current.isPending ? <LoadingState />
        : current.isError || !loaded ? <ErrorState message={current.isError ? describeError(current.error) : t('admin:noticeInvalidConfig')} onRetry={() => void current.refetch()} />
          : notices.length === 0 ? <EmptyState icon={Megaphone} title={t('admin:noticeEmpty')} hint={t('admin:noticeEmptyHint')} />
            : <ul aria-label={t('admin:noticeTitle')} className="flex flex-col divide-y divide-border rounded-xl border border-border">
              {notices.map((notice) => {
                const name = notice.title.trim() || t('common:noticeUntitled')
                return <li key={notice.id} data-slot="notice-row" className="flex min-w-0 items-start gap-3 p-3 sm:items-center">
                  <div className="flex min-w-0 flex-1 flex-col gap-1">
                    <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
                      <span className="min-w-0 truncate text-sm font-medium">{name}</span>
                      <Badge variant={published(notice) ? 'success' : 'muted'}>{t(published(notice) ? 'admin:noticePublished' : 'admin:noticeHidden')}</Badge>
                      <Badge variant={notice.level === 'critical' ? 'destructive' : notice.level === 'warning' ? 'warning' : 'outline'}>{t(`admin:noticeLevel_${notice.level}`)}</Badge>
                      {notice.popup && <Badge variant="outline">{t('admin:noticePopupBadge')}</Badge>}
                    </div>
                    <p className="truncate text-xs leading-5 text-muted-foreground">{notice.body.trim() || t('admin:noticeNoBody')}</p>
                    <p className="text-[11px] text-muted-foreground/80 tabular-nums">{t('common:updatedAt', { time: time(notice.updated_at) })}</p>
                  </div>
                  <ActionGroup label={t('common:actions')}>
                    <IconButton icon={Pencil} label={t('common:edit')} disabled={save.isPending} onClick={() => setEditing(notice)} />
                    <IconButton icon={Trash2} label={t('common:delete')} variant="destructive" disabled={save.isPending}
                      onClick={() => confirm({
                        title: t('admin:noticeDeleteTitle', { name }),
                        description: t('admin:noticeDeleteHint'),
                        onConfirm: () => save.mutate({ next: loaded.filter((item) => item.id !== notice.id), done: t('admin:noticeDeleted') }),
                      })} />
                  </ActionGroup>
                </li>
              })}
            </ul>}
    </CardContent>
    {dialog}
    {editing !== null && <NoticeEditor key={editing === 'new' ? 'new' : editing.id} notice={editing === 'new' ? null : editing}
      saving={save.isPending} onSubmit={submit} onClose={() => setEditing(null)} />}
  </Card>
}

/// 新建 / 编辑一条公告。改了标题、正文、级别，或者从未发布变成发布，才换新的 updated_at——
/// 用户那边按它记「看过哪一版」，只是改个弹窗开关不该让看过的人再被提醒一次。
function NoticeEditor({ notice, saving, onSubmit, onClose }: {
  notice: StoredNotice | null; saving: boolean; onSubmit: (notice: StoredNotice) => void; onClose: () => void
}) {
  const { t } = useTranslation()
  const [form, setForm] = useState(() => ({
    title: notice?.title ?? '', body: notice?.body ?? '', level: notice?.level ?? ('info' as NoticeLevel),
    popup: notice?.popup ?? false, enabled: notice?.enabled ?? true,
  }))
  const patch = (next: Partial<typeof form>) => setForm((previous) => ({ ...previous, ...next }))
  // 与后端的 chars() 对齐：emoji 不按两个 UTF-16 单元计数
  const titleCount = Array.from(form.title).length
  const bodyCount = Array.from(form.body).length
  const titleError = titleCount > MAX_TITLE ? t('admin:noticeTooLong', { max: MAX_TITLE }) : null
  const bodyError = bodyCount > MAX_BODY ? t('admin:noticeTooLong', { max: MAX_BODY })
    : form.enabled && !form.body.trim() ? t('admin:noticeBodyRequired') : null
  const content = { title: form.title.trim(), body: form.body.trim(), level: form.level }
  const dirty = !notice || content.title !== notice.title || content.body !== notice.body || content.level !== notice.level
    || form.popup !== notice.popup || form.enabled !== notice.enabled
  const renotify = !notice || content.title !== notice.title || content.body !== notice.body || content.level !== notice.level
    || (form.enabled && !notice.enabled)
  const submit = () => {
    if (!dirty || saving || titleError || bodyError) return
    onSubmit({ ...notice, id: notice?.id ?? newNoticeId(), ...content, popup: form.popup, enabled: form.enabled,
      updated_at: notice && !renotify ? notice.updated_at : new Date().toISOString() })
  }
  return (
    <Drawer open onClose={onClose} title={t(notice ? 'admin:noticeEdit' : 'admin:noticeNew')} description={t('admin:noticeEditorHint')}
      footer={<>
        <Button variant="ghost" onClick={onClose}>{t('common:cancel')}</Button>
        <Button className="px-5" loading={saving} disabled={!dirty || saving || !!titleError || !!bodyError} onClick={submit}>
          {t(form.enabled ? 'admin:noticePublish' : 'common:save')}
        </Button>
      </>}>
      <form className="flex min-w-0 flex-col gap-4" onSubmit={(event) => { event.preventDefault(); submit() }}>
        <fieldset disabled={saving} aria-busy={saving} className="flex min-w-0 flex-col gap-4">
          <div className="grid min-w-0 gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
            <Field label={t('admin:noticeField_title')} htmlFor="notice-title">
              <Input id="notice-title" className="h-11 md:h-9" value={form.title} onChange={(event) => patch({ title: event.target.value })}
                aria-invalid={!!titleError} aria-describedby={`notice-title-count${titleError ? ' notice-title-error' : ''}`} />
              <p id="notice-title-count" className="text-xs tabular-nums text-muted-foreground">{t('admin:noticeCharacters', { count: titleCount, max: MAX_TITLE })}</p>
              {titleError && <p id="notice-title-error" className="text-xs text-destructive">{titleError}</p>}
            </Field>
            <Field label={t('admin:noticeField_level')} htmlFor="notice-level">
              <Select id="notice-level" className="[&>select]:h-11 md:[&>select]:h-9" value={form.level} onChange={(level) => patch({ level: level as NoticeLevel })}
                options={LEVELS.map((level) => ({ value: level, label: t(`admin:noticeLevel_${level}`) }))} />
            </Field>
          </div>
          <Field label={t('admin:noticeField_body')} htmlFor="notice-body" required={form.enabled}>
            <Textarea id="notice-body" rows={7} className="resize-y" value={form.body} onChange={(event) => patch({ body: event.target.value })}
              aria-required={form.enabled} aria-invalid={!!bodyError} aria-describedby={`notice-body-hint notice-body-count${bodyError ? ' notice-body-error' : ''}`} />
            <div className="flex flex-wrap items-start justify-between gap-x-3 gap-y-1 text-xs leading-5 text-muted-foreground">
              <p id="notice-body-hint">{t('admin:noticeBodyHint')}</p>
              <p id="notice-body-count" className="shrink-0 tabular-nums">{t('admin:noticeCharacters', { count: bodyCount, max: MAX_BODY })}</p>
            </div>
            {bodyError && <p id="notice-body-error" className="text-xs text-destructive">{bodyError}</p>}
          </Field>
          <Switch checked={form.popup} onChange={(popup) => patch({ popup })}
            label={t('admin:noticePopup')} description={t('admin:noticePopupHint')}
            className="rounded-lg border border-border bg-muted/30 p-3" />
          <Switch checked={form.enabled} onChange={(enabled) => patch({ enabled })}
            label={t('admin:noticeEnabled')} description={t('admin:noticeEnabledHint')}
            className="rounded-lg border border-border bg-muted/30 p-3" />
          {notice && <p role="note" className="text-xs leading-5 text-muted-foreground">{t(renotify ? 'admin:noticeRenotify' : 'admin:noticeNoRenotify')}</p>}
        </fieldset>
        <section aria-label={t('admin:noticePreview')} className="min-w-0 space-y-2 rounded-xl border border-border bg-muted/25 p-3">
          <h4 className="text-sm font-medium">{t('admin:noticePreview')}</h4>
          {content.body ? <NoticeMessage notice={content} />
            : <p className="rounded-lg border border-dashed border-border p-4 text-xs leading-6 text-muted-foreground">{t('admin:noticePreviewEmpty')}</p>}
        </section>
      </form>
    </Drawer>
  )
}
