import { useQuery } from '@tanstack/react-query'
import { ArrowDown, Bot, ChevronDown, Download, Eraser, FileDown, FlaskConical, RotateCcw, Save, Send, SlidersHorizontal, Square, Trash2 } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { estimateCost, outputSpeed } from './cost'
import { conversationMarkdown, downloadText } from './export'
import { Markdown } from './markdown'
import { ModelInfo } from './ModelInfo'
import type { SendOptions, Turn } from './use-chat-stream'
import { useChatStream } from './use-chat-stream'
import {
  DEFAULT_TEMPERATURE,
  DEFAULT_TOP_P,
  fromSitePreset,
  removePreset,
  upsertPreset,
  useSitePresets,
  useUserPresets,
} from './presets'
import type { Preset } from './presets'
import { readSettings, writeSettings } from './settings'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Field } from '@/components/ui/field'
import { IconButton } from '@/components/ui/icon-button'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { Textarea } from '@/components/ui/textarea'
import { toast } from '@/components/ui/toast'
import { isAvailable, nonnegative } from '@/features/public-pricing/catalog-data'
import type { PricingGroup, PricingModel } from '@/features/public-pricing/types'
import { useMe } from '@/hooks/use-auth'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

/// 试用台（IMPLEMENTATION §11.39）：左栏模型 + 系统提示词 + 采样参数，右栏流式对话。
///
/// 请求经同源中继打到数据面同一处理器，账单落在登录 key 上——这里看到的通不通、多少钱，
/// 就是用户接 SDK 会看到的。助手回复按 markdown 渲染（代码块可复制），每条标出首字耗时、
/// 输出速度与估算费用；对话留在本标签页（刷新 / 去模型广场看价再回来不丢），参数与预设留在本机。
export function PlaygroundPage() {
  const me = useMe()
  // 用户身份到达后整体重建：对话与参数都按用户从存储里恢复，不在 effect 里补读
  return <Workspace key={me.data?.user_id ?? 'pending'} userId={me.data?.user_id} group={me.data?.group ?? ''} />
}

function formatDuration(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`
}

function Workspace({ userId, group }: { userId: number | undefined; group: string }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const ids = { model: useId(), list: useId(), system: useId(), temp: useId(), topP: useId(), max: useId(), preset: useId(), input: useId(), config: useId() }
  const chat = useChatStream(userId)

  const [saved] = useState(() => readSettings(userId))
  const [model, setModel] = useState(saved?.model ?? '')
  const [system, setSystem] = useState(saved?.system ?? '')
  const [temperature, setTemperature] = useState(saved?.temperature ?? String(DEFAULT_TEMPERATURE))
  const [topP, setTopP] = useState(saved?.topP ?? String(DEFAULT_TOP_P))
  const [maxTokens, setMaxTokens] = useState(saved?.maxTokens ?? '')
  const [presetName, setPresetName] = useState('')
  const [draft, setDraft] = useState('')
  // 窄屏：配置栏默认收起，对话在前（否则要先翻过整张表单才看得到聊天）
  const [configOpen, setConfigOpen] = useState(false)

  // 模型候选：本分组可用（模型广场与接入指南同口径），允许手输——目录缓存可能落后于站长刚加的模型
  const pricing = useQuery({
    queryKey: qk.publicPricing,
    queryFn: () => apiFetch<{ models: PricingModel[]; groups: PricingGroup[] }>('/api/pricing'),
    staleTime: 60_000,
  })
  const catalog = pricing.data?.models ?? []
  const available = catalog
    .filter((m) => isAvailable(m, group))
    .map((m) => m.model)
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
  const modelValue = model !== '' ? model : (available[0] ?? '')
  const factor = group ? nonnegative(pricing.data?.groups.find((g) => g.code === group)?.ratio) : 1
  const modelEntry = catalog.find((m) => m.model === modelValue.trim())

  const sitePresets = useSitePresets()
  const userPresets = useUserPresets(userId)

  const tempNum = Number(temperature)
  const topPNum = Number(topP)
  const maxNum = maxTokens.trim() === '' ? null : Number(maxTokens)
  const tempOk = temperature.trim() !== '' && Number.isFinite(tempNum) && tempNum >= 0 && tempNum <= 2
  const topPOk = topP.trim() !== '' && Number.isFinite(topPNum) && topPNum >= 0 && topPNum <= 1
  const maxOk = maxNum === null || (Number.isInteger(maxNum) && maxNum > 0)
  const paramsOk = tempOk && topPOk && maxOk && modelValue.trim() !== ''

  // 记住当前这套参数：只存用户改过的值（model 为空 = 仍跟随目录首项，不把自动选中的值固化下来）
  useEffect(() => {
    if (userId !== undefined) writeSettings(userId, { model, system, temperature, topP, maxTokens })
  }, [userId, model, system, temperature, topP, maxTokens])

  const applyPreset = (p: Preset) => {
    setModel(p.model)
    setSystem(p.system)
    setTemperature(String(p.temperature))
    setTopP(String(p.top_p))
    setMaxTokens(p.max_tokens === null ? '' : String(p.max_tokens))
    setPresetName(p.name)
  }
  const currentPreset = (): Preset | null =>
    paramsOk && presetName.trim() !== ''
      ? { name: presetName.trim(), model: modelValue.trim(), system, temperature: tempNum, top_p: topPNum, max_tokens: maxNum }
      : null

  const options = (): SendOptions => ({ model: modelValue, system, temperature: tempNum, top_p: topPNum, max_tokens: maxNum })

  // 滚动：只在用户本来就贴着底部时跟随新内容；往上翻着读旧消息时不抢滚动条，改给"回到最新"按钮
  const logRef = useRef<HTMLDivElement>(null)
  const stick = useRef(true)
  const [away, setAway] = useState(false)
  const toBottom = () => {
    stick.current = true
    setAway(false)
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight })
  }
  useEffect(() => {
    if (stick.current) logRef.current?.scrollTo({ top: logRef.current.scrollHeight })
  }, [chat.turns])

  const submit = () => {
    if (!paramsOk || chat.busy || draft.trim() === '') return
    chat.send(draft, options())
    setDraft('')
    toBottom()
  }
  const regenerate = () => {
    if (!paramsOk || chat.busy) return
    chat.regenerate(options())
    toBottom()
  }

  const exportChat = () => {
    const usageLine = (turn: Turn) =>
      [turn.model, turn.usage && t('portal:playgroundUsage', { prompt: turn.usage.prompt_tokens, completion: turn.usage.completion_tokens })].filter(Boolean).join(' · ')
    const stamp = new Date().toISOString().slice(0, 16).replace(/[-:T]/g, '')
    downloadText(
      `playground-${stamp}.md`,
      conversationMarkdown(chat.turns, { title: t('portal:playgroundExportTitle'), user: t('portal:playgroundRoleUser'), assistant: t('portal:playgroundRoleAssistant'), usage: usageLine }),
    )
  }

  const starters = [t('portal:playgroundStarter1'), t('portal:playgroundStarter2'), t('portal:playgroundStarter3')]
  const lastAssistant = chat.turns.map((turn) => turn.role).lastIndexOf('assistant')

  return (
    <div className="flex min-h-0 flex-col gap-4 max-lg:overflow-y-auto lg:h-full">
      <PageHeader
        className="shrink-0"
        icon={FlaskConical}
        title={t('portal:playgroundTitle')}
        description={t('portal:playgroundDesc')}
        action={
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" disabled={chat.turns.length === 0} onClick={exportChat}>
              <FileDown className="h-3.5 w-3.5" />
              {t('portal:playgroundExport')}
            </Button>
            <Button variant="outline" size="sm" disabled={chat.turns.length === 0} onClick={chat.clear}>
              <Eraser className="h-3.5 w-3.5" />
              {t('portal:playgroundClear')}
            </Button>
          </div>
        }
      />

      <button
        type="button"
        aria-expanded={configOpen}
        aria-controls={ids.config}
        onClick={() => setConfigOpen((open) => !open)}
        className="flex shrink-0 items-center gap-2 rounded-lg border border-border bg-card px-3 py-2 text-left text-sm outline-none focus-visible:ring-2 focus-visible:ring-primary/40 lg:hidden"
      >
        <SlidersHorizontal aria-hidden className="h-4 w-4 shrink-0 text-muted-foreground" />
        <span className="shrink-0 font-medium">{t('portal:playgroundSettings')}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">{modelValue}</span>
        <ChevronDown aria-hidden className={cn('h-4 w-4 shrink-0 text-muted-foreground transition-transform', configOpen && 'rotate-180')} />
      </button>

      <div className="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)] gap-4 lg:grid-cols-[20rem_minmax(0,1fr)]">
        {/* 左栏：配置与预设 */}
        <aside
          id={ids.config}
          className={cn('min-h-0 flex-col gap-4 overflow-y-auto rounded-lg border border-border bg-card p-4 lg:flex', configOpen ? 'flex' : 'hidden')}
        >
          <Field label={t('portal:guideModel')} htmlFor={ids.model} hint={t('portal:playgroundModelHint')}>
            <Input
              id={ids.model}
              list={ids.list}
              value={modelValue}
              spellCheck={false}
              placeholder={t('portal:guideModelPlaceholder')}
              onChange={(e) => setModel(e.target.value)}
            />
            <datalist id={ids.list}>
              {available.map((m) => <option key={m} value={m} />)}
            </datalist>
          </Field>
          {modelEntry
            ? <ModelInfo model={modelEntry} factor={factor} />
            : pricing.isSuccess && modelValue.trim() !== '' && <p className="-mt-2 text-xs text-warning">{t('portal:playgroundModelUnlisted')}</p>}
          <Field label={t('portal:playgroundSystem')} htmlFor={ids.system}>
            <Textarea
              id={ids.system}
              rows={5}
              className="font-sans"
              value={system}
              placeholder={t('portal:playgroundSystemPlaceholder')}
              onChange={(e) => setSystem(e.target.value)}
            />
          </Field>
          <div className="grid grid-cols-3 gap-2">
            <Field label="temperature" htmlFor={ids.temp} error={tempOk ? null : t('portal:playgroundRange', { min: 0, max: 2 })}>
              <Input id={ids.temp} inputMode="decimal" value={temperature} aria-invalid={!tempOk} onChange={(e) => setTemperature(e.target.value)} />
            </Field>
            <Field label="top_p" htmlFor={ids.topP} error={topPOk ? null : t('portal:playgroundRange', { min: 0, max: 1 })}>
              <Input id={ids.topP} inputMode="decimal" value={topP} aria-invalid={!topPOk} onChange={(e) => setTopP(e.target.value)} />
            </Field>
            <Field label="max_tokens" htmlFor={ids.max} error={maxOk ? null : t('portal:playgroundPositiveInt')}>
              <Input id={ids.max} inputMode="numeric" value={maxTokens} placeholder={t('portal:playgroundAuto')} aria-invalid={!maxOk} onChange={(e) => setMaxTokens(e.target.value)} />
            </Field>
          </div>

          <div className="flex flex-col gap-2 border-t border-border pt-4">
            <span className="text-xs font-medium text-muted-foreground">{t('portal:playgroundPresets')}</span>
            <div className="flex items-end gap-2">
              <Field label={t('portal:playgroundPresetName')} htmlFor={ids.preset} className="flex-1">
                <Input id={ids.preset} value={presetName} placeholder={t('portal:playgroundPresetPlaceholder')} onChange={(e) => setPresetName(e.target.value)} />
              </Field>
              <Button
                variant="outline"
                size="sm"
                disabled={userId === undefined || currentPreset() === null}
                onClick={() => {
                  const p = currentPreset()
                  if (userId !== undefined && p) {
                    upsertPreset(userId, p)
                    toast.success(t('portal:playgroundPresetSaved', { name: p.name }))
                  }
                }}
              >
                <Save className="h-3.5 w-3.5" />
                {t('common:save')}
              </Button>
            </div>
            {userPresets.length > 0 && (
              <ul className="flex flex-col gap-1">
                {userPresets.map((p) => (
                  <li key={p.name} className="flex items-center gap-1 rounded-md border border-border px-2 py-1">
                    <button type="button" className="min-w-0 flex-1 truncate text-left text-sm hover:text-primary" onClick={() => applyPreset(p)}>
                      {p.name}
                      <span className="ml-2 font-mono text-xs text-muted-foreground">{p.model}</span>
                    </button>
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() => userId !== undefined && removePreset(userId, p.name)}
                    />
                  </li>
                ))}
              </ul>
            )}
            {(sitePresets.data?.data.length ?? 0) > 0 && (
              <>
                <span className="mt-2 text-xs font-medium text-muted-foreground">{t('portal:playgroundSitePresets')}</span>
                <ul className="flex flex-col gap-1">
                  {sitePresets.data?.data.map((p) => (
                    <li key={p.name} className="flex items-center gap-1 rounded-md border border-dashed border-border px-2 py-1">
                      <span className="min-w-0 flex-1 truncate text-sm">
                        {p.name}
                        <span className="ml-2 font-mono text-xs text-muted-foreground">{p.model}</span>
                      </span>
                      {/* 一键导入 = 应用到表单 + 存为本地预设，之后可自行改参数 */}
                      <IconButton
                        icon={Download}
                        label={t('portal:playgroundImport')}
                        onClick={() => {
                          const local = fromSitePreset(p)
                          applyPreset(local)
                          if (userId !== undefined) upsertPreset(userId, local)
                          toast.success(t('portal:playgroundImported', { name: p.name }))
                        }}
                      />
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
        </aside>

        {/* 右栏：对话 */}
        <section className="relative flex h-[min(78dvh,46rem)] min-h-[22rem] flex-col rounded-lg border border-border bg-card lg:h-auto lg:min-h-0">
          <div
            ref={logRef}
            onScroll={() => {
              const el = logRef.current
              if (!el) return
              const gap = el.scrollHeight - el.scrollTop - el.clientHeight
              stick.current = gap < 80
              setAway(gap >= 80)
            }}
            className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4"
            aria-live="polite"
          >
            {chat.turns.length === 0 ? (
              <div className="m-auto flex w-full max-w-md flex-col items-center gap-3 text-center">
                <span aria-hidden className="flex h-12 w-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Bot className="h-6 w-6" /></span>
                <p className="text-sm text-muted-foreground">{t('portal:playgroundEmpty')}</p>
                <ul className="flex w-full flex-col gap-2" aria-label={t('portal:playgroundStartersLabel')}>
                  {starters.map((starter) => (
                    <li key={starter}>
                      <button
                        type="button"
                        onClick={() => {
                          setDraft(starter)
                          document.getElementById(ids.input)?.focus()
                        }}
                        className="w-full rounded-lg border border-border bg-background px-3 py-2 text-left text-sm outline-none transition-colors hover:border-primary/40 hover:bg-accent/40 focus-visible:ring-2 focus-visible:ring-primary/40"
                      >
                        {starter}
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            ) : (
              chat.turns.map((turn, index) => (
                <TurnBubble
                  key={turn.id}
                  turn={turn}
                  locale={locale}
                  cost={turn.role === 'assistant' ? estimateCost(catalog.find((m) => m.model === turn.requested), factor, turn.usage) : null}
                  onRegenerate={index === lastAssistant && !chat.busy && paramsOk ? regenerate : undefined}
                />
              ))
            )}
          </div>
          {away && chat.turns.length > 0 && (
            <button
              type="button"
              onClick={toBottom}
              aria-label={t('portal:playgroundJumpLatest')}
              className="absolute bottom-24 right-5 inline-flex h-8 items-center gap-1 rounded-full border border-border bg-card px-3 text-xs shadow-md outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-primary/40"
            >
              <ArrowDown aria-hidden className="h-3.5 w-3.5" />
              {t('portal:playgroundJumpLatest')}
            </button>
          )}
          <form
            className="flex items-end gap-2 border-t border-border p-3"
            onSubmit={(e) => {
              e.preventDefault()
              submit()
            }}
          >
            <Textarea
              id={ids.input}
              rows={2}
              className="flex-1 font-sans text-sm"
              value={draft}
              placeholder={t('portal:playgroundInputPlaceholder')}
              aria-label={t('portal:playgroundInputPlaceholder')}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                // 回车发送、Shift+回车换行（与主流聊天客户端一致）
                if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault()
                  submit()
                }
              }}
            />
            {chat.busy ? (
              <Button type="button" variant="outline" onClick={chat.stop}>
                <Square className="h-3.5 w-3.5" />
                {t('portal:playgroundStop')}
              </Button>
            ) : (
              <Button type="submit" disabled={!paramsOk || draft.trim() === ''}>
                <Send className="h-3.5 w-3.5" />
                {t('portal:playgroundSend')}
              </Button>
            )}
          </form>
        </section>
      </div>
    </div>
  )
}

function TurnBubble({ turn, locale, cost, onRegenerate }: { turn: Turn; locale: string; cost: number | null; onRegenerate?: () => void }) {
  const { t } = useTranslation()
  if (turn.role === 'user') {
    return (
      <div className="flex justify-end" data-role="user">
        <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-2xl rounded-tr-sm bg-primary px-3.5 py-2 text-sm text-primary-foreground [overflow-wrap:anywhere]">{turn.content}</div>
      </div>
    )
  }
  const done = !turn.streaming && turn.error === undefined
  const speed = turn.usage ? outputSpeed(turn.usage.completion_tokens, turn.ttftMs, turn.durationMs) : null
  return (
    <div className="flex items-start gap-2.5" data-role="assistant">
      <span aria-hidden className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary"><Bot className="h-4 w-4" /></span>
      <div className="min-w-0 max-w-[88%] space-y-1.5">
        <div className="min-w-0 rounded-2xl rounded-tl-sm bg-accent/60 px-3.5 py-2.5 text-sm">
          {turn.reasoning !== undefined && turn.reasoning !== '' && (
            <details className="mb-2 text-xs text-muted-foreground">
              <summary className="cursor-pointer">{t('portal:playgroundReasoning')}</summary>
              <div className="mt-1 whitespace-pre-wrap">{turn.reasoning}</div>
            </details>
          )}
          <Markdown source={turn.content} />
          {turn.streaming && <span className="ml-0.5 inline-block h-3.5 w-1.5 animate-pulse bg-current align-text-bottom" aria-hidden />}
          {turn.error !== undefined && (
            <p className="mt-1 text-xs text-destructive" role="alert">
              {describeError(new ApiError(turn.error.status, turn.error.code, turn.error.param))}
            </p>
          )}
        </div>
        {(done || turn.error !== undefined) && (
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-1 text-[11px] text-muted-foreground">
            {done && turn.model && <Badge variant="outline" className="font-mono">{turn.model}</Badge>}
            {done && turn.usage && (
              <span>
                {t('portal:playgroundUsage', {
                  prompt: formatCount(turn.usage.prompt_tokens, locale),
                  completion: formatCount(turn.usage.completion_tokens, locale),
                })}
                {turn.usage.cached_tokens > 0 && ` · ${t('portal:playgroundCached', { n: formatCount(turn.usage.cached_tokens, locale) })}`}
              </span>
            )}
            {done && turn.ttftMs !== undefined && <span title={t('portal:playgroundTtftHint')}>{t('portal:playgroundTtft', { time: formatDuration(turn.ttftMs) })}</span>}
            {done && speed !== null && <span>{t('portal:playgroundSpeed', { n: speed.toFixed(speed >= 100 ? 0 : 1) })}</span>}
            {done && cost !== null && <span title={t('portal:playgroundCostHint')}>{t('portal:playgroundCost', { cost: formatMoney(cost, locale) })}</span>}
            <span className="ml-auto flex items-center gap-0.5">
              {done && turn.content !== '' && <CopyButton value={turn.content} label={t('portal:playgroundCopyReply')} size="xs" />}
              {onRegenerate && (
                <IconButton icon={RotateCcw} label={turn.error !== undefined ? t('portal:playgroundRetry') : t('portal:playgroundRegenerate')} onClick={onRegenerate} className="h-6 w-6" />
              )}
            </span>
          </div>
        )}
      </div>
    </div>
  )
}
