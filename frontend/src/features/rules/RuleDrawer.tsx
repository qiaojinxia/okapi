import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { RuleRow, RuleType, StackingMode } from '@/features/rules/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import {
  RULE_TYPES,
  STACKING_LABEL,
  STACKING_MODES,
  TYPE_HINT,
  TYPE_LABEL,
  WEEKDAY_LABEL,
} from '@/features/rules/types'
import { Select } from '@/components/ui/select'
import { TagInput } from '@/components/ui/tag-input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

const str = (v: unknown, fallback = ''): string =>
  v === undefined || v === null ? fallback : String(v)

/// 规则抽屉。按类型只显示该类型用得到的参数——四种类型的必填项互不相同，
/// 全部平铺会让人不知道哪几个字段对当前类型有效。
///
/// `initial` 给出即为编辑：全部字段回填、rule_code 锁定（它是规则的身份，
/// 后端按 code upsert）。此前没有编辑入口，改一条规则要记住 code 重新走新建表单。
export function RuleDrawer({
  onClose,
  onDone,
  initial,
}: {
  onClose: () => void
  onDone: () => void
  initial?: RuleRow
}) {
  const { t } = useTranslation()
  const editing = initial !== undefined
  const p = initial?.params ?? {}
  const [ruleType, setRuleType] = useState<RuleType>(
    (initial?.rule_type as RuleType | undefined) ?? 'discount',
  )
  const [form, setForm] = useState({
    rule_code: initial?.rule_code ?? '',
    multiplier: str(p.multiplier, '0.9'),
    min_monthly_tokens: Number(p.min_monthly_tokens ?? 0) > 0 ? str(p.min_monthly_tokens) : '',
    min_monthly_spend_usd:
      Number(p.min_monthly_spend_micro ?? 0) > 0
        ? String(Number(p.min_monthly_spend_micro) / 1_000_000)
        : '',
    start_minute: p.start_minute === undefined ? '' : str(p.start_minute),
    end_minute: p.end_minute === undefined ? '' : str(p.end_minute),
    priority: str(initial?.priority, '0'),
  })
  const [stacking, setStacking] = useState<StackingMode>(
    (typeof p.stacking_mode === 'string' ? (p.stacking_mode as StackingMode) : undefined) ??
      'stackable',
  )
  // 全不勾 = 每天生效（不发字段）；勾了才收窄
  const [weekdays, setWeekdays] = useState<Set<number>>(
    new Set(Array.isArray(p.weekdays) ? (p.weekdays as number[]) : []),
  )
  const [groups, setGroups] = useState<string[]>(initial?.scope.groups ?? [])
  const [models, setModels] = useState<string[]>(initial?.scope.models ?? [])
  const [users, setUsers] = useState<string[]>((initial?.scope.users ?? []).map(String))

  const upsert = useMutation({
    mutationFn: () => {
      // 消费阈值以 USD 输入、以 micro 整数落库（配置值一次换算，非计费热路径）
      const spendUsd = Number(form.min_monthly_spend_usd)
      const spendMicro =
        ruleType === 'volume' && Number.isFinite(spendUsd) && spendUsd > 0
          ? Math.round(spendUsd * 1_000_000)
          : undefined
      return apiFetch('/admin/pricing/rules', {
        method: 'POST',
        body: {
          rule_code: form.rule_code.trim(),
          rule_type: ruleType,
          multiplier: form.multiplier,
          priority: Number(form.priority) || 0,
          stacking_mode: stacking,
          // 后端按 rule_type 校验必填项，这里只负责不发送无关字段
          min_monthly_tokens:
            ruleType === 'volume' ? Number(form.min_monthly_tokens) || 0 : undefined,
          min_monthly_spend_micro: spendMicro,
          start_minute: ruleType === 'time_based' ? Number(form.start_minute) || 0 : undefined,
          end_minute: ruleType === 'time_based' ? Number(form.end_minute) || 0 : undefined,
          weekdays:
            ruleType === 'time_based' && weekdays.size > 0 ? [...weekdays].sort() : undefined,
          // 空数组要发成 undefined：{} 与 {"groups":[]} 语义不同，后者会命中零个分组
          scope: {
            groups: groups.length > 0 ? groups : undefined,
            models: models.length > 0 ? models : undefined,
            users: users.length > 0 ? users.map(Number).filter((n) => !Number.isNaN(n)) : undefined,
          },
        },
      })
    },
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
      title={editing ? t('admin:ruleEdit', { code: initial.rule_code }) : t('admin:ruleCreate')}
      description={t('admin:ruleDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={form.rule_code.trim() === '' || upsert.isPending}
            onClick={() => upsert.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')} hint={t('admin:ruleTypeHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="r-type">{t('admin:ruleType')}</Label>
          <Select
            id="r-type"
            className="w-44"
            value={ruleType}
            onChange={(v) => setRuleType(v as RuleType)}
            options={RULE_TYPES.map((rt) => ({ value: rt, label: t(TYPE_LABEL[rt]) }))}
          />
          <p className="text-xs text-muted-foreground">{t(TYPE_HINT[ruleType])}</p>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="r-code">{t('admin:ruleCode')}</Label>
          <Input
            id="r-code"
            className="font-mono text-sm"
            value={form.rule_code}
            placeholder="night-discount"
            // 编辑态锁定：code 是规则身份，改它等于新建一条、旧的留着
            disabled={editing}
            onChange={(e) => setForm((f) => ({ ...f, rule_code: e.target.value }))}
          />
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:ruleParams')} hint={t('admin:ruleParamsHint')}>
        <div className="grid grid-cols-2 gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="r-mult">{t('admin:ruleMultiplier')}</Label>
            <Input
              id="r-mult"
              inputMode="decimal"
              value={form.multiplier}
              onChange={(e) => setForm((f) => ({ ...f, multiplier: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="r-prio">{t('admin:priority')}</Label>
            <Input
              id="r-prio"
              inputMode="numeric"
              value={form.priority}
              onChange={(e) => setForm((f) => ({ ...f, priority: e.target.value }))}
            />
          </div>
          {ruleType === 'volume' && (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="r-thr">{t('admin:ruleThreshold')}</Label>
                <Input
                  id="r-thr"
                  inputMode="numeric"
                  value={form.min_monthly_tokens}
                  onChange={(e) => setForm((f) => ({ ...f, min_monthly_tokens: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="r-spend">{t('admin:ruleSpendThreshold')}</Label>
                <Input
                  id="r-spend"
                  inputMode="decimal"
                  placeholder="50"
                  value={form.min_monthly_spend_usd}
                  onChange={(e) =>
                    setForm((f) => ({ ...f, min_monthly_spend_usd: e.target.value }))
                  }
                />
              </div>
            </>
          )}
          {ruleType === 'time_based' && (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="r-start">{t('admin:ruleStart')}</Label>
                <Input
                  id="r-start"
                  inputMode="numeric"
                  placeholder="0"
                  value={form.start_minute}
                  onChange={(e) => setForm((f) => ({ ...f, start_minute: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="r-end">{t('admin:ruleEnd')}</Label>
                <Input
                  id="r-end"
                  inputMode="numeric"
                  placeholder="1439"
                  value={form.end_minute}
                  onChange={(e) => setForm((f) => ({ ...f, end_minute: e.target.value }))}
                />
              </div>
            </>
          )}
        </div>
        {ruleType === 'time_based' && (
          <p className="text-xs text-muted-foreground">{t('admin:ruleMinuteHint')}</p>
        )}
        {ruleType === 'volume' && (
          <p className="text-xs text-muted-foreground">{t('admin:ruleVolumeAxesHint')}</p>
        )}
        {ruleType === 'surge' && (
          <p className="text-xs text-muted-foreground">{t('admin:ruleSurgeHint')}</p>
        )}
        {ruleType === 'time_based' && (
          <div className="flex flex-col gap-1.5">
            <Label>{t('admin:ruleWeekdays')}</Label>
            <div className="flex flex-wrap gap-3">
              {WEEKDAY_LABEL.map((key, day) => (
                <Checkbox
                  key={key}
                  label={t(key)}
                  checked={weekdays.has(day)}
                  onChange={() =>
                    setWeekdays((prev) => {
                      const next = new Set(prev)
                      if (next.has(day)) next.delete(day)
                      else next.add(day)
                      return next
                    })
                  }
                />
              ))}
            </div>
            <p className="text-xs text-muted-foreground">{t('admin:ruleWeekdaysHint')}</p>
          </div>
        )}
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="r-stacking">{t('admin:stackingMode')}</Label>
          <Select
            id="r-stacking"
            className="w-56"
            value={stacking}
            onChange={(v) => setStacking(v as StackingMode)}
            options={STACKING_MODES.map((m) => ({ value: m, label: t(STACKING_LABEL[m]) }))}
          />
          <p className="text-xs text-muted-foreground">{t('admin:stackingModeHint')}</p>
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:ruleScope')} hint={t('admin:ruleScopeHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="s-groups">{t('admin:scopeGroupsLabel')}</Label>
          <TagInput
            id="s-groups"
            value={groups}
            onChange={setGroups}
            placeholder={t('admin:tagInputHint')}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="s-models">{t('admin:scopeModelsLabel')}</Label>
          <TagInput
            id="s-models"
            value={models}
            onChange={setModels}
            placeholder={t('admin:tagInputHint')}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="s-users">{t('admin:scopeUsersLabel')}</Label>
          <TagInput
            id="s-users"
            value={users}
            onChange={setUsers}
            placeholder={t('admin:scopeUsersHint')}
          />
        </div>
      </FieldGroup>
    </Drawer>
  )
}
