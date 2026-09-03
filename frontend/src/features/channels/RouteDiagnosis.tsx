import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

interface DiagKeyReport {
  key_id: number
  status: number
  cooldown_until: string | null
  weight: number
  ok: boolean
  reason: string | null
}

interface DiagChannelReport {
  channel_id: number
  name: string
  provider: string
  status: number
  priority: number
  pools: string[]
  /// 经降级池才可见（正常时不接流量）。
  via_fallback: boolean
  excluded: string | null
  keys: DiagKeyReport[]
}

interface DiagReport {
  model: {
    requested: string
    canonical: string | null
    active: boolean
    priced: boolean
    via_alias: boolean
    fallback_models: string[]
  }
  scope: {
    group_code: string | null
    group_ratio: string | null
    pool_code: string
    pool_source: string
    /// 主池 → 降级池（单跳）。
    pool_chain: string[]
    routing_strategy: string | null
  }
  channels: DiagChannelReport[]
  candidates: number
  verdict: string
  fallbacks: { model: string; viable: boolean; candidates: number; reason: string | null }[]
}

/// 结论 → 文案键的显式映射（禁动态拼 t() 键，同 AXIS_LABEL 的理由）。
const VERDICT_LABEL: Record<string, string> = {
  ok: 'admin:diagVerdictOk',
  model_not_found: 'admin:diagVerdictModelNotFound',
  model_disabled: 'admin:diagVerdictModelDisabled',
  model_unpriced: 'admin:diagVerdictModelUnpriced',
  no_channel_serves_model: 'admin:diagVerdictNoChannelServes',
  no_available_channel: 'admin:diagVerdictNoAvailable',
}

const REASON_LABEL: Record<string, string> = {
  channel_disabled: 'admin:diagReasonChannelDisabled',
  not_in_pool: 'admin:diagReasonNotInPool',
  orphan_channel: 'admin:diagReasonOrphan',
  key_cooling: 'admin:diagReasonKeyCooling',
  key_rate_limited: 'admin:diagReasonKeyRateLimited',
  key_quota_exhausted: 'admin:diagReasonKeyQuotaExhausted',
  key_banned: 'admin:diagReasonKeyBanned',
  key_invalid: 'admin:diagReasonKeyInvalid',
  model_subset_mismatch: 'admin:diagReasonSubset',
  unpriced: 'admin:diagReasonUnpriced',
  no_available_channel: 'admin:diagVerdictNoAvailable',
  missing_or_disabled: 'admin:diagReasonMissingOrDisabled',
}

/// 路由诊断抽屉："为什么这个请求没有候选"。
///
/// 输入 模型 ×（分组 | 池），后端逐环返回 模型→分组→池→渠道→key 的解析结果
/// 与淘汰原因——竞品的通病是这三处要分三个页面自查（new-api FAQ"无可用渠道"
/// 三连查），这里一次给全。
export function RouteDiagnosisDrawer({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation()
  const [model, setModel] = useState('')
  const [group, setGroup] = useState('')
  const [pool, setPool] = useState('')
  const [report, setReport] = useState<DiagReport | null>(null)
  const [msg, setMsg] = useState<string | null>(null)

  const groups = useQuery({
    queryKey: qk.adminGroups,
    queryFn: () => apiFetch<{ data: { group_code: string }[] }>('/admin/groups'),
  })
  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: { pool_code: string }[] }>('/admin/pools'),
  })

  const run = useMutation({
    mutationFn: () => {
      const params = new URLSearchParams({ model: model.trim() })
      if (group !== '') params.set('group', group)
      if (pool !== '') params.set('pool', pool)
      return apiFetch<DiagReport>(`/admin/diagnose/route?${params.toString()}`)
    },
    onSuccess: (r) => {
      setMsg(null)
      setReport(r)
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const reasonText = (reason: string | null) => {
    if (reason === null) return null
    const key = REASON_LABEL[reason]
    return key === undefined ? reason : t(key)
  }

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:diagTitle')}
      description={t('admin:diagDesc')}
      footer={
        <>
          {msg !== null && <span className="mr-auto text-xs text-muted-foreground">{msg}</span>}
          <Button variant="ghost" onClick={onClose}>
            {t('common:close')}
          </Button>
          <Button disabled={model.trim() === '' || run.isPending} onClick={() => run.mutate()}>
            {t('admin:diagRun')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('admin:diagInputs')} hint={t('admin:diagInputsHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="diag-model">{t('admin:modelName')}</Label>
          <Input
            id="diag-model"
            className="font-mono text-sm"
            value={model}
            placeholder="gpt-4o"
            onChange={(e) => setModel(e.target.value)}
          />
        </div>
        <div className="grid grid-cols-2 gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="diag-group">{t('admin:diagGroup')}</Label>
            <Select
              id="diag-group"
              value={group}
              onChange={setGroup}
              placeholder={t('admin:diagGroupNone')}
              options={(groups.data?.data ?? []).map((g) => ({
                value: g.group_code,
                label: g.group_code,
              }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="diag-pool">{t('admin:diagPool')}</Label>
            <Select
              id="diag-pool"
              value={pool}
              onChange={setPool}
              placeholder={t('admin:diagPoolFollow')}
              options={(pools.data?.data ?? []).map((p) => ({
                value: p.pool_code,
                label: p.pool_code,
              }))}
            />
          </div>
        </div>
      </FieldGroup>

      {report !== null && (
        <>
          <FieldGroup title={t('admin:diagVerdict')}>
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant={report.verdict === 'ok' ? 'success' : 'destructive'}>
                {t(VERDICT_LABEL[report.verdict] ?? 'admin:diagVerdictNoAvailable')}
              </Badge>
              <span className="text-sm text-muted-foreground">
                {t('admin:diagCandidates', { n: report.candidates })}
              </span>
            </div>
          </FieldGroup>

          <FieldGroup title={t('admin:diagModelHop')}>
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <span className="font-mono">{report.model.canonical ?? report.model.requested}</span>
              {report.model.via_alias && (
                <Badge variant="muted">
                  {t('admin:diagViaAlias', { name: report.model.requested })}
                </Badge>
              )}
              <Badge variant={report.model.active ? 'success' : 'destructive'}>
                {report.model.active ? t('common:enabled') : t('common:disabled')}
              </Badge>
              <Badge variant={report.model.priced ? 'success' : 'destructive'}>
                {report.model.priced ? t('admin:diagPriced') : t('admin:diagUnpriced')}
              </Badge>
            </div>
          </FieldGroup>

          <FieldGroup title={t('admin:diagScopeHop')}>
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <Badge variant="muted">
                {report.scope.group_code === null
                  ? t('admin:diagGroupNone')
                  : `${report.scope.group_code} ×${report.scope.group_ratio ?? '1'}`}
              </Badge>
              <Badge variant="muted">
                {`${report.scope.pool_code} (${report.scope.routing_strategy ?? ''})`}
              </Badge>
              {/* 池链：主池打不通再进降级池——诊断要把这一跳明示出来 */}
              {report.scope.pool_chain.length > 1 && (
                <Badge variant="muted">
                  {t('admin:poolReachFallback', { pool: report.scope.pool_chain[1] })}
                </Badge>
              )}
            </div>
          </FieldGroup>

          <FieldGroup title={t('admin:diagChannelsHop')} hint={t('admin:diagChannelsHint')}>
            {report.channels.length === 0 ? (
              <p className="text-xs text-muted-foreground">{t('admin:diagNoChannels')}</p>
            ) : (
              <div className="flex flex-col gap-2">
                {report.channels.map((ch) => (
                  <div key={ch.channel_id} className="rounded-md border border-border p-2">
                    <div className="flex flex-wrap items-center gap-2 text-sm">
                      <span className="font-medium">{ch.name}</span>
                      <Badge variant="muted">{ch.provider}</Badge>
                      <Badge variant="muted">P{ch.priority}</Badge>
                      {ch.pools.map((p) => (
                        <Badge key={p} variant="muted" className="font-mono">
                          {p}
                        </Badge>
                      ))}
                      {ch.excluded !== null && (
                        <Badge variant="destructive">{reasonText(ch.excluded)}</Badge>
                      )}
                      {ch.via_fallback && (
                        <Badge variant="warning">{t('admin:diagViaFallback')}</Badge>
                      )}
                    </div>
                    <div className="mt-1.5 flex flex-wrap gap-1.5">
                      {ch.keys.map((k) => (
                        <Badge
                          key={k.key_id}
                          variant={k.ok ? 'success' : k.reason === null ? 'muted' : 'warning'}
                          className="font-mono"
                        >
                          #{k.key_id}
                          {k.ok
                            ? ` ${t('admin:diagKeyOk')}`
                            : k.reason !== null
                              ? ` ${reasonText(k.reason)}`
                              : ''}
                        </Badge>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </FieldGroup>

          {report.fallbacks.length > 0 && (
            <FieldGroup title={t('admin:diagFallbacksHop')} hint={t('admin:diagFallbacksHint')}>
              <div className="flex flex-col gap-1.5">
                {report.fallbacks.map((f) => (
                  <div key={f.model} className="flex flex-wrap items-center gap-2 text-sm">
                    <span className="font-mono">{f.model}</span>
                    {f.viable ? (
                      <Badge variant="success">
                        {t('admin:diagViable', { n: f.candidates })}
                      </Badge>
                    ) : (
                      <Badge variant="destructive">{reasonText(f.reason)}</Badge>
                    )}
                  </div>
                ))}
              </div>
            </FieldGroup>
          )}
        </>
      )}
    </Drawer>
  )
}
