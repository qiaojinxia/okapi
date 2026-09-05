import { Plus, Trash2 } from 'lucide-react'
import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ModelListRow } from '@/features/models/types'
import { AXIS_LABEL, MODAL_AXES, TEXT_AXES } from '@/features/models/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { IconButton } from '@/components/ui/icon-button'
import { Input, Label } from '@/components/ui/input'
import { TagInput } from '@/components/ui/tag-input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 模型定价抽屉。倍率按"文本轴 / 多模态轴 / service_tier 档位"分段，
/// 每段给出该轴什么时候需要配，避免用户对着一排 1 不知道该改哪个。
export function ModelDrawer({
  model,
  onClose,
  onDone,
}: {
  model: ModelListRow | undefined
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [name, setName] = useState(model?.model_name ?? '')
  const [axes, setAxes] = useState<Record<string, string>>({
    model_ratio: model?.model_ratio ?? '1',
    completion_ratio: model?.completion_ratio ?? '1',
    cache_ratio: model?.cache_ratio ?? '1',
    cache_write_ratio: model?.cache_write_ratio ?? '1',
    audio_ratio: model?.audio_ratio ?? '1',
    audio_completion_ratio: model?.audio_completion_ratio ?? '1',
    image_ratio: model?.image_ratio ?? '1',
  })
  const [tiers, setTiers] = useState<{ tier: string; ratio: string }[]>([])
  // 阶梯计价表 "0:2.5,128000:5"（from_tokens:USD_per_1M）。空串 = ratio 模式。
  // 与 model_ratio 互斥：填了阶梯，每 token 基准单价改由查表得出，model_ratio 不再参与。
  const [tierExpr, setTierExpr] = useState(model?.tier_expr ?? '')
  // 从现值起手：编辑倍率时不该顺手清掉已配的降级链（后端 None=不改动，
  // 但始终回传现值让"清空"也是显式操作）
  const [fallbacks, setFallbacks] = useState<string[]>(model?.fallback_models ?? [])

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/models', {
        method: 'POST',
        body: {
          model_name: name.trim(),
          ...axes,
          // 档位为空就不发，避免把空对象写成"已配置档位"
          tier_ratios:
            tiers.length > 0
              ? Object.fromEntries(
                  tiers.filter((x) => x.tier.trim() !== '').map((x) => [x.tier.trim(), x.ratio]),
                )
              : undefined,
          fallback_models: fallbacks,
          // 始终回传：空串是"切回 ratio"的显式表达，undefined 才是"不改动"
          tier_expr: tierExpr.trim(),
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const axisField = (key: keyof typeof AXIS_LABEL) => (
    <div key={key} className="flex flex-col gap-1.5">
      <Label htmlFor={`ax-${key}`}>{t(AXIS_LABEL[key])}</Label>
      <Input
        id={`ax-${key}`}
        value={axes[key] ?? '1'}
        inputMode="decimal"
        onChange={(e) => setAxes((a) => ({ ...a, [key]: e.target.value }))}
      />
    </div>
  )

  return (
    <Drawer
      open
      onClose={onClose}
      title={model ? t('admin:editModel', { name: model.model_name }) : t('admin:createModel')}
      description={t('admin:modelDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button disabled={name.trim() === '' || upsert.isPending} onClick={() => upsert.mutate()}>
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')} hint={t('admin:modelNameHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="m-name">{t('admin:modelName')}</Label>
          <Input
            id="m-name"
            className="font-mono text-sm"
            value={name}
            readOnly={model !== undefined}
            placeholder="gpt-4o"
            onChange={(e) => setName(e.target.value)}
          />
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:axesText')} hint={t('admin:axesTextHint')}>
        <div className="grid grid-cols-2 gap-3">{TEXT_AXES.map(axisField)}</div>
      </FieldGroup>

      <FieldGroup title={t('admin:axesModal')} hint={t('admin:axesModalHint')}>
        <div className="grid grid-cols-2 gap-3">{MODAL_AXES.map(axisField)}</div>
      </FieldGroup>

      <FieldGroup title={t('admin:tierExpr')} hint={t('admin:tierExprHint')}>
        <Input
          id="m-tier-expr"
          className="font-mono"
          placeholder="0:2.5,128000:5"
          value={tierExpr}
          onChange={(e) => setTierExpr(e.target.value)}
        />
        <p className="text-xs text-muted-foreground">
          {tierExpr.trim() === ''
            ? t('admin:tierExprModeRatio')
            : t('admin:tierExprModeTiered')}
        </p>
      </FieldGroup>

      <FieldGroup title={t('admin:fallbackModels')} hint={t('admin:fallbackModelsHint')}>
        <TagInput
          id="m-fallbacks"
          value={fallbacks}
          onChange={setFallbacks}
          placeholder="gpt-4o-mini"
        />
      </FieldGroup>

      <FieldGroup title={t('admin:tierRatios')} hint={t('admin:tierRatiosHint')}>
        {tiers.map((row, i) => (
          <div key={i} className="flex items-end gap-2">
            <div className="flex flex-1 flex-col gap-1.5">
              <Label htmlFor={`tier-${i}`}>{t('admin:tierName')}</Label>
              <Input
                id={`tier-${i}`}
                value={row.tier}
                placeholder="flex"
                onChange={(e) =>
                  setTiers((ts) => ts.map((x, j) => (i === j ? { ...x, tier: e.target.value } : x)))
                }
              />
            </div>
            <div className="flex w-28 flex-col gap-1.5">
              <Label htmlFor={`tratio-${i}`}>{t('admin:ruleMultiplier')}</Label>
              <Input
                id={`tratio-${i}`}
                value={row.ratio}
                inputMode="decimal"
                onChange={(e) =>
                  setTiers((ts) => ts.map((x, j) => (i === j ? { ...x, ratio: e.target.value } : x)))
                }
              />
            </div>
            <IconButton
              icon={Trash2}
              label={t('common:delete')}
              variant="destructive"
              onClick={() => setTiers((ts) => ts.filter((_, j) => j !== i))}
            />
          </div>
        ))}
        <Button
          size="sm"
          variant="outline"
          className="self-start"
          onClick={() => setTiers((ts) => [...ts, { tier: '', ratio: '1' }])}
        >
          <Plus className="h-4 w-4" />
          {t('admin:tierAdd')}
        </Button>
      </FieldGroup>
    </Drawer>
  )
}
