import { Plus, Power, PowerOff, Trash2 } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { RuleDrawer } from '@/features/rules/RuleDrawer'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

interface RuleRow {
  rule_code: string
  rule_type: string
  scope: { groups?: string[]; models?: string[]; users?: number[] }
  params: Record<string, unknown>
  priority: number
  enabled: boolean
  valid_from: string | null
  valid_to: string | null
}



/// 计费规则页（阶梯 / 时段 / 折扣 / 加价）。
///
/// 规则是叠在模型倍率与分组倍率之上的第三层，独立成页：它有自己的生命周期
/// （上线、下线、复用），与"模型值多少钱"不是同一个决策。
export function RulesPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [drawer, setDrawer] = useState(false)
  const { confirm, dialog } = useConfirm()

  const rules = useQuery({
    queryKey: qk.adminPricingRules,
    queryFn: () => apiFetch<{ data: RuleRow[] }>('/admin/pricing/rules'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminPricingRules })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/pricing/rules/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('admin:requiresPublish'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  // 活动上下线：不删配置便于复用（如每年双十一复用同一规则）
  const toggle = useMutation({
    mutationFn: (arg: { code: string; enabled: boolean }) =>
      apiFetch(`/admin/pricing/rules/${encodeURIComponent(arg.code)}/toggle`, {
        method: 'POST',
        body: { enabled: arg.enabled },
      }),
    onSuccess: () => {
      setMsg(t('admin:requiresPublish'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const rows = rules.data?.data ?? []

  /// 参数列人话化。此前直接 JSON.stringify(params)，运营看到的是
  /// {"min_monthly_tokens":100,"multiplier":"0.9"}——要读懂得先知道字段名。
  const describeParams = (r: RuleRow) => {
    const p = r.params
    const mult = typeof p.multiplier === 'string' ? p.multiplier : String(p.multiplier ?? '1')
    if (r.rule_type === 'volume') {
      return t('admin:paramsVolume', { n: String(p.min_monthly_tokens ?? 0), mult })
    }
    if (r.rule_type === 'time_based') {
      const hhmm = (v: unknown) => {
        const m = Number(v ?? 0)
        return `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`
      }
      return t('admin:paramsTime', {
        from: hhmm(p.start_minute),
        to: hhmm(p.end_minute),
        mult,
      })
    }
    return t('admin:paramsMult', { mult })
  }
  const describeScope = (s: RuleRow['scope']) => {
    const parts: string[] = []
    if (s.groups?.length) parts.push(t('admin:scopeGroups', { list: s.groups.join(', ') }))
    if (s.models?.length) parts.push(t('admin:scopeModels', { list: s.models.join(', ') }))
    if (s.users?.length) parts.push(t('admin:scopeUsers', { n: s.users.length }))
    return parts.length === 0 ? t('admin:scopeAll') : parts.join(' · ')
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:rulesTitle')}
        description={t('admin:rulesHint')}
        action={
          <Button onClick={() => setDrawer(true)}>
            <Plus className="h-4 w-4" />
            {t('admin:ruleUpsert')}
          </Button>
        }
      />

      {msg !== null && <p className="text-xs text-muted-foreground">{msg}</p>}
      {dialog}

      {rules.isError ? (
        <ErrorState message={describeError(rules.error)} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:rulesEmptyHint')} />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>{t('admin:ruleCode')}</Th>
              <Th>{t('admin:ruleType')}</Th>
              <Th>{t('admin:ruleParams')}</Th>
              <Th>{t('admin:ruleScope')}</Th>
              <Th>{t('admin:priority')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => (
              <Tr key={r.rule_code}>
                <Td className="font-mono text-xs">{r.rule_code}</Td>
                <Td>
                  <Badge>{r.rule_type}</Badge>
                </Td>
                <Td className="max-w-64 truncate text-xs">{describeParams(r)}</Td>
                <Td className="max-w-64 truncate text-xs text-muted-foreground">
                  {describeScope(r.scope)}
                </Td>
                <Td>{r.priority}</Td>
                <Td>
                  <Badge variant={r.enabled ? 'success' : 'muted'}>
                    {r.enabled ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td>
                  <div className="flex items-center gap-0.5">
                    <IconButton
                      icon={r.enabled ? PowerOff : Power}
                      label={r.enabled ? t('common:disabled') : t('common:enabled')}
                      disabled={toggle.isPending}
                      onClick={() => toggle.mutate({ code: r.rule_code, enabled: !r.enabled })}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: r.rule_code }),
                          description: t('admin:confirmRuleDelete'),
                          onConfirm: () => remove.mutate(r.rule_code),
                        })
                      }
                    />
                  </div>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {drawer && (
        <RuleDrawer
          onClose={() => setDrawer(false)}
          onDone={() => {
            setMsg(t('admin:requiresPublish'))
            invalidate()
          }}
        />
      )}
    </div>
  )
}
