import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Send } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PasswordInput } from '@/components/ui/password-input'
import { Segmented } from '@/components/ui/segmented'
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

const EMPTY: Smtp = {
  host: '',
  port: 0,
  security: 'starttls',
  username: '',
  password: '',
  from_address: '',
  from_name: '',
  reply_to: '',
}

const DEFAULT_PORT: Record<Security, number> = { starttls: 587, tls: 465, none: 25 }

/// SMTP 邮件出口（settings.smtp，IMPLEMENTATION §11.27）：注册验证码 / 找回密码 / 事件通知
/// 共用。含密码，故设置列表只回"已配置"，本卡用单键接口回显（写权限门槛）。
/// "发送测试邮件"用的是**已保存**的配置——先保存再测，按钮上写明了。
export function SmtpCard() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<Smtp | null>(null)
  const [testTo, setTestTo] = useState('')
  const current = useQuery({
    queryKey: qk.setting('smtp'),
    queryFn: () => apiFetch<{ value: Partial<Smtp> | null }>('/admin/settings/smtp'),
  })
  // reply_to 后端存 null 表示"无"；表单里用空串
  const saved: Smtp = { ...EMPTY, ...current.data?.value, reply_to: current.data?.value?.reply_to ?? '' }
  const form: Smtp = draft ?? saved
  const patch = (next: Partial<Smtp>) => setDraft({ ...form, ...next })
  const configured = saved.host.trim() !== '' && saved.from_address.trim() !== ''

  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: {
          key: 'smtp',
          value: {
            ...form,
            host: form.host.trim(),
            username: form.username.trim(),
            from_address: form.from_address.trim(),
            from_name: form.from_name.trim(),
            reply_to: form.reply_to.trim() === '' ? null : form.reply_to.trim(),
          },
        },
      }),
    onSuccess: () => {
      toast.success(t('admin:smtpSaved'))
      setDraft(null)
      void current.refetch()
      void queryClient.invalidateQueries({ queryKey: qk.adminSettings })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const test = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings/smtp/test', {
        method: 'POST',
        body: { to: testTo.trim(), lang: i18n.language },
      }),
    onSuccess: () => toast.success(t('admin:smtpTestSent', { to: testTo.trim() })),
    onError: (err) => toast.error(describeError(err)),
  })

  const securityLabel: Record<Security, string> = {
    starttls: t('admin:smtpSecurityStarttls'),
    tls: t('admin:smtpSecurityTls'),
    none: t('admin:smtpSecurityNone'),
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:smtpTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-5">
        <p className="text-xs text-muted-foreground">{t('admin:smtpHint')}</p>

        <div className="grid grid-cols-1 gap-4 sm:grid-cols-[1fr_8rem]">
          <Field label={t('admin:smtpHost')} htmlFor="smtp-host">
            <Input
              id="smtp-host"
              autoComplete="off"
              placeholder="smtp.example.com"
              value={form.host}
              onChange={(e) => patch({ host: e.target.value })}
            />
          </Field>
          <Field label={t('admin:smtpPort')} htmlFor="smtp-port" hint={t('admin:smtpPortHint', { port: DEFAULT_PORT[form.security] })}>
            <Input
              id="smtp-port"
              inputMode="numeric"
              placeholder={String(DEFAULT_PORT[form.security])}
              value={form.port === 0 ? '' : String(form.port)}
              onChange={(e) => {
                const n = Number(e.target.value)
                patch({ port: Number.isSafeInteger(n) && n > 0 && n <= 65535 ? n : 0 })
              }}
            />
          </Field>
        </div>

        <Field label={t('admin:smtpSecurity')} hint={t('admin:smtpSecurityHint')}>
          <Segmented
            options={SECURITY.map((s) => ({ value: s, label: securityLabel[s] }))}
            value={form.security}
            onChange={(s) => patch({ security: s })}
            size="sm"
            className="self-start"
          />
        </Field>

        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <Field label={t('admin:smtpUsername')} htmlFor="smtp-user" hint={t('admin:smtpUsernameHint')}>
            <Input
              id="smtp-user"
              autoComplete="off"
              value={form.username}
              onChange={(e) => patch({ username: e.target.value })}
            />
          </Field>
          <Field label={t('admin:smtpPassword')} htmlFor="smtp-pass">
            <PasswordInput
              id="smtp-pass"
              autoComplete="new-password"
              value={form.password}
              onChange={(e) => patch({ password: e.target.value })}
            />
          </Field>
        </div>

        <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          <Field label={t('admin:smtpFrom')} htmlFor="smtp-from">
            <Input
              id="smtp-from"
              type="email"
              autoComplete="off"
              placeholder="no-reply@example.com"
              value={form.from_address}
              onChange={(e) => patch({ from_address: e.target.value })}
            />
          </Field>
          <Field label={t('admin:smtpFromName')} htmlFor="smtp-from-name">
            <Input
              id="smtp-from-name"
              autoComplete="off"
              value={form.from_name}
              onChange={(e) => patch({ from_name: e.target.value })}
            />
          </Field>
          <Field label={t('admin:smtpReplyTo')} htmlFor="smtp-reply-to">
            <Input
              id="smtp-reply-to"
              type="email"
              autoComplete="off"
              value={form.reply_to}
              onChange={(e) => patch({ reply_to: e.target.value })}
            />
          </Field>
        </div>

        <div className="flex items-center gap-2">
          <Button
            disabled={draft === null || save.isPending || !form.host.trim() || !form.from_address.trim()}
            onClick={() => save.mutate()}
          >
            {t('common:save')}
          </Button>
          {draft !== null && (
            <Button variant="ghost" onClick={() => setDraft(null)}>
              {t('common:cancel')}
            </Button>
          )}
        </div>

        <div className="flex flex-col gap-2 rounded-md border border-dashed border-border p-3">
          <p className="text-xs text-muted-foreground">
            {configured ? t('admin:smtpTestHint') : t('admin:smtpTestNeedsSave')}
          </p>
          <div className="flex items-end gap-2">
            <Field label={t('admin:smtpTestTo')} htmlFor="smtp-test-to" className="flex-1">
              <Input
                id="smtp-test-to"
                type="email"
                autoComplete="off"
                placeholder="you@example.com"
                value={testTo}
                onChange={(e) => setTestTo(e.target.value)}
              />
            </Field>
            <Button
              variant="outline"
              loading={test.isPending}
              disabled={!configured || draft !== null || !testTo.includes('@')}
              onClick={() => test.mutate()}
            >
              <Send className="h-4 w-4" />
              {t('admin:smtpTest')}
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
