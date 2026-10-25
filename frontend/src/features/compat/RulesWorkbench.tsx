import { Braces, Check, RotateCcw } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { Input, Label, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { toast } from '@/components/ui/toast'
import { useCatalog, useSetActions } from '@/features/compat/api'
import { pretty } from '@/features/compat/diff'
import type { RulesEditor } from '@/features/compat/editor'
import { describeCompatError, fmtAt } from '@/features/compat/format'
import { RulesDiffView } from '@/features/compat/RulesDiffView'
import type { Recipe, RuleIssue, SetOverview, SetScope } from '@/features/compat/types'

/// 配方的标题：语言包里有就用本地化的（键里的 `-` 写成 `_`），没有退回目录里的英文原文。
export function recipeTitle(t: (key: string, options?: Record<string, unknown>) => string, recipe: Recipe): string {
  return t(`compat:recipe_${recipe.id.replaceAll('-', '_')}`, { defaultValue: recipe.title })
}

export function IssueList({ issues }: { issues: RuleIssue[] }) {
  const { t } = useTranslation()
  return (
    <div role="alert" className="flex flex-col gap-1">
      <p className="text-xs font-medium text-destructive">{t('compat:validBad', { n: issues.length })}</p>
      <ul className="flex max-h-40 flex-col gap-1 overflow-auto text-xs">
        {issues.slice(0, 20).map((issue, index) => (
          <li key={index} className="flex min-w-0 flex-wrap items-baseline gap-x-2 text-destructive">
            <code className="break-all">{issue.rule ?? `#${issue.index ?? '?'}`}</code>
            <b className="font-medium">{issue.code}</b>
            <span className="min-w-0 break-words text-destructive/80">{issue.detail}</span>
          </li>
        ))}
      </ul>
    </div>
  )
}

/// 一个规则集的编辑与审批：JSON 编辑器（带配方、实时校验）、草稿横幅与差异、存草稿 / 发布 / 丢弃。
///
/// 编辑器状态在外层（`useRulesEditor`），因为蓝图与试跑页签要读同一份「正在编辑的规则」。
/// 权限由外层按范围算好传进来：`canWrite` = 这个范围的写权限点，`canPublish` = `compat.publish`。
/// 前端只决定显示哪些按钮，真正的拦截在后端。
export function RulesWorkbench({
  scope,
  id,
  overview,
  editor,
  canWrite,
  canPublish,
}: {
  scope: SetScope
  id: number
  overview: SetOverview
  editor: RulesEditor
  canWrite: boolean
  canPublish: boolean
}) {
  const { t } = useTranslation()
  const catalog = useCatalog()
  const actions = useSetActions(scope, id)
  const { confirm, dialog } = useConfirm()
  const [diff, setDiff] = useState<'draft' | 'unsaved' | null>(null)
  const [note, setNote] = useState('')
  const { published, draft } = overview
  const base = published?.version ?? 0
  const { validation } = editor
  const valid = editor.rules !== null && validation.valid === true && !validation.pending
  const canSave = canWrite && editor.dirty && valid
  const busy =
    actions.saveDraft.isPending ||
    actions.publishDirect.isPending ||
    actions.publishDraft.isPending ||
    actions.discardDraft.isPending
  const fail = (err: unknown) => toast.error(describeCompatError(err, t))
  const trimmedNote = note.trim()

  const saveDraft = () => {
    if (editor.rules === null) return
    actions.saveDraft.mutate(
      { rules: editor.rules, base_version: base, ...(trimmedNote === '' ? {} : { note: trimmedNote }) },
      {
        onSuccess: () => {
          toast.success(t('compat:draftSaved'))
          setNote('')
        },
        onError: fail,
      },
    )
  }
  const publishNow = () => {
    if (editor.rules === null) return
    actions.publishDirect.mutate(
      { rules: editor.rules, base_version: base, ...(trimmedNote === '' ? {} : { note: trimmedNote }) },
      {
        onSuccess: (done) => {
          toast.success(t('compat:publishedToast', { version: done.version }))
          setNote('')
        },
        onError: fail,
      },
    )
  }
  const approve = () =>
    actions.publishDraft.mutate(undefined, {
      onSuccess: (done) => toast.success(t('compat:publishedToast', { version: done.version })),
      onError: fail,
    })
  const discard = () =>
    confirm({
      title: t('compat:discardTitle'),
      description: t('compat:discardBody'),
      confirmLabel: t('compat:discard'),
      onConfirm: () =>
        actions.discardDraft.mutate(undefined, {
          onSuccess: () => toast.success(t('compat:discarded')),
          onError: fail,
        }),
    })

  const recipes = catalog.data?.recipes ?? []

  return (
    <div className="flex flex-col gap-4">
      {dialog}
      <section aria-label={t('compat:liveVersion')} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
        <span className="font-medium">{t('compat:liveVersion')}</span>
        {published === null ? (
          <span className="text-muted-foreground">{t('compat:noPublished')}</span>
        ) : (
          <span className="text-muted-foreground">
            {t('compat:liveMeta', { version: published.version, n: published.rules.length, at: fmtAt(published.published_at) })}
          </span>
        )}
      </section>

      {draft !== null && (
        <Alert tone={overview.stale ? 'warning' : 'info'} title={t('compat:draftBanner', { version: draft.version })}>
          <span className="flex flex-col gap-1 text-xs">
            <span className="flex flex-wrap items-center gap-2">
              <Badge variant={draft.source === 'mcp' ? 'info' : 'muted'}>
                {draft.source === 'mcp' ? t('compat:sourceAi') : t('compat:sourceAdmin')}
              </Badge>
              <span>{fmtAt(draft.created_at)}</span>
              {overview.stale && <Badge variant="warning">{t('compat:stale')}</Badge>}
            </span>
            {draft.rationale !== null && (
              <span className="break-words">
                <b className="font-medium">{t('compat:rationale')}</b>
                {draft.rationale}
              </span>
            )}
            {draft.note !== null && <span className="break-words text-muted-foreground">{draft.note}</span>}
            {editor.dirty && <span className="text-warning">{t('compat:approveNeedsClean')}</span>}
            {/* 按钮放在文字下面而不是右侧：窄屏上右侧的动作会把说明挤成一条竖线 */}
            <span className="flex flex-wrap gap-2 pt-1.5">
              <Button size="sm" variant="outline" onClick={() => setDiff(diff === 'draft' ? null : 'draft')}>
                {diff === 'draft' ? t('compat:hideDiff') : t('compat:viewDiff')}
              </Button>
              {canPublish && (
                <Button
                  size="sm"
                  loading={actions.publishDraft.isPending}
                  disabled={busy || editor.dirty || overview.stale}
                  onClick={approve}
                >
                  {t('compat:approve')}
                </Button>
              )}
              {canWrite && (
                <Button size="sm" variant="outline" disabled={busy} onClick={discard}>
                  {t('compat:discard')}
                </Button>
              )}
            </span>
          </span>
        </Alert>
      )}
      {diff === 'draft' && draft !== null && <RulesDiffView before={published?.rules ?? []} after={draft.rules} />}

      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-end justify-between gap-2">
          <div className="flex flex-col gap-0.5">
            <Label htmlFor="compat-rules-editor">{t('compat:editorLabel')}</Label>
            <p className="text-xs text-muted-foreground">{t('compat:editorHint')}</p>
          </div>
          {canWrite && (
            <div className="flex flex-wrap items-center gap-2">
              <Select
                className="w-56"
                aria-label={t('compat:insertRecipe')}
                value=""
                placeholder={t('compat:insertRecipe')}
                options={recipes.map((recipe) => ({ value: recipe.id, label: recipeTitle(t, recipe) }))}
                onChange={(recipeId) => {
                  const recipe = recipes.find((r) => r.id === recipeId)
                  if (recipe !== undefined && !editor.insertRule(recipe.rule)) toast.error(t('compat:insertNeedsJson'))
                }}
              />
              <Button
                size="sm"
                variant="outline"
                disabled={editor.rules === null}
                onClick={() => editor.rules !== null && editor.setText(pretty(editor.rules))}
              >
                <Braces className="h-3.5 w-3.5" />
                {t('compat:format')}
              </Button>
              <Button size="sm" variant="outline" disabled={!editor.dirty} onClick={editor.reset}>
                <RotateCcw className="h-3.5 w-3.5" />
                {t('compat:revert')}
              </Button>
            </div>
          )}
        </div>
        <Textarea
          id="compat-rules-editor"
          value={editor.text}
          readOnly={!canWrite}
          spellCheck={false}
          rows={16}
          aria-invalid={editor.rules === null || validation.valid === false}
          className="min-h-64 font-mono text-xs leading-5"
          onChange={(event) => editor.setText(event.target.value)}
        />
        {editor.rules === null ? (
          <p role="alert" className="text-xs text-destructive">{t('compat:notJson')}</p>
        ) : validation.pending ? (
          <p className="text-xs text-muted-foreground">{t('compat:validPending')}</p>
        ) : validation.valid === true ? (
          <p className="flex items-center gap-1 text-xs text-success">
            <Check className="h-3.5 w-3.5" />
            {t('compat:validOk', { n: editor.rules.length })}
          </p>
        ) : validation.valid === false ? (
          <IssueList issues={validation.issues} />
        ) : null}
        {!canWrite && <p className="text-xs text-muted-foreground">{t('compat:readOnly')}</p>}
        {editor.dirty && editor.rules !== null && valid && (
          <div className="flex flex-col gap-2">
            <button
              type="button"
              className="self-start text-xs text-primary underline-offset-4 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40"
              onClick={() => setDiff(diff === 'unsaved' ? null : 'unsaved')}
            >
              {diff === 'unsaved' ? t('compat:hideUnsaved') : t('compat:viewUnsaved')}
            </button>
            {diff === 'unsaved' && <RulesDiffView before={editor.baseline} after={editor.rules} />}
          </div>
        )}
      </div>

      {canWrite && (
        <div className="flex flex-col gap-3 border-t border-border pt-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="compat-note">{t('compat:noteLabel')}</Label>
            <Input
              id="compat-note"
              value={note}
              maxLength={255}
              placeholder={t('compat:notePlaceholder')}
              onChange={(event) => setNote(event.target.value)}
            />
          </div>
          {draft !== null && editor.dirty && <p className="text-xs text-warning">{t('compat:overwritesDraft')}</p>}
          <div className="flex flex-wrap justify-end gap-2">
            <Button
              variant={canPublish ? 'outline' : 'default'}
              loading={actions.saveDraft.isPending}
              disabled={!canSave || busy}
              onClick={saveDraft}
            >
              {canPublish ? t('compat:saveDraft') : t('compat:submitForApproval')}
            </Button>
            {canPublish && (
              <Button loading={actions.publishDirect.isPending} disabled={!canSave || busy} onClick={publishNow}>
                {t('compat:saveAndPublish')}
              </Button>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
