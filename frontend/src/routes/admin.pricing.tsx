import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Textarea } from '@/components/ui/textarea'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/admin/pricing')({
  component: PricingPage,
})

function PricingPage() {
  return (
    <div className="flex flex-col gap-4">
      <ModelCard />
      <ModelListCard />
      <GroupsCard />
      <RulesCard />
    </div>
  )
}

interface ModelListRow {
  model_name: string
  vendor: string | null
  status: number
  pricing_mode: string | null
  model_ratio: string | null
  completion_ratio: string | null
  cache_ratio: string | null
  cache_write_ratio: string | null
  audio_ratio: string | null
  audio_completion_ratio: string | null
  image_ratio: string | null
  per_call_price_micro: number | null
}

/// 已配置模型总览 + 删除。
/// 未定价模型（pricing_mode 为空）高亮标注——建了模型却没定价会让请求直接被拒。
function ModelListCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)

  const models = useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: ModelListRow[] }>('/admin/models'),
  })

  const remove = useMutation({
    mutationFn: (name: string) =>
      apiFetch<{ requires_publish: boolean }>(`/admin/models/${encodeURIComponent(name)}`, {
        method: 'DELETE',
      }),
    onSuccess: (r) => {
      setMsg(r.requires_publish ? t('admin:requiresPublish') : t('common:success'))
      void queryClient.invalidateQueries({ queryKey: qk.adminModels })
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:modelListTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        {models.isError ? (
          <p className="text-sm text-destructive">{describeError(models.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:modelName')}</Th>
                <Th>{t('admin:pricingMode')}</Th>
                <Th>{t('admin:modelRatio')}</Th>
                <Th>{t('admin:completionRatio')}</Th>
                <Th>{t('admin:cacheRatio')}</Th>
                <Th>{t('admin:cacheWriteRatioShort')}</Th>
                <Th>{t('admin:audioRatioShort')}</Th>
                <Th>{t('admin:imageRatio')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(models.data?.data ?? []).map((m) => (
                <Tr key={m.model_name}>
                  <Td className="font-mono text-xs">{m.model_name}</Td>
                  <Td>
                    {m.pricing_mode === null ? (
                      <Badge variant="destructive">{t('admin:unpriced')}</Badge>
                    ) : (
                      <Badge variant="muted">{m.pricing_mode}</Badge>
                    )}
                  </Td>
                  <Td>{m.model_ratio ?? '—'}</Td>
                  <Td>{m.completion_ratio ?? '—'}</Td>
                  <Td>{m.cache_ratio ?? '—'}</Td>
                  <Td>{m.cache_write_ratio ?? '—'}</Td>
                  <Td>
                    {m.audio_ratio === null || m.audio_ratio === '1.000000'
                      ? '—'
                      : `${m.audio_ratio} ×${m.audio_completion_ratio ?? '1'}`}
                  </Td>
                  <Td>{m.image_ratio ?? '—'}</Td>
                  <Td>
                    <Button
                      size="sm"
                      variant="destructive"
                      onClick={() => remove.mutate(m.model_name)}
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

interface GroupListRow {
  group_code: string
  group_ratio: string | null
  description: string | null
  is_default: boolean
  user_count: number
  channel_count: number
}

/// 分组倍率：新建 / 列表 / 删除。
/// 列表带占用计数——被用户或渠道引用时后端回 409，这里先把数字摆出来避免无谓尝试。
function GroupsCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [form, setForm] = useState({ group_code: '', group_ratio: '1', description: '' })
  const [msg, setMsg] = useState<string | null>(null)

  const groups = useQuery({
    queryKey: qk.adminGroups,
    queryFn: () => apiFetch<{ data: GroupListRow[] }>('/admin/groups'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminGroups })

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/groups', {
        method: 'POST',
        body: {
          group_code: form.group_code.trim(),
          group_ratio: form.group_ratio.trim(),
          description: form.description.trim(),
        },
      }),
    onSuccess: () => {
      setMsg(t('admin:requiresPublish'))
      setForm({ group_code: '', group_ratio: '1', description: '' })
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/groups/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('admin:requiresPublish'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const fields = [
    ['group_code', t('admin:groupCode')],
    ['group_ratio', t('admin:groupRatio')],
    ['description', t('admin:groupDesc')],
  ] as const

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:groupsTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={`g-${field}`}>{label}</Label>
              <Input
                id={`g-${field}`}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
        </div>
        <div className="flex items-center gap-3">
          <Button
            disabled={upsert.isPending || form.group_code.trim() === ''}
            onClick={() => upsert.mutate()}
          >
            {t('common:save')}
          </Button>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        {groups.isError ? (
          <p className="text-sm text-destructive">{describeError(groups.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:groupCode')}</Th>
                <Th>{t('admin:groupRatio')}</Th>
                <Th>{t('admin:groupDesc')}</Th>
                <Th>{t('admin:groupUsers')}</Th>
                <Th>{t('admin:groupChannels')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(groups.data?.data ?? []).map((g) => (
                <Tr key={g.group_code}>
                  <Td className="font-mono text-xs">
                    {g.group_code}
                    {g.is_default && (
                      <Badge variant="muted" className="ml-2">
                        {t('admin:groupDefault')}
                      </Badge>
                    )}
                  </Td>
                  <Td>{g.group_ratio ?? '—'}</Td>
                  <Td>{g.description ?? '—'}</Td>
                  <Td>{g.user_count}</Td>
                  <Td>{g.channel_count}</Td>
                  <Td>
                    <Button
                      size="sm"
                      variant="destructive"
                      disabled={g.is_default}
                      onClick={() => remove.mutate(g.group_code)}
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

function ModelCard() {
  const { t } = useTranslation()
  const [msg, setMsg] = useState<string | null>(null)
  const [form, setForm] = useState({
    model_name: '',
    model_ratio: '1',
    completion_ratio: '1',
    cache_ratio: '1',
    cache_write_ratio: '1',
    audio_ratio: '1',
    audio_completion_ratio: '1',
    image_ratio: '1',
  })
  const [tierJson, setTierJson] = useState('')

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/models', {
        method: 'POST',
        body: {
          ...form,
          tier_ratios: tierJson.trim() ? (JSON.parse(tierJson) as unknown) : undefined,
        },
      }),
    onSuccess: () => setMsg(t('common:success')),
    onError: (err) =>
      setMsg(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })
  const publish = useMutation({
    mutationFn: () =>
      apiFetch<{ epoch: number }>('/admin/pricing/publish', { method: 'POST', body: {} }),
    onSuccess: (data) => setMsg(t('admin:publishedEpoch', { epoch: data.epoch })),
    onError: (err) => setMsg(describeError(err)),
  })

  const [importJson, setImportJson] = useState('')
  const [importMsg, setImportMsg] = useState<string | null>(null)
  const importRun = useMutation({
    mutationFn: () => {
      const parsed: unknown = JSON.parse(importJson)
      return apiFetch<{ imported: number; skipped: string[] }>(
        '/admin/pricing/import-newapi',
        { method: 'POST', body: parsed },
      )
    },
    onSuccess: (r) =>
      setImportMsg(
        t('admin:importResult', { imported: r.imported, skipped: r.skipped.length }),
      ),
    onError: (err) => setImportMsg(describeError(err)),
  })

  const fields = [
    ['model_name', t('admin:modelName')],
    ['model_ratio', t('admin:modelRatio')],
    ['completion_ratio', t('admin:completionRatio')],
    ['cache_ratio', t('admin:cacheRatio')],
    ['cache_write_ratio', t('admin:cacheWriteRatio')],
    ['audio_ratio', t('admin:audioRatio')],
    ['audio_completion_ratio', t('admin:audioCompletionRatio')],
    ['image_ratio', t('admin:imageRatio')],
  ] as const

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:pricing')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
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
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="tier-ratios">{t('admin:tierRatios')}</Label>
          <Input
            id="tier-ratios"
            className="font-mono text-xs"
            value={tierJson}
            onChange={(e) => setTierJson(e.target.value)}
            placeholder='{"flex":"0.5","priority":"2.0"}'
          />
        </div>
        <div className="flex items-center gap-3">
          <Button
            variant="outline"
            disabled={upsert.isPending || !form.model_name}
            onClick={() => upsert.mutate()}
          >
            {t('admin:upsertModel')}
          </Button>
          <Button disabled={publish.isPending} onClick={() => publish.mutate()}>
            {t('admin:publish')}
          </Button>
          {msg && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        <div className="mt-4 flex flex-col gap-2 border-t border-border pt-4">
          <Label htmlFor="import">{t('admin:importTitle')}</Label>
          <Textarea
            id="import"
            rows={6}
            value={importJson}
            placeholder={t('admin:importPlaceholder')}
            onChange={(e) => setImportJson(e.target.value)}
          />
          <div className="flex items-center gap-3">
            <Button
              variant="outline"
              disabled={importRun.isPending || !importJson.trim()}
              onClick={() => importRun.mutate()}
            >
              {t('admin:importRun')}
            </Button>
            {importMsg && <span className="text-xs text-muted-foreground">{importMsg}</span>}
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

const RULE_TYPES = ['volume', 'time_based', 'discount', 'surge'] as const
type RuleType = (typeof RULE_TYPES)[number]

interface RuleRow {
  rule_code: string
  rule_type: string
  scope: Record<string, unknown>
  params: Record<string, unknown>
  priority: number
  enabled: boolean
  valid_from: string | null
  valid_to: string | null
}

function RulesCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [ruleType, setRuleType] = useState<RuleType>('discount')
  const [form, setForm] = useState({
    rule_code: '',
    multiplier: '0.9',
    min_monthly_tokens: '',
    start_minute: '',
    end_minute: '',
    priority: '0',
  })
  const [scopeJson, setScopeJson] = useState('')

  const rules = useQuery({
    queryKey: qk.adminPricingRules,
    queryFn: () => apiFetch<{ data: RuleRow[] }>('/admin/pricing/rules'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminPricingRules })

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/pricing/rules', {
        method: 'POST',
        body: {
          rule_code: form.rule_code,
          rule_type: ruleType,
          multiplier: form.multiplier,
          priority: Number(form.priority) || 0,
          // 后端按 rule_type 校验必填项，这里只负责不发送无关字段
          min_monthly_tokens:
            ruleType === 'volume' ? Number(form.min_monthly_tokens) || 0 : undefined,
          start_minute: ruleType === 'time_based' ? Number(form.start_minute) || 0 : undefined,
          end_minute: ruleType === 'time_based' ? Number(form.end_minute) || 0 : undefined,
          scope: scopeJson.trim() ? (JSON.parse(scopeJson) as unknown) : undefined,
        },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) =>
      setMsg(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/pricing/rules/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('common:success'))
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

  const typeFields: ReadonlyArray<readonly [keyof typeof form, string]> =
    ruleType === 'volume'
      ? [['min_monthly_tokens', t('admin:ruleThreshold')]]
      : ruleType === 'time_based'
        ? [
            ['start_minute', t('admin:ruleStart')],
            ['end_minute', t('admin:ruleEnd')],
          ]
        : []

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:rulesTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:rulesHint')}</p>

        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rule_type">{t('admin:ruleType')}</Label>
            <select
              id="rule_type"
              className="h-9 rounded-md border border-input bg-card px-2 text-sm"
              value={ruleType}
              onChange={(e) => setRuleType(e.target.value as RuleType)}
            >
              {RULE_TYPES.map((rt) => (
                <option key={rt} value={rt}>
                  {rt}
                </option>
              ))}
            </select>
          </div>
          {(
            [
              ['rule_code', t('admin:ruleCode')],
              ['multiplier', t('admin:ruleMultiplier')],
              ['priority', t('admin:priority')],
              ...typeFields,
            ] as ReadonlyArray<readonly [keyof typeof form, string]>
          ).map(([field, label]) => (
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

        <div className="flex flex-col gap-1.5">
          <Label htmlFor="scope">{t('admin:ruleScope')}</Label>
          <Input
            id="scope"
            className="font-mono text-xs"
            value={scopeJson}
            placeholder={t('admin:ruleScopePlaceholder')}
            onChange={(e) => setScopeJson(e.target.value)}
          />
        </div>

        <div className="flex items-center gap-3">
          <Button
            disabled={upsert.isPending || !form.rule_code || !form.multiplier}
            onClick={() => upsert.mutate()}
          >
            {t('admin:ruleUpsert')}
          </Button>
          {ruleType === 'surge' && (
            <span className="text-xs text-muted-foreground">{t('admin:ruleSurgeHint')}</span>
          )}
          {msg && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        {rules.isError ? (
          <p className="text-sm text-destructive">{describeError(rules.error)}</p>
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
              {(rules.data?.data ?? []).map((r) => (
                <Tr key={r.rule_code}>
                  <Td className="font-mono text-xs">{r.rule_code}</Td>
                  <Td>
                    <Badge>{r.rule_type}</Badge>
                  </Td>
                  <Td className="max-w-64 truncate font-mono text-xs">
                    {JSON.stringify(r.params)}
                  </Td>
                  <Td className="max-w-56 truncate font-mono text-xs">
                    {Object.keys(r.scope).length > 0 ? JSON.stringify(r.scope) : '—'}
                  </Td>
                  <Td>{r.priority}</Td>
                  <Td>
                    <Badge variant={r.enabled ? 'success' : 'muted'}>
                      {r.enabled ? t('common:enabled') : t('common:disabled')}
                    </Badge>
                  </Td>
                  <Td className="flex flex-wrap gap-1.5">
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={toggle.isPending}
                      onClick={() =>
                        toggle.mutate({ code: r.rule_code, enabled: !r.enabled })
                      }
                    >
                      {r.enabled ? t('common:disabled') : t('common:enabled')}
                    </Button>
                    <Button
                      size="sm"
                      variant="destructive"
                      disabled={remove.isPending}
                      onClick={() => remove.mutate(r.rule_code)}
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
