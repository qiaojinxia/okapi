import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { GroupListRow } from '@/features/groups/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { PoolReach } from '@/features/pools/PoolReach'
import { DEFAULT_POOL } from '@/features/pools/types'
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
  // 分组必有池：新建缺省 default（新渠道也缺省进 default，两端对齐后"建完就能用"）
  const [form, setForm] = useState({
    group_code: group?.group_code ?? '',
    group_ratio: group?.group_ratio ?? '1',
    description: group?.description ?? '',
    pool_code: group?.pool_code ?? DEFAULT_POOL,
    self_select: group?.self_select ?? false,
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
          pool_code: form.pool_code,
          self_select: form.self_select,
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={group ? t('admin:groupEdit', { name: group.group_code }) : t('admin:groupCreate')}
      description={t('admin:groupDrawerDesc')}
      footer={
        <>
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

      <FieldGroup title={t('admin:groupPool')} hint={t('admin:groupPoolHint')}>
        <Select
          id="g-pool"
          className="w-56"
          value={form.pool_code}
          onChange={(v) => setForm((f) => ({ ...f, pool_code: v }))}
          options={(pools.data?.data ?? []).map((p) => ({
            value: p.pool_code,
            label: p.pool_code === DEFAULT_POOL ? t('admin:poolDefaultOption') : p.pool_code,
          }))}
        />
        {/* 选了池就地看它给得出什么：分组 → 池 → 渠道 → 模型 三跳一处看全 */}
        <PoolReach poolCode={form.pool_code} />
      </FieldGroup>

      <FieldGroup title={t('admin:groupSelfSelect')} hint={t('admin:groupSelfSelectHint')}>
        <Switch
          label={t('admin:groupSelfSelectLabel')}
          checked={form.self_select}
          onChange={(v) => setForm((f) => ({ ...f, self_select: v }))}
        />
      </FieldGroup>
    </Drawer>
  )
}
