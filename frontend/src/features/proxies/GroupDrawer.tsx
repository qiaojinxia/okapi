import { useMutation, useQuery } from '@tanstack/react-query'
import { Plus, X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { IconButton } from '@/components/ui/icon-button'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { proxyOptions } from './options'
import type { ProxyGroupMember, ProxyGroupMode, ProxyGroupRow, ReconcileReport } from './types'
import { GROUP_MODES, GROUP_MODE_HINT, GROUP_MODE_LABEL } from './types'

const CODE_PATTERN = /^[A-Za-z0-9_.-]{1,32}$/

interface MemberDraft {
  proxy_id: number
  priority: string
  weight: string
}

function memberOf(draft: MemberDraft): ProxyGroupMember | null {
  const priority = Number(draft.priority.trim() || '0')
  const weight = Number(draft.weight.trim() || '1')
  if (!Number.isInteger(priority) || priority < -1000 || priority > 1000) return null
  if (!Number.isInteger(weight) || weight < 1 || weight > 1_000_000) return null
  return { proxy_id: draft.proxy_id, priority, weight }
}

/// 代理组抽屉：新建与编辑共用（成员整组替换）。
///
/// 固定分配组：每把 key（账号）分到一个代理并记住，之后一直走它；代理熔断或停用时这把 key
/// 等它恢复而不换 IP，只有把代理移出组 / 删掉才改分。轮换组：每次请求按 priority 分层、
/// 层内按 weight 抽一个健康成员。priority / weight 只在轮换组里决定流量；固定分配组里
/// 分配看已分配数与容量，priority 只做同等条件下的次序。
export function GroupDrawer({
  group,
  onClose,
  onDone,
}: {
  group: ProxyGroupRow | undefined
  onClose: () => void
  onDone: (report?: ReconcileReport) => void
}) {
  const { t } = useTranslation()
  const isEdit = group !== undefined
  const [code, setCode] = useState(group?.code ?? '')
  const [name, setName] = useState(group?.name ?? '')
  const [mode, setMode] = useState<ProxyGroupMode>(group?.mode ?? 'pinned')
  const [description, setDescription] = useState(group?.description ?? '')
  const [members, setMembers] = useState<MemberDraft[]>(
    (group?.members ?? []).map((m) => ({
      proxy_id: m.proxy_id,
      priority: String(m.priority),
      weight: String(m.weight),
    })),
  )
  const [adding, setAdding] = useState('')
  const proxies = useQuery(proxyOptions())
  const available = (proxies.data ?? []).filter((p) => !members.some((m) => m.proxy_id === p.id))
  const parsed = members.map(memberOf)
  const membersValid = parsed.every((m) => m !== null)
  const codeValid = CODE_PATTERN.test(code.trim())

  const save = useMutation({
    mutationFn: () =>
      apiFetch<{ assignment?: ReconcileReport }>('/admin/proxy-groups', {
        method: 'POST',
        body: {
          code: code.trim(),
          name: name.trim() === '' ? undefined : name.trim(),
          mode,
          description: description.trim() === '' ? undefined : description.trim(),
          members: parsed.filter((m): m is ProxyGroupMember => m !== null),
        },
      }),
    onSuccess: (r) => {
      onDone(r.assignment)
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const patch = (index: number, field: 'priority' | 'weight', value: string) =>
    setMembers((list) => list.map((m, i) => (i === index ? { ...m, [field]: value } : m)))

  return (
    <Drawer
      open
      onClose={onClose}
      title={isEdit ? t('admin:proxyGroupEdit', { name: group.name }) : t('admin:proxyGroupCreate')}
      description={t('admin:proxyGroupDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button disabled={!codeValid || !membersValid || save.isPending} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')}>
        <div className="grid grid-cols-2 gap-3">
          <Field
            label={t('admin:proxyGroupCode')}
            htmlFor="pg-code"
            hint={t('admin:proxyGroupCodeHint')}
            error={code !== '' && !codeValid ? t('errors:bad_request', { param: 'code' }) : undefined}
          >
            <Input
              id="pg-code"
              className="font-mono text-sm"
              value={code}
              maxLength={32}
              readOnly={isEdit}
              placeholder="hk-static"
              onChange={(e) => setCode(e.target.value)}
            />
          </Field>
          <Field label={t('admin:proxyName')} htmlFor="pg-name">
            <Input id="pg-name" value={name} maxLength={128} placeholder={code} onChange={(e) => setName(e.target.value)} />
          </Field>
        </div>
        <Field label={t('common:description')} htmlFor="pg-desc">
          <Input id="pg-desc" value={description} maxLength={255} onChange={(e) => setDescription(e.target.value)} />
        </Field>
      </FieldGroup>

      <FieldGroup title={t('admin:proxyGroupMode')} hint={t(GROUP_MODE_HINT[mode])}>
        <Select
          id="pg-mode"
          className="w-56"
          value={mode}
          onChange={(v) => setMode(v as ProxyGroupMode)}
          options={GROUP_MODES.map((m) => ({ value: m, label: t(GROUP_MODE_LABEL[m]) }))}
        />
        {isEdit && mode !== group.mode && (
          <p role="note" className="text-xs text-warning">
            {t(mode === 'rotate' ? 'admin:proxyGroupToRotateWarn' : 'admin:proxyGroupToPinnedWarn')}
          </p>
        )}
      </FieldGroup>

      <FieldGroup title={t('admin:proxyGroupMembers')} hint={t('admin:proxyGroupMembersHint')}>
        {members.length === 0 && (
          <p className="text-xs text-warning">{t('admin:egressGroupEmptyWarn')}</p>
        )}
        {members.map((m, i) => {
          const proxy = proxies.data?.find((p) => p.id === m.proxy_id)
          const invalid = parsed[i] === null
          return (
            <div key={m.proxy_id} className="flex items-end gap-2">
              <div className="flex min-w-0 flex-1 flex-col">
                <span className="truncate text-sm font-medium">{proxy?.name ?? `#${m.proxy_id}`}</span>
                <span className="truncate font-mono text-xs text-muted-foreground">{proxy?.url_masked ?? ''}</span>
              </div>
              <Field label={t('admin:priority')} htmlFor={`pg-pri-${m.proxy_id}`} className="w-20">
                <Input
                  id={`pg-pri-${m.proxy_id}`}
                  inputMode="numeric"
                  value={m.priority}
                  aria-invalid={invalid}
                  onChange={(e) => patch(i, 'priority', e.target.value)}
                />
              </Field>
              <Field label={t('admin:keyWeight')} htmlFor={`pg-w-${m.proxy_id}`} className="w-20">
                <Input
                  id={`pg-w-${m.proxy_id}`}
                  inputMode="numeric"
                  value={m.weight}
                  aria-invalid={invalid}
                  onChange={(e) => patch(i, 'weight', e.target.value)}
                />
              </Field>
              <IconButton
                icon={X}
                label={t('common:delete')}
                onClick={() => setMembers((list) => list.filter((_, j) => j !== i))}
              />
            </div>
          )
        })}
        <div className="flex items-center gap-2">
          <Select
            id="pg-add"
            aria-label={t('admin:proxyGroupAddMember')}
            className="min-w-56 flex-1"
            value={adding}
            placeholder={t('admin:proxyGroupAddMember')}
            onChange={setAdding}
            options={available.map((p) => ({ value: String(p.id), label: `${p.name} · ${p.url_masked}` }))}
          />
          <Button
            variant="outline"
            disabled={adding === ''}
            onClick={() => {
              setMembers((list) => [...list, { proxy_id: Number(adding), priority: '0', weight: '1' }])
              setAdding('')
            }}
          >
            <Plus className="h-4 w-4" />
            {t('admin:proxyGroupAddMember')}
          </Button>
        </div>
      </FieldGroup>
    </Drawer>
  )
}
