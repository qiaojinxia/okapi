import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { TagInput } from '@/components/ui/tag-input'
import { InjectFieldsEditor } from '@/features/channels/KeyValueEditors'
import {
  readSimpleRules,
  useChannelCompat,
  useCompatVendors,
  type CompatLayer,
  type CompatRule,
} from '@/features/channels/compat'
import { describeError } from '@/lib/i18n'

/// 抽屉里还没保存的适配改动；字段缺省 = 没动。
export interface CompatDraft {
  /// null = 清掉厂商；字符串 = 选定内置厂商（带命名空间）。
  vendor?: string | null
  strip?: string[]
  inject?: Record<string, unknown>
}

/// 去掉已经保存的那部分草稿，保留保存期间用户又改的。
export function withoutSaved(current: CompatDraft, saved: CompatDraft): CompatDraft {
  const next = { ...current }
  for (const key of ['vendor', 'strip', 'inject'] as const) {
    if (JSON.stringify(current[key]) === JSON.stringify(saved[key])) delete next[key]
  }
  return next
}

const LAYER_VARIANTS: Record<CompatLayer, 'info' | 'default' | 'warning' | 'muted'> = {
  vendor: 'info',
  profile: 'muted',
  channel: 'default',
  guard: 'warning',
}

/// 一条规则的一行摘要：操作名 + 它动了哪些路径，足够看懂「这条规则在干什么」。
function describeRule(rule: CompatRule): string {
  const parts = (rule.do ?? []).map((op) => {
    const paths = Array.isArray(op.paths) ? (op.paths as string[]).join(' ') : ''
    const target = paths || [op.path, op.from, op.to].filter((p): p is string => typeof p === 'string').join(' → ')
    return `${op.op} ${target}`.trim()
  })
  return parts.join('；')
}

/// 渠道抽屉「行为」页签里的适配规则：厂商预设、剥/注字段两个编辑器（读写渠道层的两条约定规则），
/// 以及合并后的有效规则（只读，带来源）。更复杂的规则走接口 / 后续的规则页，这里原样保留不动。
export function CompatPanel({
  channelId,
  draft,
  onDraft,
}: {
  channelId: number
  draft: CompatDraft
  onDraft: (patch: CompatDraft) => void
}) {
  const { t } = useTranslation()
  const view = useChannelCompat(channelId)
  const vendors = useCompatVendors(true)
  if (view.isPending) return <p className="text-xs text-muted-foreground">{t('common:loading')}</p>
  if (view.isError) {
    return (
      <div className="flex items-center justify-between gap-2">
        <p role="alert" className="text-xs text-destructive">{describeError(view.error)}</p>
        <Button size="sm" variant="outline" disabled={view.isFetching} onClick={() => void view.refetch()}>
          {t('common:retry')}
        </Button>
      </div>
    )
  }
  const data = view.data
  const simple = readSimpleRules(data.channel?.rules ?? [])
  const vendor = draft.vendor === undefined ? data.vendor : draft.vendor
  const suggestion = data.suggested_vendor
  // 版本变了（保存成功后刷新）就让编辑器按新数据重新挂载，草稿状态不会越过版本
  const editorKey = data.channel?.version ?? 0
  const known = [
    ...(vendors.data?.data ?? []).map((v) => ({ value: v.id, label: v.display_name })),
    // 适配档案（custom:<slug>）：同一个下拉，标出这是档案
    ...(vendors.data?.profiles ?? []).map((p) => ({ value: p.id, label: t('compat:profileOption', { name: p.display_name }) })),
  ]
  // 目录里没有的旧值也要能显示出来，否则下拉会悄悄显示成「不使用」
  const vendorOptions = vendor !== null && !known.some((o) => o.value === vendor) ? [...known, { value: vendor, label: vendor }] : known
  const stripValue = draft.strip ?? simple.strip
  const stripsUsage = stripValue.includes('stream_options') || 'stream_options' in (draft.inject ?? simple.inject)
  const shadowed = data.effective.shadowed
  return (
    <>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="d-vendor">{t('admin:compatVendor')}</Label>
        <p className="text-xs text-muted-foreground">{t('admin:compatVendorHint')}</p>
        <Select
          id="d-vendor"
          className="w-72"
          value={vendor ?? ''}
          placeholder={t('admin:compatVendorNone')}
          options={vendorOptions}
          onChange={(v) => onDraft({ vendor: v === '' ? null : v })}
        />
        {vendor === null && suggestion !== null && (
          <p role="note" className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            {t('admin:compatVendorSuggest', { name: suggestion.display_name })}
            <Button type="button" size="sm" variant="outline" onClick={() => onDraft({ vendor: suggestion.id })}>
              {t('admin:compatVendorApply')}
            </Button>
          </p>
        )}
      </div>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="d-strip">{t('admin:stripFields')}</Label>
        <p className="text-xs text-muted-foreground">{t('admin:stripFieldsHint')}</p>
        {simple.stripLocked ? (
          <p role="note" className="text-xs text-muted-foreground">{t('admin:compatEditorLocked')}</p>
        ) : (
          <TagInput
            key={`strip-${editorKey}`}
            id="d-strip"
            value={stripValue}
            onChange={(strip) => onDraft({ strip })}
            placeholder="logit_bias"
          />
        )}
      </div>
      {simple.injectLocked ? (
        <div className="flex flex-col gap-1.5">
          <Label>{t('admin:injectFields')}</Label>
          <p role="note" className="text-xs text-muted-foreground">{t('admin:compatEditorLocked')}</p>
        </div>
      ) : (
        <InjectFieldsEditor
          key={`inject-${editorKey}`}
          value={draft.inject ?? simple.inject}
          onChange={(inject) => onDraft({ inject })}
        />
      )}
      {stripsUsage && (
        <p role="note" className="text-xs leading-5 text-warning">{t('admin:compatUsageWarning')}</p>
      )}
      <details className="group/effective rounded-md border border-border px-3 py-2">
        <summary className="cursor-pointer text-sm font-medium outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
          {t('admin:compatEffective', { n: data.effective.rules.length })}
        </summary>
        <p className="mt-2 text-xs leading-5 text-muted-foreground">{t('admin:compatEffectiveHint')}</p>
        {data.effective.rules.length === 0 ? (
          <p className="mt-2 text-xs text-muted-foreground">{t('admin:compatEffectiveEmpty')}</p>
        ) : (
          <ol className="mt-2 flex flex-col gap-2">
            {data.effective.rules.map(({ layer, rule }) => (
              <li key={`${layer}:${rule.id}`} className="flex min-w-0 flex-col gap-0.5 text-xs">
                <span className="flex flex-wrap items-center gap-2">
                  <Badge variant={LAYER_VARIANTS[layer]}>{t(`admin:compatLayer_${layer}`)}</Badge>
                  <code className="break-all">{rule.id}</code>
                </span>
                <span className="break-words text-muted-foreground">{describeRule(rule)}</span>
                {rule.doc?.why && <span className="break-words text-muted-foreground/80">{rule.doc.why}</span>}
              </li>
            ))}
          </ol>
        )}
        {shadowed.length > 0 && (
          <p className="mt-2 text-xs leading-5 text-muted-foreground">
            {t('admin:compatShadowed', {
              list: shadowed.map((s) => `${s.id}（${t(`admin:compatLayer_${s.layer}`)}→${t(`admin:compatLayer_${s.by}`)}）`).join('、'),
            })}
          </p>
        )}
      </details>
    </>
  )
}
