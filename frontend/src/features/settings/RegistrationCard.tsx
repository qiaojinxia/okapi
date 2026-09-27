import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input, Textarea } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { scaledInteger } from './setting-catalog'

const MODES = ['open', 'invite_only', 'closed'] as const
type Mode = (typeof MODES)[number]
const DOMAIN_MODES = ['any', 'allowlist', 'blocklist'] as const
type DomainMode = (typeof DOMAIN_MODES)[number]

interface Policy {
  mode: Mode
  email_domain_mode: DomainMode
  email_domains: string[]
  new_user_credit_micro: number
  invitee_credit_micro: number
  inviter_credit_micro: number
  email_verification: boolean
}
type CreditKey = 'new_user_credit_micro' | 'invitee_credit_micro' | 'inviter_credit_micro'
type CreditInputs = Record<CreditKey, string>
interface Draft { policy: Policy; amounts: CreditInputs; domains: string }

const EMPTY: Policy = {
  mode: 'open',
  email_domain_mode: 'any',
  email_domains: [],
  new_user_credit_micro: 0,
  invitee_credit_micro: 0,
  inviter_credit_micro: 0,
  email_verification: false,
}

// 按整数位拆分，最小单位和大金额都不经浮点除法或科学计数法回显。
function microToUsd(micro: number): string {
  if (micro === 0) return ''
  if (!Number.isSafeInteger(micro)) return String(micro / 1_000_000)
  const digits = Math.abs(micro).toString().padStart(7, '0')
  const fraction = digits.slice(-6).replace(/0+$/, '')
  return `${micro < 0 ? '-' : ''}${digits.slice(0, -6)}${fraction ? `.${fraction}` : ''}`
}
function readCredit(raw: string): { micro: number | null; error?: 'format' | 'range' } {
  const trimmed = raw.trim()
  if (trimmed === '') return { micro: 0 }
  // 允许继续输入 0.，也接受 .29；输入框始终保留原文。
  const text = /^\d+\.$/.test(trimmed) ? trimmed.slice(0, -1) : trimmed.replace(/^\.(?=\d)/, '0.')
  if (!/^\d+(?:\.\d{1,6})?$/.test(text)) return { micro: null, error: 'format' }
  const micro = scaledInteger(text, 6)
  return micro === null ? { micro: null, error: 'range' } : { micro }
}
function creditInputs(policy: Policy): CreditInputs {
  return {
    new_user_credit_micro: microToUsd(policy.new_user_credit_micro),
    invitee_credit_micro: microToUsd(policy.invitee_credit_micro),
    inviter_credit_micro: microToUsd(policy.inviter_credit_micro),
  }
}

/// 注册与风控（settings.registration_policy；new-api 运营设置里 RegisterEnabled / 邮箱域名限制 /
/// QuotaForNewUser / 邀请奖励 四组的合体）。结构化表单：模式是三档枚举、域名是清单、
/// 金额按美元填——JSON 键值页里既发现不了可选项，也校验不了 micro 单位。
export function RegistrationCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<Draft | null>(null)
  const current = useQuery({
    queryKey: qk.setting('registration_policy'),
    queryFn: () =>
      apiFetch<{ value: Partial<Policy> | null }>('/admin/settings/registration_policy'),
    retry: false,
  })
  const form: Policy = draft?.policy ?? { ...EMPTY, ...current.data?.value }
  const amounts = draft?.amounts ?? creditInputs(form)
  const domains = draft?.domains ?? form.email_domains.join('\n')
  const patch = (next: Partial<Policy>) => setDraft({ policy: { ...form, ...next }, amounts, domains })
  const patchAmount = (key: CreditKey, value: string) => setDraft({ policy: form, amounts: { ...amounts, [key]: value }, domains })
  const credits = {
    new_user_credit_micro: readCredit(amounts.new_user_credit_micro),
    invitee_credit_micro: readCredit(amounts.invitee_credit_micro),
    inviter_credit_micro: readCredit(amounts.inviter_credit_micro),
  }
  const invalid = Object.values(credits).some((value) => value.error)

  const save = useMutation({
    mutationFn: (value: Policy) =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: {
          key: 'registration_policy',
          value,
        },
      }),
    onSuccess: (_response, value) => {
      // 先回填已提交的数据再清草稿，避免显示旧值或让后续编辑被异步回读覆盖。
      queryClient.setQueryData(qk.setting('registration_policy'), { value })
      toast.success(t('admin:regSaved'))
      setDraft(null)
      void queryClient.invalidateQueries({ queryKey: qk.adminSettings })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const submit = () => {
    if (draft === null || save.isPending || current.isPending || current.isError) return
    const { new_user_credit_micro, invitee_credit_micro, inviter_credit_micro } = credits
    if (new_user_credit_micro.micro === null || invitee_credit_micro.micro === null || inviter_credit_micro.micro === null) return
    save.mutate({
      ...form,
      new_user_credit_micro: new_user_credit_micro.micro,
      invitee_credit_micro: invitee_credit_micro.micro,
      inviter_credit_micro: inviter_credit_micro.micro,
      email_domains: [...new Set(domains.split(/[,，\s]+/).map((d) => d.trim().toLowerCase()).filter(Boolean))],
    })
  }

  const modeLabel: Record<Mode, string> = {
    open: t('admin:regModeOpen'),
    invite_only: t('admin:regModeInvite'),
    closed: t('admin:regModeClosed'),
  }
  // 显式映射而非拼 t() 键：守卫脚本只认字面量键（与 RouteDiagnosis 同一纪律）
  const modeHint: Record<Mode, string> = {
    open: t('admin:regModeHint_open'),
    invite_only: t('admin:regModeHint_invite_only'),
    closed: t('admin:regModeHint_closed'),
  }
  const domainLabel: Record<DomainMode, string> = {
    any: t('admin:regDomainAny'),
    allowlist: t('admin:regDomainAllow'),
    blocklist: t('admin:regDomainBlock'),
  }
  const money = (
    id: string,
    label: string,
    hint: string,
    key: CreditKey,
  ) => {
    const issue = credits[key].error
    const error = issue === 'format' ? t('admin:regAmountInvalid') : issue === 'range' ? t('admin:regAmountTooLarge') : null
    return <Field label={label} htmlFor={id}>
      <div className="relative">
        <span aria-hidden className="pointer-events-none absolute inset-y-0 left-3 flex items-center text-sm text-muted-foreground">$</span>
        <Input
          id={id}
          className={`h-11 pl-7 tabular-nums md:h-9 ${error ? 'border-destructive' : ''}`}
          inputMode="decimal"
          placeholder="0"
          autoComplete="off"
          aria-invalid={!!error}
          aria-describedby={`${id}-hint reg-credit-hint${error ? ` ${id}-error` : ''}`}
          value={amounts[key]}
          onChange={(e) => patchAmount(key, e.target.value)}
        />
      </div>
      <p id={`${id}-hint`} className="text-xs leading-5 text-muted-foreground">{hint}</p>
      {error && <p id={`${id}-error`} aria-live="polite" className="text-xs leading-5 text-destructive">{error}</p>}
    </Field>
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:regTitle')}</CardTitle>
      </CardHeader>
      <CardContent>
        {current.isPending ? <LoadingState /> : current.isError ? <ErrorState message={describeError(current.error)} onRetry={() => void current.refetch()} /> :
          <form onSubmit={(event) => { event.preventDefault(); submit() }}>
            <fieldset disabled={save.isPending} aria-busy={save.isPending} className="flex min-w-0 flex-col gap-5">
              <p className="text-xs text-muted-foreground">{t('admin:regHint')}</p>

              <div className="grid min-w-0 grid-cols-1 gap-5 md:grid-cols-2">
                <Field label={t('admin:regMode')} hint={modeHint[form.mode]}>
                  <Segmented
                    ariaLabel={t('admin:regMode')}
                    options={MODES.map((m) => ({ value: m, label: modeLabel[m] }))}
                    value={form.mode}
                    onChange={(m) => patch({ mode: m })}
                    size="sm"
                    className="self-start"
                  />
                </Field>

                <Field label={t('admin:regDomain')} className="md:col-start-2 md:row-span-2">
                  <Segmented
                    ariaLabel={t('admin:regDomain')}
                    options={DOMAIN_MODES.map((m) => ({ value: m, label: domainLabel[m] }))}
                    value={form.email_domain_mode}
                    onChange={(m) => patch({ email_domain_mode: m })}
                    size="sm"
                    className="self-start"
                  />
                  {form.email_domain_mode !== 'any' && (
                    <Textarea
                      id="reg-domains"
                      aria-label={t('admin:regDomain')}
                      aria-describedby="reg-domain-hint"
                      rows={3}
                      className="resize-y font-mono text-xs"
                      value={domains}
                      onChange={(event) => setDraft({ policy: form, amounts, domains: event.target.value })}
                      placeholder={t('admin:regDomainPlaceholder')}
                    />
                  )}
                  <p id="reg-domain-hint" className="text-xs leading-5 text-muted-foreground">{t('admin:regDomainHint')}</p>
                </Field>
                <Switch
                  className="rounded-lg border border-border bg-muted/30 p-3 md:col-start-1"
                  label={t('admin:regEmailVerification')}
                  description={t('admin:regEmailVerificationHint')}
                  checked={form.email_verification}
                  onChange={(on) => patch({ email_verification: on })}
                />
              </div>

              <section aria-labelledby="reg-credit-title" className="space-y-3 border-t border-border pt-4">
                <div className="space-y-1">
                  <h4 id="reg-credit-title" className="text-sm font-medium">{t('admin:regCreditTitle')}</h4>
                  <p id="reg-credit-hint" className="text-xs leading-5 text-muted-foreground">{t('admin:regCreditHint')}</p>
                </div>
                <div className="grid min-w-0 grid-cols-1 gap-4 md:grid-cols-3">
                  {money(
                    'reg-gift',
                    t('admin:regGift'),
                    t('admin:regGiftHint'),
                    'new_user_credit_micro',
                  )}
                  {money(
                    'reg-invitee',
                    t('admin:regInvitee'),
                    t('admin:regInviteeHint'),
                    'invitee_credit_micro',
                  )}
                  {money(
                    'reg-inviter',
                    t('admin:regInviter'),
                    t('admin:regInviterHint'),
                    'inviter_credit_micro',
                  )}
                </div>
              </section>

              <div className="flex flex-wrap items-center gap-2 border-t border-border pt-4">
                <Button type="submit" className="min-h-11 md:min-h-9" disabled={draft === null || invalid} loading={save.isPending}>
                  {t('common:save')}
                </Button>
                {draft !== null && (
                  <Button variant="ghost" className="min-h-11 md:min-h-9" onClick={() => setDraft(null)}>
                    {t('common:cancel')}
                  </Button>
                )}
                {draft !== null && <span className="text-xs text-muted-foreground">{t('admin:regUnsaved')}</span>}
              </div>
            </fieldset>
          </form>}
      </CardContent>
    </Card>
  )
}
