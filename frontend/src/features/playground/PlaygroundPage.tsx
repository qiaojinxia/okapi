import { useQuery } from '@tanstack/react-query'
import { Bot, Download, Eraser, FlaskConical, Save, Send, Square, Trash2 } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { Turn } from './use-chat-stream'
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
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Field } from '@/components/ui/field'
import { IconButton } from '@/components/ui/icon-button'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { Textarea } from '@/components/ui/textarea'
import { toast } from '@/components/ui/toast'
import { isAvailable } from '@/features/public-pricing/catalog-data'
import type { PricingGroup, PricingModel } from '@/features/public-pricing/types'
import { useMe } from '@/hooks/use-auth'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount } from '@/lib/money'
import { qk } from '@/lib/query-keys'

/// 试用台（IMPLEMENTATION §11.39）：左栏模型 + 系统提示词 + 采样参数，右栏流式对话。
///
/// 请求经同源中继打到数据面同一处理器，账单落在登录 key 上——这里看到的通不通、多少钱，
/// 就是用户接 SDK 会看到的。助手正文按纯文本保留换行渲染，不引入 markdown 库。
export function PlaygroundPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const me = useMe()
  const ids = { model: useId(), list: useId(), system: useId(), temp: useId(), topP: useId(), max: useId(), preset: useId(), input: useId() }
  const chat = useChatStream()

  const [model, setModel] = useState('')
  const [system, setSystem] = useState('')
  const [temperature, setTemperature] = useState(String(DEFAULT_TEMPERATURE))
  const [topP, setTopP] = useState(String(DEFAULT_TOP_P))
  const [maxTokens, setMaxTokens] = useState('')
  const [presetName, setPresetName] = useState('')
  const [draft, setDraft] = useState('')

  // 模型候选：本分组可用（模型广场与接入指南同口径），允许手输——目录缓存可能落后于站长刚加的模型
  const pricing = useQuery({
    queryKey: qk.publicPricing,
    queryFn: () => apiFetch<{ models: PricingModel[]; groups: PricingGroup[] }>('/api/pricing'),
    staleTime: 60_000,
  })
  const group = me.data?.group ?? ''
  const available = (pricing.data?.models ?? [])
    .filter((m) => isAvailable(m, group))
    .map((m) => m.model)
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
  const modelValue = model !== '' ? model : (available[0] ?? '')

  const sitePresets = useSitePresets()
  const userPresets = useUserPresets(me.data?.user_id)

  const tempNum = Number(temperature)
  const topPNum = Number(topP)
  const maxNum = maxTokens.trim() === '' ? null : Number(maxTokens)
  const tempOk = temperature.trim() !== '' && Number.isFinite(tempNum) && tempNum >= 0 && tempNum <= 2
  const topPOk = topP.trim() !== '' && Number.isFinite(topPNum) && topPNum >= 0 && topPNum <= 1
  const maxOk = maxNum === null || (Number.isInteger(maxNum) && maxNum > 0)
  const paramsOk = tempOk && topPOk && maxOk && modelValue.trim() !== ''

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

  const submit = () => {
    if (!paramsOk || chat.busy || draft.trim() === '') return
    chat.send(draft, { model: modelValue, system, temperature: tempNum, top_p: topPNum, max_tokens: maxNum })
    setDraft('')
  }

  // 新内容到达时滚到底：流式期间用户几乎总是在看最后一行
  const logRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight })
  }, [chat.turns])

  const userId = me.data?.user_id

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <PageHeader
        className="shrink-0"
        icon={FlaskConical}
        title={t('portal:playgroundTitle')}
        description={t('portal:playgroundDesc')}
        action={
          <Button variant="outline" size="sm" disabled={chat.turns.length === 0} onClick={chat.clear}>
            <Eraser className="h-3.5 w-3.5" />
            {t('portal:playgroundClear')}
          </Button>
        }
      />

      <div className="grid min-h-0 flex-1 gap-4 lg:grid-cols-[20rem_minmax(0,1fr)]">
        {/* 左栏：配置与预设 */}
        <aside className="flex min-h-0 flex-col gap-4 overflow-y-auto rounded-lg border border-border bg-card p-4">
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
        <section className="flex min-h-0 flex-col rounded-lg border border-border bg-card">
          <div ref={logRef} className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4" aria-live="polite">
            {chat.turns.length === 0 ? (
              <div className="m-auto flex flex-col items-center gap-2 text-center text-muted-foreground">
                <Bot className="h-8 w-8" />
                <p className="text-sm">{t('portal:playgroundEmpty')}</p>
              </div>
            ) : (
              chat.turns.map((turn) => <TurnBubble key={turn.id} turn={turn} locale={locale} />)
            )}
          </div>
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

function TurnBubble({ turn, locale }: { turn: Turn; locale: string }) {
  const { t } = useTranslation()
  const mine = turn.role === 'user'
  return (
    <div className={mine ? 'flex justify-end' : 'flex justify-start'} data-role={turn.role}>
      <div
        className={
          mine
            ? 'max-w-[80%] rounded-lg bg-primary px-3 py-2 text-sm text-primary-foreground whitespace-pre-wrap break-words'
            : 'max-w-[80%] rounded-lg bg-accent/60 px-3 py-2 text-sm whitespace-pre-wrap break-words'
        }
      >
        {turn.reasoning !== undefined && turn.reasoning !== '' && (
          <details className="mb-2 text-xs text-muted-foreground">
            <summary className="cursor-pointer">{t('portal:playgroundReasoning')}</summary>
            <div className="mt-1 whitespace-pre-wrap">{turn.reasoning}</div>
          </details>
        )}
        {turn.content}
        {turn.streaming && <span className="ml-0.5 inline-block h-3.5 w-1.5 animate-pulse bg-current align-text-bottom" aria-hidden />}
        {turn.error !== undefined && (
          <p className="mt-1 text-xs text-destructive" role="alert">
            {describeError(new ApiError(turn.error.status, turn.error.code, turn.error.param))}
          </p>
        )}
        {!mine && !turn.streaming && turn.error === undefined && (turn.usage || turn.model) && (
          <div className="mt-2 flex flex-wrap items-center gap-1 text-[11px] text-muted-foreground">
            {turn.model && <Badge variant="outline" className="font-mono">{turn.model}</Badge>}
            {turn.usage && (
              <span>
                {t('portal:playgroundUsage', {
                  prompt: formatCount(turn.usage.prompt_tokens, locale),
                  completion: formatCount(turn.usage.completion_tokens, locale),
                })}
                {turn.usage.cached_tokens > 0 && ` · ${t('portal:playgroundCached', { n: formatCount(turn.usage.cached_tokens, locale) })}`}
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  )
}
