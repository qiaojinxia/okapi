import { Mail, Plus, Trash2, Webhook } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { toast } from '@/components/ui/toast'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Field } from '@/components/ui/field'
import { EVENT_LABEL, NOTIFY_EVENTS } from '@/features/settings/types'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { Input } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { TagInput } from '@/components/ui/tag-input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

export interface NotifyChannel {
  type: 'webhook' | 'email'
  url?: string
  to?: string[]
  lang?: 'en' | 'zh-CN'
  events: string[]
  min_interval_secs: number
}

interface DraftRow { id: string; channel: NotifyChannel; interval: string }

// worker::notify 的默认静默期为 300 秒；0 也使用默认值。
function readInterval(raw: string): number | null {
  const text = raw.trim()
  if (text === '') return 300
  const value = Number(text)
  return /^\d+$/.test(text) && Number.isSafeInteger(value) ? value : null
}

function readChannels(value: unknown): NotifyChannel[] | null {
  if (value == null) return []
  if (!Array.isArray(value) || !value.every((row) => row && typeof row === 'object'
    && (row.type === 'email' || row.type === 'webhook')
    && Array.isArray(row.events) && row.events.every((event: unknown) => typeof event === 'string')
    && (row.url == null || typeof row.url === 'string')
    && (row.to == null || (Array.isArray(row.to) && row.to.every((to: unknown) => typeof to === 'string'))))) return null
  return value as NotifyChannel[]
}

function validUrl(value: string) {
  try { return ['http:', 'https:'].includes(new URL(value.trim()).protocol) } catch { return false }
}

/// 每路通知独立编辑；本地身份与数组位置分开，删除前一项不会复用另一行的输入草稿。
export function NotifyCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [rows, setRows] = useState<DraftRow[] | null>(null)
  const [attempted, setAttempted] = useState(false)
  const [reset, setReset] = useState(0)
  const sequence = useRef(0)
  const focusAfterDelete = useRef<string | null>(null)
  const formRef = useRef<HTMLFormElement>(null)
  const current = useQuery({
    queryKey: qk.setting('notify_channels'),
    queryFn: () => apiFetch<{ value: unknown }>('/admin/settings/notify_channels'),
    retry: false,
  })
  const loaded = useMemo(() => readChannels(current.data?.value)?.map((channel, index) => ({
    id: `saved-${index}`, channel, interval: String(channel.min_interval_secs ?? 300),
  })) ?? null, [current.data])
  const list = rows ?? loaded ?? []
  useLayoutEffect(() => {
    const id = focusAfterDelete.current
    if (id === null) return
    focusAfterDelete.current = null
    const selector = id === 'add' ? '#notify-add-webhook' : `[data-notify-row="${id}"] input`
    formRef.current?.querySelector<HTMLElement>(selector)?.focus()
  }, [rows])
  const patch = (id: string, next: Partial<NotifyChannel>) => setRows((previous) => (previous ?? loaded ?? []).map((row) => row.id === id ? { ...row, channel: { ...row.channel, ...next } } : row))
  const patchInterval = (id: string, interval: string) => setRows((previous) => (previous ?? loaded ?? []).map((row) => row.id === id ? { ...row, interval } : row))
  const cancel = () => { setRows(null); setAttempted(false); setReset((value) => value + 1) }
  const remove = (id: string) => {
    const index = list.findIndex((row) => row.id === id)
    const remaining = list.filter((row) => row.id !== id)
    focusAfterDelete.current = remaining[Math.min(index, remaining.length - 1)]?.id ?? 'add'
    setRows((previous) => (previous ?? loaded ?? []).filter((row) => row.id !== id))
  }
  const add = (type: NotifyChannel['type']) => {
    const row: DraftRow = {
      id: `new-${++sequence.current}`,
      interval: '300',
      channel: { type, ...(type === 'email' ? { to: [], lang: 'en' as const } : { url: '' }), events: [...NOTIFY_EVENTS], min_interval_secs: 300 },
    }
    setRows((previous) => [...(previous ?? loaded ?? []), row])
  }

  const save = useMutation({
    mutationFn: (value: NotifyChannel[]) => apiFetch('/admin/settings', { method: 'POST', body: { key: 'notify_channels', value } }),
    onSuccess: async (_response, value) => {
      // 保存结果先进入缓存，再清理草稿；旧的后台读取不能把已保存的新值改回去。
      await queryClient.cancelQueries({ queryKey: qk.setting('notify_channels'), exact: true })
      queryClient.setQueryData(qk.setting('notify_channels'), { value })
      cancel()
      toast.success(t('admin:saved'))
      void queryClient.invalidateQueries({ queryKey: qk.adminSettings })
    },
    onError: (error) => toast.error(describeError(error)),
  })
  const problems = list.map(({ channel, interval }) => ({
    destination: channel.type === 'webhook'
      ? validUrl(channel.url ?? '') ? null : t('admin:notifyUrlInvalid')
      : (channel.to?.length ?? 0) > 0 && channel.to!.every((to) => /^[^\s@<>]+@[^\s@<>]+$/.test(to)) ? null : t('admin:notifyRecipientsInvalid'),
    interval: readInterval(interval) === null ? t('admin:notifyIntervalInvalid') : null,
  }))
  const submit = () => {
    if (save.isPending || current.isPending || current.isError || loaded === null) return
    setAttempted(true)
    const first = problems.findIndex((problem) => problem.destination || problem.interval)
    if (first >= 0) {
      const prefix = problems[first]?.destination ? list[first]?.channel.type === 'email' ? 'nto' : 'nurl' : 'nint'
      formRef.current?.querySelector<HTMLInputElement>(`#${prefix}-${first}`)?.focus()
      return
    }
    save.mutate(list.map(({ channel, interval }) => ({
      ...channel,
      ...(channel.type === 'webhook' ? { url: channel.url!.trim() } : {}),
      min_interval_secs: readInterval(interval)!,
    })))
  }

  return <Card className="min-w-0">
    <CardHeader className="flex-row flex-wrap items-center justify-between gap-2">
      <CardTitle>{t('admin:notify')}</CardTitle>
      {current.isSuccess && loaded !== null && <Badge variant="outline">{t('common:resultCount', { n: list.length })}</Badge>}
    </CardHeader>
    <CardContent className="px-4 sm:px-5">
      {current.isPending ? <LoadingState /> : current.isError ? <ErrorState message={describeError(current.error)} onRetry={() => void current.refetch()} />
        : loaded === null ? <ErrorState message={t('admin:notifyConfigInvalid')} onRetry={() => void current.refetch()} /> :
          <form ref={formRef} noValidate onSubmit={(event) => { event.preventDefault(); submit() }}>
            <fieldset disabled={save.isPending} aria-busy={save.isPending} className="flex min-w-0 flex-col gap-4">
              <p className="text-xs leading-5 text-muted-foreground">{t('admin:notifyHint')}</p>
              {list.length === 0 && <EmptyState hint={t('admin:notifyEmptyHint')} />}
              {list.map(({ id, channel: row, interval }, i) => {
                const email = row.type === 'email'
                const Icon = email ? Mail : Webhook
                const destinationError = attempted ? problems[i]?.destination : null
                const intervalError = problems[i]?.interval
                return <section key={id} data-notify-row={id} aria-labelledby={`notify-row-${id}`} className="min-w-0 overflow-hidden rounded-xl border border-border">
                  <div className="flex items-center justify-between gap-2 border-b border-border/60 bg-muted/30 px-3 py-2">
                    <h4 id={`notify-row-${id}`} className="flex min-w-0 items-center gap-2 text-sm font-medium"><Icon aria-hidden className="h-4 w-4 shrink-0 text-primary" />{t(email ? 'admin:notifyTypeEmail' : 'admin:notifyTypeWebhook')} {i + 1}</h4>
                    <IconButton icon={Trash2} label={t('common:delete')} variant="destructive" className="h-11 w-11 md:h-8 md:w-8" onClick={() => remove(id)} />
                  </div>
                  <div className="flex min-w-0 flex-col gap-4 p-3 sm:p-4">
                    <div className="grid min-w-0 gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(13rem,0.45fr)]">
                      <Field label={t(email ? 'admin:notifyRecipients' : 'admin:notifyUrl')} htmlFor={`${email ? 'nto' : 'nurl'}-${i}`}>
                        {email ? <TagInput key={reset} id={`nto-${i}`} value={row.to ?? []} onChange={(to) => patch(id, { to })} placeholder="ops@example.com" aria-invalid={!!destinationError} aria-describedby={destinationError ? `ndest-error-${i}` : undefined} /> : <Input id={`nurl-${i}`} className="h-11 font-mono text-xs md:h-9" type="url" autoComplete="off" spellCheck={false} value={row.url ?? ''} placeholder="https://hooks.example.com/..." aria-invalid={!!destinationError} aria-describedby={destinationError ? `ndest-error-${i}` : undefined} onChange={(event) => patch(id, { url: event.target.value })} />}
                        {email && <p className="text-xs leading-5 text-muted-foreground">{t('admin:notifyRecipientsHint')}</p>}
                        {destinationError && <p id={`ndest-error-${i}`} role="alert" className="text-xs leading-5 text-destructive">{destinationError}</p>}
                      </Field>
                      <div className="grid min-w-0 gap-4 sm:grid-cols-2 lg:grid-cols-1">
                        <Field label={t('admin:notifyInterval')} htmlFor={`nint-${i}`}>
                          <Input id={`nint-${i}`} inputMode="numeric" className="h-11 tabular-nums md:h-9" value={interval} placeholder="300" aria-invalid={!!intervalError} aria-describedby={`nint-hint-${i}${intervalError ? ` nint-error-${i}` : ''}`} onChange={(event) => patchInterval(id, event.target.value)} />
                          <p id={`nint-hint-${i}`} className="text-xs leading-5 text-muted-foreground">{t('admin:notifyIntervalHint')}</p>
                          {intervalError && <p id={`nint-error-${i}`} role="alert" className="text-xs leading-5 text-destructive">{intervalError}</p>}
                        </Field>
                        {email && <Field label={t('admin:notifyLang')}>
                          <Segmented ariaLabel={t('admin:notifyLang')} size="sm" options={[{ value: 'en', label: t('admin:notifyLangEn') }, { value: 'zh-CN', label: t('admin:notifyLangZh') }]} value={row.lang ?? 'en'} onChange={(lang) => patch(id, { lang })} />
                        </Field>}
                      </div>
                    </div>
                    <fieldset className="min-w-0 border-t border-border/60 pt-2">
                      <legend className="pr-2 text-xs font-medium text-muted-foreground">{t('admin:notifyEvents')}</legend>
                      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
                        {NOTIFY_EVENTS.map((event) => <Checkbox key={event} label={t(EVENT_LABEL[event])} className="min-h-11 rounded-lg bg-muted/30 px-3 py-2" checked={row.events.includes(event)} onChange={(on) => patch(id, { events: on ? [...row.events, event] : row.events.filter((item) => item !== event) })} />)}
                      </div>
                      {row.events.length === 0 && <p className="mt-2 text-xs leading-5 text-muted-foreground">{t('admin:notifyNoEvents')}</p>}
                    </fieldset>
                  </div>
                </section>
              })}
              <div className="flex flex-wrap items-center gap-2">
                <Button id="notify-add-webhook" size="sm" className="h-11 md:h-9" variant="outline" onClick={() => add('webhook')}><Plus className="h-4 w-4" />{t('admin:notifyAdd')}</Button>
                <Button size="sm" className="h-11 md:h-9" variant="outline" onClick={() => add('email')}><Plus className="h-4 w-4" />{t('admin:notifyAddEmail')}</Button>
              </div>
              <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-4">
                <p className="text-xs leading-5 text-muted-foreground">{t('admin:notifySaveHint')}</p>
                <div className="flex items-center gap-2">
                  <Button variant="outline" className="h-11 md:h-9" onClick={cancel}>{t('common:cancel')}</Button>
                  <Button type="submit" className="h-11 md:h-9" loading={save.isPending}>{t('common:save')}</Button>
                </div>
              </div>
            </fieldset>
          </form>}
    </CardContent>
  </Card>
}
