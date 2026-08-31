import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { RuleType } from '@/features/rules/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { RULE_TYPES, TYPE_HINT, TYPE_LABEL } from '@/features/rules/types'
import { Select } from '@/components/ui/select'
import { TagInput } from '@/components/ui/tag-input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 规则抽屉。按类型只显示该类型用得到的参数——四种类型的必填项互不相同，
/// 全部平铺会让人不知道哪几个字段对当前类型有效。
export function RuleDrawer({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { t } = useTranslation()
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
  const [groups, setGroups] = useState<string[]>([])
  const [models, setModels] = useState<string[]>([])
  const [users, setUsers] = useState<string[]>([])

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/pricing/rules', {
        method: 'POST',
        body: {
          rule_code: form.rule_code.trim(),
          rule_type: ruleType,
          multiplier: form.multiplier,
          priority: Number(form.priority) || 0,
          // 后端按 rule_type 校验必填项，这里只负责不发送无关字段
          min_monthly_tokens:
            ruleType === 'volume' ? Number(form.min_monthly_tokens) || 0 : undefined,
          start_minute: ruleType === 'time_based' ? Number(form.start_minute) || 0 : undefined,
          end_minute: ruleType === 'time_based' ? Number(form.end_minute) || 0 : undefined,
          // 空数组要发成 undefined：{} 与 {"groups":[]} 语义不同，后者会命中零个分组
          scope: {
            groups: groups.length > 0 ? groups : undefined,
            models: models.length > 0 ? models : undefined,
            users: users.length > 0 ? users.map(Number).filter((n) => !Number.isNaN(n)) : undefined,
          },
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
      title={t('admin:ruleUpsert')}
      description={t('admin:ruleDrawerDesc')}
      footer={
        <>
          {msg !== null && <span className="mr-auto text-xs text-muted-foreground">{msg}</span>}
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
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="r-thr">{t('admin:ruleThreshold')}</Label>
              <Input
                id="r-thr"
                inputMode="numeric"
                value={form.min_monthly_tokens}
                onChange={(e) => setForm((f) => ({ ...f, min_monthly_tokens: e.target.value }))}
              />
            </div>
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
        {ruleType === 'surge' && (
          <p className="text-xs text-muted-foreground">{t('admin:ruleSurgeHint')}</p>
        )}
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
