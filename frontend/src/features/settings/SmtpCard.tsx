import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Mail, Send, Server } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PasswordInput } from '@/components/ui/password-input'
import { Segmented } from '@/components/ui/segmented'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

const SECURITY = ['starttls', 'tls', 'none'] as const
type Security = (typeof SECURITY)[number]
interface Smtp {
  host: string
  port: number
  security: Security
  username: string
  password: string
  from_address: string
  from_name: string
  reply_to: string
}
type SavedSmtp = Omit<Smtp, 'reply_to'> & { reply_to: string | null }
interface Draft { value: Smtp; port: string }

const EMPTY: Smtp = {
  host: '', port: 0, security: 'starttls', username: '', password: '',
  from_address: '', from_name: '', reply_to: '',
}
const DEFAULT_PORT: Record<Security, number> = { starttls: 587, tls: 465, none: 25 }
const validEmail = (value: string) => /^[^\s@<>]+@[^\s@<>]+$/.test(value.trim())

function validReplyTo(value: string) {
  const text = value.trim()
  if (text === '' || validEmail(text)) return true
  // 后端 Reply-To 接受带显示名称的 Mailbox；编辑其他字段时不能挡住已有的合法地址。
  const open = text.indexOf('<')
  return open >= 0 && text.endsWith('>') && !/[<>\r\n]/.test(text.slice(0, open)) && validEmail(text.slice(open + 1, -1))
}

function readPort(raw: string): number | null {
  const text = raw.trim()
  if (text === '') return 0
  const value = Number(text)
  return /^\d+$/.test(text) && Number.isInteger(value) && value <= 65535 ? value : null
}

function readSmtp(raw: unknown): Smtp | null {
  if (raw == null) return EMPTY
  if (typeof raw !== 'object' || Array.isArray(raw)) return null
  const value = raw as Record<string, unknown>
  if (['host', 'username', 'password', 'from_address', 'from_name'].some((key) => value[key] !== undefined && typeof value[key] !== 'string')
    || (value.port !== undefined && typeof value.port !== 'number')
    || (value.security !== undefined && !SECURITY.includes(value.security as Security))
    || (value.reply_to != null && typeof value.reply_to !== 'string')) return null
  // 保留扩展字段；reply_to 的 null 只在表单中显示为空串。
  return { ...EMPTY, ...value, reply_to: value.reply_to ?? '' } as Smtp
}

/// 连接、发件身份和验证发送分区；编辑使用原始端口文本，提交时再转换。
export function SmtpCard() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<Draft | null>(null)
  const [testTo, setTestTo] = useState('')
  const current = useQuery({
    queryKey: qk.setting('smtp'),
    queryFn: () => apiFetch<{ value: unknown }>('/admin/settings/smtp'),
    retry: false,
  })
  const saved = readSmtp(current.data?.value)
  const form = draft?.value ?? saved ?? EMPTY
  const portText = draft?.port ?? (form.port === 0 ? '' : String(form.port))
  const port = readPort(portText)
  const patch = (next: Partial<Smtp>) => setDraft({ value: { ...form, ...next }, port: portText })
  const configured = !!saved?.host.trim() && !!saved.from_address.trim()
  const hostFilled = form.host.trim() !== ''
  const fromValid = !hostFilled || validEmail(form.from_address)
  const replyValid = validReplyTo(form.reply_to)
  const dirty = draft !== null
  const ready = current.isSuccess && saved !== null
  const fromError = hostFilled && form.from_address.trim() !== '' && !fromValid
  const replyError = !replyValid

  const save = useMutation({
    mutationFn: (value: SavedSmtp) => apiFetch('/admin/settings', { method: 'POST', body: { key: 'smtp', value } }),
    onSuccess: async (_response, value) => {
      await queryClient.cancelQueries({ queryKey: qk.setting('smtp'), exact: true })
      queryClient.setQueryData(qk.setting('smtp'), { value })
      setDraft(null)
      toast.success(t('admin:smtpSaved'))
      void queryClient.invalidateQueries({ queryKey: qk.adminSettings })
    },
    onError: (error) => toast.error(describeError(error)),
  })
  const test = useMutation({
    mutationFn: (value: { to: string; lang: string }) => apiFetch('/admin/settings/smtp/test', { method: 'POST', body: value }),
    onSuccess: (_response, value) => toast.success(t('admin:smtpTestSent', { to: value.to })),
    onError: (error) => toast.error(describeError(error)),
  })
  const busy = save.isPending || test.isPending
  const canSave = ready && dirty && !busy && port !== null && fromValid && replyValid
  const canTest = ready && configured && !dirty && !busy && port !== null && fromValid && replyValid && validEmail(testTo)
  const submit = () => {
    if (!canSave || port === null) return
    save.mutate({
      ...form, port,
      host: form.host.trim(), username: form.username.trim(),
      from_address: form.from_address.trim(), from_name: form.from_name.trim(),
      reply_to: form.reply_to.trim() || null,
    })
  }
  const securityLabel: Record<Security, string> = {
    starttls: t('admin:smtpSecurityStarttls'), tls: t('admin:smtpSecurityTls'), none: t('admin:smtpSecurityNone'),
  }
  const status = dirty ? t('admin:smtpUnsaved') : configured ? t('admin:smtpConfigured') : t('admin:smtpOff')
  const inputSize = 'h-11 md:h-9'

  return <Card className="min-w-0">
    <CardHeader className="flex-row flex-wrap items-center justify-between gap-2">
      <CardTitle>{t('admin:smtpTitle')}</CardTitle>
      {ready && <Badge variant={dirty ? 'warning' : 'outline'}>{status}</Badge>}
    </CardHeader>
    <CardContent className="px-4 sm:px-5">
      {current.isPending ? <LoadingState /> : current.isError ? <ErrorState message={describeError(current.error)} onRetry={() => void current.refetch()} />
        : saved === null ? <ErrorState message={t('admin:smtpConfigInvalid')} onRetry={() => void current.refetch()} /> :
          <fieldset disabled={busy} aria-busy={busy} className="flex min-w-0 flex-col gap-5">
            <p className="text-xs leading-5 text-muted-foreground">{t('admin:smtpHint')}</p>
            <form noValidate onSubmit={(event) => { event.preventDefault(); submit() }} className="flex min-w-0 flex-col gap-4">
              <div className="grid min-w-0 gap-4 lg:grid-cols-2">
                <section aria-labelledby="smtp-connection-title" className="min-w-0 rounded-xl border border-border">
                  <h4 id="smtp-connection-title" className="flex items-center gap-2 border-b border-border/60 bg-muted/30 px-4 py-3 text-sm font-medium"><Server aria-hidden className="h-4 w-4 text-primary" />{t('admin:smtpConnection')}</h4>
                  <div className="flex min-w-0 flex-col gap-4 p-3 sm:p-4">
                    <div className="grid min-w-0 gap-4 sm:grid-cols-[minmax(0,1fr)_8rem]">
                      <Field label={t('admin:smtpHost')} htmlFor="smtp-host">
                        <Input id="smtp-host" className={inputSize} autoComplete="off" spellCheck={false} placeholder="smtp.example.com" value={form.host} onChange={(event) => patch({ host: event.target.value })} />
                      </Field>
                      <Field label={t('admin:smtpPort')} htmlFor="smtp-port">
                        <Input id="smtp-port" className={`${inputSize} tabular-nums`} inputMode="numeric" placeholder={String(DEFAULT_PORT[form.security])} aria-invalid={port === null} aria-describedby={`smtp-port-hint${port === null ? ' smtp-port-error' : ''}`} value={portText} onChange={(event) => setDraft({ value: form, port: event.target.value })} />
                        <p id="smtp-port-hint" className="text-xs leading-5 text-muted-foreground">{t('admin:smtpPortHint', { port: DEFAULT_PORT[form.security] })}</p>
                        {port === null && <p id="smtp-port-error" role="alert" className="text-xs leading-5 text-destructive">{t('admin:smtpPortInvalid')}</p>}
                      </Field>
                    </div>
                    <Field label={t('admin:smtpSecurity')}>
                      <Segmented ariaLabel={t('admin:smtpSecurity')} options={SECURITY.map((value) => ({ value, label: securityLabel[value] }))} value={form.security} onChange={(security) => patch({ security })} size="sm" className="self-start" />
                      <p className="text-xs leading-5 text-muted-foreground">{t('admin:smtpSecurityHint')}</p>
                    </Field>
                    <div className="grid min-w-0 gap-4 sm:grid-cols-2">
                      <Field label={t('admin:smtpUsername')} htmlFor="smtp-user">
                        <Input id="smtp-user" className={inputSize} autoComplete="off" value={form.username} onChange={(event) => patch({ username: event.target.value })} />
                      </Field>
                      <Field label={t('admin:smtpPassword')} htmlFor="smtp-pass">
                        <PasswordInput id="smtp-pass" className={inputSize} autoComplete="new-password" value={form.password} onChange={(event) => patch({ password: event.target.value })} />
                      </Field>
                    </div>
                    <p className="text-xs leading-5 text-muted-foreground">{t('admin:smtpUsernameHint')}</p>
                  </div>
                </section>
                <section aria-labelledby="smtp-sender-title" className="min-w-0 rounded-xl border border-border">
                  <h4 id="smtp-sender-title" className="flex items-center gap-2 border-b border-border/60 bg-muted/30 px-4 py-3 text-sm font-medium"><Mail aria-hidden className="h-4 w-4 text-primary" />{t('admin:smtpSender')}</h4>
                  <div className="flex min-w-0 flex-col gap-4 p-3 sm:p-4">
                    <Field label={t('admin:smtpFrom')} htmlFor="smtp-from" required={hostFilled}>
                      <Input id="smtp-from" className={inputSize} type="email" autoComplete="off" placeholder="no-reply@example.com" aria-invalid={fromError} aria-describedby={fromError ? 'smtp-from-error' : undefined} value={form.from_address} onChange={(event) => patch({ from_address: event.target.value })} />
                      {fromError && <p id="smtp-from-error" role="alert" className="text-xs leading-5 text-destructive">{t('admin:smtpAddressInvalid')}</p>}
                    </Field>
                    <Field label={t('admin:smtpFromName')} htmlFor="smtp-from-name">
                      <Input id="smtp-from-name" className={inputSize} autoComplete="off" value={form.from_name} onChange={(event) => patch({ from_name: event.target.value })} />
                    </Field>
                    <Field label={t('admin:smtpReplyTo')} htmlFor="smtp-reply-to">
                      <Input id="smtp-reply-to" className={inputSize} type="email" autoComplete="off" aria-invalid={replyError} aria-describedby={replyError ? 'smtp-reply-error' : undefined} value={form.reply_to} onChange={(event) => patch({ reply_to: event.target.value })} />
                      {replyError && <p id="smtp-reply-error" role="alert" className="text-xs leading-5 text-destructive">{t('admin:smtpAddressInvalid')}</p>}
                    </Field>
                    <div role="note" aria-label={t('admin:smtpSenderPreview')} className="rounded-lg bg-muted/40 p-3">
                      <p className="text-xs text-muted-foreground">{t('admin:smtpSenderPreview')}</p>
                      <p className="mt-1 break-words text-sm font-medium">{form.from_name.trim() || form.from_address.trim() || '—'}</p>
                      {form.from_name.trim() && <p className="mt-0.5 break-all font-mono text-xs text-muted-foreground">{form.from_address.trim() || '—'}</p>}
                    </div>
                  </div>
                </section>
              </div>
              <div className="flex flex-wrap items-center justify-between gap-3">
                <p className="text-xs leading-5 text-muted-foreground">{dirty ? t('admin:smtpUnsavedHint') : status}</p>
                <div className="flex gap-2">
                  <Button variant="outline" className={inputSize} disabled={!dirty} onClick={() => setDraft(null)}>{t('common:cancel')}</Button>
                  <Button type="submit" className={inputSize} loading={save.isPending} disabled={!canSave}>{t('common:save')}</Button>
                </div>
              </div>
            </form>
            <section aria-labelledby="smtp-test-title" className="min-w-0 rounded-xl border border-dashed border-border bg-muted/20 p-3 sm:p-4">
              <h4 id="smtp-test-title" className="flex items-center gap-2 text-sm font-medium"><Send aria-hidden className="h-4 w-4 text-primary" />{t('admin:smtpVerify')}</h4>
              <p className="mt-2 text-xs leading-5 text-muted-foreground">{dirty ? t('admin:smtpUnsavedHint') : configured ? t('admin:smtpTestHint') : t('admin:smtpTestNeedsSave')}</p>
              <form noValidate onSubmit={(event) => { event.preventDefault(); if (canTest) test.mutate({ to: testTo.trim(), lang: i18n.language }) }} className="mt-3 grid min-w-0 items-end gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
                <Field label={t('admin:smtpTestTo')} htmlFor="smtp-test-to">
                  <Input id="smtp-test-to" className={inputSize} type="email" autoComplete="off" placeholder="you@example.com" value={testTo} onChange={(event) => setTestTo(event.target.value)} />
                </Field>
                <Button type="submit" className={inputSize} variant="outline" loading={test.isPending} disabled={!canTest}><Send className="h-4 w-4" />{t('admin:smtpTest')}</Button>
              </form>
            </section>
          </fieldset>}
    </CardContent>
  </Card>
}
