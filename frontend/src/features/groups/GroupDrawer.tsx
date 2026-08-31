import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { GroupListRow } from '@/features/groups/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { describeError } from '@/lib/i18n'

export function GroupDrawer({
  group,
  onClose,
  onDone,
}: {
  group: GroupListRow | undefined
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [msg, setMsg] = useState<string | null>(null)
  const [form, setForm] = useState({
    group_code: group?.group_code ?? '',
    group_ratio: group?.group_ratio ?? '1',
    description: group?.description ?? '',
    pool_code: group?.pool_code ?? '',
  })

  // 池清单从后端取：手输池代码会因不存在而被 FK 拒绝，且提示不直观
  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: { pool_code: string }[] }>('/admin/pools'),
  })

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/groups', {
        method: 'POST',
        body: {
          group_code: form.group_code.trim(),
          group_ratio: form.group_ratio.trim(),
          description: form.description.trim(),
          // 空串表示"不限"，要发 null 而不是 ""（后者会被当成池代码去查 FK）
          pool_code: form.pool_code === '' ? null : form.pool_code,
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={group ? t('admin:groupEdit', { name: group.group_code }) : t('admin:groupCreate')}
      description={t('admin:groupDrawerDesc')}
      footer={
        <>
          {msg !== null && <span className="mr-auto text-xs text-muted-foreground">{msg}</span>}
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={form.group_code.trim() === '' || upsert.isPending}
            onClick={() => upsert.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')} hint={t('admin:groupCodeHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="g-code">{t('admin:groupCode')}</Label>
          <Input
            id="g-code"
            className="font-mono text-sm"
            value={form.group_code}
            readOnly={group !== undefined}
            placeholder="vip"
            onChange={(e) => setForm((f) => ({ ...f, group_code: e.target.value }))}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="g-ratio">{t('admin:groupRatio')}</Label>
          <Input
            id="g-ratio"
            className="w-28"
            inputMode="decimal"
            value={form.group_ratio}
            onChange={(e) => setForm((f) => ({ ...f, group_ratio: e.target.value }))}
          />
          <p className="text-xs text-muted-foreground">{t('admin:groupRatioHint')}</p>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="g-desc">{t('admin:groupDesc')}</Label>
          <Input
            id="g-desc"
            value={form.description}
            onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
          />
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:poolMembership')} hint={t('admin:groupPoolHint')}>
        <Select
          id="g-pool"
          className="w-56"
          value={form.pool_code}
          onChange={(v) => setForm((f) => ({ ...f, pool_code: v }))}
          placeholder={t('admin:poolUnlimited')}
          options={(pools.data?.data ?? []).map((p) => ({
            value: p.pool_code,
            label: p.pool_code,
          }))}
        />
      </FieldGroup>
    </Drawer>
  )
}
