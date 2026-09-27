import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { NoticeMessage } from '@/components/notice-banner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

interface NoticeDraft {
  enabled: boolean
  title: string
  body: string
  level: 'info' | 'warning' | 'critical'
  updated_at?: string
}

const EMPTY: NoticeDraft = { enabled: false, title: '', body: '', level: 'info' }
const EDITABLE = ['enabled', 'title', 'body', 'level'] as const

function readNotice(value: unknown): NoticeDraft | null {
  if (value === null) return { ...EMPTY }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null
  const raw = value as Record<string, unknown>
  if (raw.enabled !== undefined && typeof raw.enabled !== 'boolean') return null
  for (const key of ['title', 'body', 'updated_at']) if (raw[key] !== undefined && typeof raw[key] !== 'string') return null
  if (raw.level !== undefined && (typeof raw.level !== 'string' || !['info', 'warning', 'critical'].includes(raw.level))) return null
  // 高级设置可能附有其他字段；结构化编辑只改自己负责的字段。
  return { ...EMPTY, ...raw } as NoticeDraft
}

export function NoticeCard() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<NoticeDraft | null>(null)
  const [dismissedPreview, setDismissedPreview] = useState<string | null>(null)
  const previewContent = useRef<HTMLDivElement>(null)
  const movePreviewFocus = useRef(false)
  const current = useQuery({
    queryKey: qk.setting('site_notice'),
    queryFn: () => apiFetch<{ value: unknown }>('/admin/settings/site_notice'),
    retry: false,
  })
  const loaded = readNotice(current.data?.value)
  const form = draft ?? loaded ?? EMPTY
  const dirty = loaded !== null && EDITABLE.some((key) => form[key] !== loaded[key])
  const published = !!loaded?.enabled && !!loaded.body.trim()
  // 与公开接口的 Rust chars() 对齐，emoji 不按两个 UTF-16 单元计数。
  const titleCount = Array.from(form.title).length
  const bodyCount = Array.from(form.body).length
  const titleError = titleCount > 80 ? t('admin:noticeTooLong', { max: 80 }) : null
  const bodyError = bodyCount > 4000 ? t('admin:noticeTooLong', { max: 4000 })
    : form.enabled && !form.body.trim() ? t('admin:noticeBodyRequired') : null
  const preview = { title: form.title.trim(), body: form.body.trim(), level: form.level }
  const previewKey = JSON.stringify(preview)
  useLayoutEffect(() => {
    if (!movePreviewFocus.current) return
    movePreviewFocus.current = false
    const content = previewContent.current
    ;(content?.querySelector('button') ?? content)?.focus({ preventScroll: true })
  }, [dismissedPreview, previewKey])
  const updated = loaded?.updated_at ? new Date(loaded.updated_at) : null
  const savedAt = updated && Number.isFinite(updated.getTime()) ? updated.toLocaleString(i18n.language) : null

  const save = useMutation({
    mutationFn: (value: NoticeDraft) => apiFetch('/admin/settings', { method: 'POST', body: { key: 'site_notice', value } }),
    onSuccess: async (_response, value) => {
      await queryClient.cancelQueries({ queryKey: qk.setting('site_notice'), exact: true })
      queryClient.setQueryData(qk.setting('site_notice'), { value })
      setDraft(null)
      setDismissedPreview(null)
      toast.success(t(value.enabled ? 'admin:noticeSaved' : 'admin:noticeSavedHidden'))
      void queryClient.invalidateQueries({ queryKey: qk.adminSettings })
      void queryClient.invalidateQueries({ queryKey: qk.notice })
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const patch = (next: Partial<NoticeDraft>) => setDraft((previous) => ({ ...(previous ?? loaded ?? EMPTY), ...next }))
  const submit = () => {
    if (!loaded || !dirty || save.isPending || current.isPending || current.isError || titleError || bodyError) return
    save.mutate({ ...form, title: preview.title, body: preview.body, updated_at: new Date().toISOString() })
  }

  return <Card className="min-w-0">
    <CardHeader className="gap-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <CardTitle>{t('admin:noticeTitle')}</CardTitle>
        {!current.isPending && !current.isError && loaded && <Badge variant={dirty ? 'warning' : published ? 'success' : 'muted'}>
          {t(dirty ? 'admin:regUnsaved' : published ? 'admin:noticePublished' : 'admin:noticeHidden')}
        </Badge>}
      </div>
      <p className="text-xs leading-5 text-muted-foreground">{t('admin:noticeHint')}</p>
    </CardHeader>
    <CardContent>
      {current.isPending ? <LoadingState /> : current.isError || !loaded ? <ErrorState message={current.isError ? describeError(current.error) : t('admin:noticeInvalidConfig')} onRetry={() => void current.refetch()} /> :
        <div className="grid min-w-0 items-start gap-6 lg:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)]">
          <form onSubmit={(event) => { event.preventDefault(); submit() }}>
            <fieldset disabled={save.isPending} aria-busy={save.isPending} className="flex min-w-0 flex-col gap-4">
              <Switch checked={form.enabled} onChange={(enabled) => patch({ enabled })}
                label={t('admin:noticeEnabled')} description={t('admin:noticeEnabledHint')}
                className="rounded-lg border border-border bg-muted/30 p-3" />
              <div className="grid min-w-0 gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
                <Field label={t('admin:noticeField_title')} htmlFor="notice-title">
                  <Input id="notice-title" className="h-11 md:h-9" value={form.title} onChange={(event) => patch({ title: event.target.value })}
                    aria-invalid={!!titleError} aria-describedby={`notice-title-count${titleError ? ' notice-title-error' : ''}`} />
                  <p id="notice-title-count" className="text-xs tabular-nums text-muted-foreground">{t('admin:noticeCharacters', { count: titleCount, max: 80 })}</p>
                  {titleError && <p id="notice-title-error" className="text-xs text-destructive">{titleError}</p>}
                </Field>
                <Field label={t('admin:noticeField_level')} htmlFor="notice-level">
                  <Select id="notice-level" className="[&>select]:h-11 md:[&>select]:h-9" value={form.level} onChange={(level) => patch({ level: level as NoticeDraft['level'] })}
                    options={[
                      { value: 'info', label: t('admin:noticeLevel_info') },
                      { value: 'warning', label: t('admin:noticeLevel_warning') },
                      { value: 'critical', label: t('admin:noticeLevel_critical') },
                    ]} />
                </Field>
              </div>
              <Field label={t('admin:noticeField_body')} htmlFor="notice-body" required={form.enabled}>
                <Textarea id="notice-body" rows={7} className="resize-y" value={form.body} onChange={(event) => patch({ body: event.target.value })}
                  aria-required={form.enabled} aria-invalid={!!bodyError} aria-describedby={`notice-body-hint notice-body-count${bodyError ? ' notice-body-error' : ''}`} />
                <div className="flex flex-wrap items-start justify-between gap-x-3 gap-y-1 text-xs leading-5 text-muted-foreground">
                  <p id="notice-body-hint">{t('admin:noticeBodyHint')}</p>
                  <p id="notice-body-count" className="shrink-0 tabular-nums">{t('admin:noticeCharacters', { count: bodyCount, max: 4000 })}</p>
                </div>
                {bodyError && <p id="notice-body-error" className="text-xs text-destructive">{bodyError}</p>}
              </Field>
              <div className="space-y-2 border-t border-border pt-4">
                <div className="flex flex-wrap items-center gap-2">
                  <Button type="submit" className="min-h-11 md:min-h-9" disabled={!dirty || !!titleError || !!bodyError} loading={save.isPending}>
                    {t(form.enabled ? 'admin:noticePublish' : published ? 'admin:noticeUnpublish' : 'common:save')}
                  </Button>
                  {draft !== null && <Button variant="ghost" className="min-h-11 md:min-h-9" onClick={() => { setDraft(null); setDismissedPreview(null) }}>{t('common:cancel')}</Button>}
                </div>
                <p className="text-xs leading-5 text-muted-foreground">{t(form.enabled ? 'admin:noticePublishHint' : 'admin:noticeHiddenHint')}</p>
                {savedAt && <p className="text-xs text-muted-foreground">{t('common:updatedAt', { time: savedAt })}</p>}
              </div>
            </fieldset>
          </form>
          <section aria-label={t('admin:noticePreview')} className="min-w-0 space-y-3 rounded-xl border border-border bg-muted/25 p-3 lg:sticky lg:top-20">
            <div className="space-y-1">
              <h4 className="text-sm font-medium">{t('admin:noticePreview')}</h4>
              <p className="text-xs leading-5 text-muted-foreground">{t('admin:noticePreviewHint')}</p>
            </div>
            <div ref={previewContent} tabIndex={preview.body ? 0 : undefined} role="group" aria-label={t('admin:noticePreviewContent')} className="max-h-80 min-w-0 overflow-y-auto overscroll-contain rounded-lg outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
              {!preview.body ? <p className="rounded-lg border border-dashed border-border p-4 text-xs leading-6 text-muted-foreground">{t('admin:noticePreviewEmpty')}</p>
                : dismissedPreview === previewKey && preview.level !== 'critical' ? <div className="flex flex-wrap items-center gap-2 p-3">
                  <span className="text-xs text-muted-foreground">{t('admin:noticePreviewDismissed')}</span>
                  <Button size="sm" variant="outline" className="min-h-11 md:min-h-9" onClick={() => { movePreviewFocus.current = true; setDismissedPreview(null) }}>{t('admin:noticePreviewRestore')}</Button>
                </div> : <NoticeMessage notice={preview} announce={false} onDismiss={() => { movePreviewFocus.current = true; setDismissedPreview(previewKey) }} />}
            </div>
            <p className="text-xs leading-5 text-muted-foreground">{t(form.enabled ? 'admin:noticePreviewEnabled' : 'admin:noticeHiddenHint')}</p>
            <p className="text-xs leading-5 text-muted-foreground">{t(form.level === 'critical' ? 'admin:noticeCriticalHint' : 'admin:noticeDismissHint')}</p>
          </section>
        </div>}
    </CardContent>
  </Card>
}
