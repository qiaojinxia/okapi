import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { TagInput } from '@/components/ui/tag-input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

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
}

const EMPTY: Policy = {
  mode: 'open',
  email_domain_mode: 'any',
  email_domains: [],
  new_user_credit_micro: 0,
  invitee_credit_micro: 0,
  inviter_credit_micro: 0,
}

/// micro ↔ 美元字符串（表单里填 "1.5"，库里存 1_500_000）；只做展示层换算，不经浮点累加。
function microToUsd(micro: number): string {
  return micro === 0 ? '' : (micro / 1_000_000).toString()
}
function usdToMicro(raw: string): number {
  const n = Number(raw.trim())
  return raw.trim() === '' || !Number.isFinite(n) || n < 0 ? 0 : Math.round(n * 1_000_000)
}

/// 注册与风控（settings.registration_policy；new-api 运营设置里 RegisterEnabled / 邮箱域名限制 /
/// QuotaForNewUser / 邀请奖励 四组的合体）。结构化表单：模式是三档枚举、域名是清单、
/// 金额按美元填——JSON 键值页里既发现不了可选项，也校验不了 micro 单位。
export function RegistrationCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<Policy | null>(null)
  const current = useQuery({
    queryKey: ['setting', 'registration_policy'],
    queryFn: () =>
      apiFetch<{ value: Partial<Policy> | null }>('/admin/settings/registration_policy'),
  })
  const form: Policy = draft ?? { ...EMPTY, ...current.data?.value }
  const patch = (next: Partial<Policy>) => setDraft({ ...form, ...next })

  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: {
          key: 'registration_policy',
          value: {
            ...form,
            email_domains: form.email_domains.map((d) => d.trim().toLowerCase()).filter(Boolean),
          },
        },
      }),
    onSuccess: () => {
      toast.success(t('admin:regSaved'))
      setDraft(null)
      void current.refetch()
      void queryClient.invalidateQueries({ queryKey: ['admin', 'settings'] })
    },
    onError: (err) => toast.error(describeError(err)),
  })

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
    value: number,
    onChange: (micro: number) => void,
  ) => (
    <Field label={label} htmlFor={id} hint={hint}>
      <div className="flex items-center gap-1.5">
        <span className="text-sm text-muted-foreground">$</span>
        <Input
          id={id}
          className="w-28"
          inputMode="decimal"
          placeholder="0"
          value={microToUsd(value)}
          onChange={(e) => onChange(usdToMicro(e.target.value))}
        />
      </div>
    </Field>
  )

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:regTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-5">
        <p className="text-xs text-muted-foreground">{t('admin:regHint')}</p>

        <Field label={t('admin:regMode')} hint={modeHint[form.mode]}>
          <Segmented
            options={MODES.map((m) => ({ value: m, label: modeLabel[m] }))}
            value={form.mode}
            onChange={(m) => patch({ mode: m })}
            size="sm"
            className="self-start"
          />
        </Field>

        <Field label={t('admin:regDomain')} hint={t('admin:regDomainHint')}>
          <Segmented
            options={DOMAIN_MODES.map((m) => ({ value: m, label: domainLabel[m] }))}
            value={form.email_domain_mode}
            onChange={(m) => patch({ email_domain_mode: m })}
            size="sm"
            className="self-start"
          />
          {form.email_domain_mode !== 'any' && (
            <TagInput
              id="reg-domains"
              value={form.email_domains}
              onChange={(v) => patch({ email_domains: v })}
              placeholder={t('admin:regDomainPlaceholder')}
            />
          )}
        </Field>

        <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          {money(
            'reg-gift',
            t('admin:regGift'),
            t('admin:regGiftHint'),
            form.new_user_credit_micro,
            (v) => patch({ new_user_credit_micro: v }),
          )}
          {money(
            'reg-invitee',
            t('admin:regInvitee'),
            t('admin:regInviteeHint'),
            form.invitee_credit_micro,
            (v) => patch({ invitee_credit_micro: v }),
          )}
          {money(
            'reg-inviter',
            t('admin:regInviter'),
            t('admin:regInviterHint'),
            form.inviter_credit_micro,
            (v) => patch({ inviter_credit_micro: v }),
          )}
        </div>

        <div className="flex items-center gap-2">
          <Button disabled={draft === null || save.isPending} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
          {draft !== null && (
            <Button variant="ghost" onClick={() => setDraft(null)}>
              {t('common:cancel')}
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  )
}
