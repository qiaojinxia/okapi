import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

// 路径与后端 API 的 /admin/redemptions 区分开：同名可行（console 已按 Accept 做
// 导航协商），但页面是管理视图、API 是"建批次"动作，分开更少歧义。
export const Route = createFileRoute('/admin/codes')({
  component: RedemptionsPage,
})

interface CreateResp {
  batch_id: string
  codes: string[]
}

function RedemptionsPage() {
  return (
    <div className="flex flex-col gap-4">
      <GenerateCard />
      <CodeListCard />
      <PlanCard />
      <PlanListCard />
    </div>
  )
}

interface CodeRow {
  id: number
  batch_id: string
  amount_micro: number
  status: number
  plan_code: string | null
  bind_user_id: number | null
  redeemed_by: number | null
  redeemed_at: string | null
  created_at: string
}

const CODE_STATUS = { unused: 1, used: 2, disabled: 3 } as const

/// 兑换码列表（不含码明文——后端只存 SHA-256，生成时一次性返回）+ 按批次停用。
function CodeListCard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [status, setStatus] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const codes = useQuery({
    queryKey: ['admin', 'redemptions', status],
    queryFn: () => {
      const params = new URLSearchParams({ limit: '100' })
      if (status !== '') params.set('status', status)
      return apiFetch<{ total: number; data: CodeRow[] }>(`/admin/redemptions?${params}`)
    },
  })

  const disableBatch = useMutation({
    mutationFn: (batch: string) =>
      apiFetch<{ affected: number }>(`/admin/redemptions/${batch}`, { method: 'DELETE' }),
    onSuccess: (r) => {
      setMsg(t('admin:batchDisabled', { n: r.affected }))
      void queryClient.invalidateQueries({ queryKey: ['admin', 'redemptions'] })
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const statusLabel = (s: number) => {
    if (s === CODE_STATUS.used) return t('admin:codeUsed')
    if (s === CODE_STATUS.disabled) return t('common:disabled')
    return t('admin:codeUnused')
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:codeListTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="cstatus">{t('common:status')}</Label>
            <select
              id="cstatus"
              className="h-9 rounded-md border border-input bg-card px-2 text-sm"
              value={status}
              onChange={(e) => setStatus(e.target.value)}
            >
              <option value="">{t('admin:codeAll')}</option>
              <option value="1">{t('admin:codeUnused')}</option>
              <option value="2">{t('admin:codeUsed')}</option>
              <option value="3">{t('common:disabled')}</option>
            </select>
          </div>
          <span className="text-xs text-muted-foreground">
            {t('admin:keyTotal', { n: codes.data?.total ?? 0 })}
          </span>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        {codes.isError ? (
          <p className="text-sm text-destructive">{describeError(codes.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>ID</Th>
                <Th>{t('admin:codeBatch')}</Th>
                <Th>{t('common:amount')}</Th>
                <Th>{t('common:status')}</Th>
                <Th>{t('admin:codePlan')}</Th>
                <Th>{t('admin:codeRedeemedBy')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(codes.data?.data ?? []).map((c) => (
                <Tr key={c.id}>
                  <Td>{c.id}</Td>
                  <Td className="font-mono text-xs">{c.batch_id.slice(0, 8)}…</Td>
                  <Td>{formatMoney(c.amount_micro, locale)}</Td>
                  <Td>
                    <Badge variant={c.status === CODE_STATUS.unused ? 'success' : 'muted'}>
                      {statusLabel(c.status)}
                    </Badge>
                  </Td>
                  <Td>{c.plan_code ?? '—'}</Td>
                  <Td>{c.redeemed_by ?? '—'}</Td>
                  <Td>
                    <Button
                      size="sm"
                      variant="destructive"
                      disabled={c.status !== CODE_STATUS.unused}
                      onClick={() => disableBatch.mutate(c.batch_id)}
                    >
                      {t('admin:disableBatch')}
                    </Button>
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}

interface PlanRow {
  id: number
  plan_code: string
  display_name: string
  grant_micro: number
  group_code: string | null
  balance_valid_days: number | null
  status: number
  code_count: number
}

/// 套餐列表 + 删除。被兑换码引用时后端回 409（历史兑换码需保留套餐语义），
/// 故列表把引用数摆出来。
function PlanListCard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)

  const plans = useQuery({
    queryKey: ['admin', 'plans'],
    queryFn: () => apiFetch<{ data: PlanRow[] }>('/admin/plans'),
  })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/plans/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('common:success'))
      void queryClient.invalidateQueries({ queryKey: ['admin', 'plans'] })
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:planListTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        {plans.isError ? (
          <p className="text-sm text-destructive">{describeError(plans.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:planCode')}</Th>
                <Th>{t('admin:planName')}</Th>
                <Th>{t('admin:planGrant')}</Th>
                <Th>{t('portal:group')}</Th>
                <Th>{t('admin:planCodeCount')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(plans.data?.data ?? []).map((p) => (
                <Tr key={p.id}>
                  <Td className="font-mono text-xs">{p.plan_code}</Td>
                  <Td>{p.display_name}</Td>
                  <Td>{formatMoney(p.grant_micro, locale)}</Td>
                  <Td>{p.group_code ?? '—'}</Td>
                  <Td>{p.code_count}</Td>
                  <Td>
                    <Button
                      size="sm"
                      variant="destructive"
                      onClick={() => remove.mutate(p.plan_code)}
                    >
                      {t('common:delete')}
                    </Button>
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}

function GenerateCard() {
  const { t, i18n } = useTranslation()
  const [form, setForm] = useState({
    count: '10',
    amount_usd: '10',
    plan_code: '',
    bind_user_id: '',
    max_per_ip: '',
    expires_at: '',
  })
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<CreateResp | null>(null)
  const [copied, setCopied] = useState(false)

  const amountMicro = Math.round((Number(form.amount_usd) || 0) * 1_000_000)

  const create = useMutation({
    mutationFn: () =>
      apiFetch<CreateResp>('/admin/redemptions', {
        method: 'POST',
        body: {
          count: Number(form.count) || 0,
          amount_micro: amountMicro,
          plan_code: form.plan_code.trim() === '' ? undefined : form.plan_code.trim(),
          bind_user_id:
            form.bind_user_id.trim() === '' ? undefined : Number(form.bind_user_id) || undefined,
          max_per_ip:
            form.max_per_ip.trim() === '' ? undefined : Number(form.max_per_ip) || undefined,
          // datetime-local 无时区，按本地时间转 UTC ISO 交给后端
          expires_at:
            form.expires_at === '' ? undefined : new Date(form.expires_at).toISOString(),
        },
      }),
    onSuccess: (data) => {
      setError(null)
      setCopied(false)
      setResult(data)
    },
    onError: (err) => {
      setResult(null)
      setError(describeError(err))
    },
  })

  const copyAll = () => {
    if (result === null) return
    void navigator.clipboard.writeText(result.codes.join('\n')).then(() => setCopied(true))
  }

  const fields: ReadonlyArray<readonly [keyof typeof form, string]> = [
    ['count', t('admin:redeemCount')],
    ['amount_usd', t('admin:redeemAmount')],
    ['plan_code', t('admin:redeemPlanCode')],
    ['bind_user_id', t('admin:redeemBindUser')],
    ['max_per_ip', t('admin:redeemMaxPerIp')],
  ]

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:redeemTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:redeemHint')}</p>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={field}>{label}</Label>
              <Input
                id={field}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="expires_at">{t('admin:redeemExpiresAt')}</Label>
            <Input
              id="expires_at"
              type="datetime-local"
              value={form.expires_at}
              onChange={(e) => setForm((f) => ({ ...f, expires_at: e.target.value }))}
            />
          </div>
        </div>
        <div className="flex items-center gap-3">
          <Button disabled={create.isPending || amountMicro <= 0} onClick={() => create.mutate()}>
            {t('admin:redeemGenerate')}
          </Button>
          <span className="text-xs text-muted-foreground">
            {t('admin:redeemFaceValue', { amount: formatMoney(amountMicro, i18n.language) })}
          </span>
          {error !== null && <span className="text-xs text-destructive">{error}</span>}
        </div>

        {result !== null && (
          <div className="flex flex-col gap-2 border-t border-border pt-3">
            <div className="flex items-center gap-3">
              <Label>{t('admin:redeemBatch', { id: result.batch_id })}</Label>
              <Button size="sm" variant="outline" onClick={copyAll}>
                {copied ? t('portal:affCopied') : t('admin:redeemCopyAll')}
              </Button>
            </div>
            <p className="text-xs text-destructive">{t('admin:redeemOnceOnly')}</p>
            <Textarea readOnly rows={8} className="font-mono text-xs" value={result.codes.join('\n')} />
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function PlanCard() {
  const { t } = useTranslation()
  const [form, setForm] = useState({
    plan_code: '',
    display_name: '',
    grant_usd: '10',
    group_code: '',
    balance_valid_days: '',
  })
  const [msg, setMsg] = useState<string | null>(null)

  const grantMicro = Math.round((Number(form.grant_usd) || 0) * 1_000_000)

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch<{ plan_id: number }>('/admin/plans', {
        method: 'POST',
        body: {
          plan_code: form.plan_code.trim(),
          display_name: form.display_name.trim(),
          grant_micro: grantMicro,
          group_code: form.group_code.trim() === '' ? undefined : form.group_code.trim(),
          balance_valid_days:
            form.balance_valid_days.trim() === ''
              ? undefined
              : Number(form.balance_valid_days) || undefined,
        },
      }),
    onSuccess: () => setMsg(t('common:success')),
    onError: (err) => setMsg(describeError(err)),
  })

  const fields: ReadonlyArray<readonly [keyof typeof form, string]> = [
    ['plan_code', t('admin:planCode')],
    ['display_name', t('admin:planName')],
    ['grant_usd', t('admin:planGrant')],
    ['group_code', t('admin:planGroup')],
    ['balance_valid_days', t('admin:planValidDays')],
  ]

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:planTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:planHint')}</p>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={field}>{label}</Label>
              <Input
                id={field}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
        </div>
        <div className="flex items-center gap-3">
          <Button
            disabled={upsert.isPending || form.plan_code.trim() === '' || grantMicro <= 0}
            onClick={() => upsert.mutate()}
          >
            {t('admin:planUpsert')}
          </Button>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>
      </CardContent>
    </Card>
  )
}
