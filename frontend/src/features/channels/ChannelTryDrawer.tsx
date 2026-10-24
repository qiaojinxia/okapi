import { Play, Square } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Label, Textarea } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ProviderChip } from '@/features/channels/ProviderChip'
import type { ChannelRow } from '@/features/channels/types'
import { type ChatUsage, streamSse } from '@/features/playground/chat-stream'
import { ApiError, getKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { maskAccount, maskChannelName } from '@/lib/mask'

type Phase = 'idle' | 'running' | 'done' | 'failed'

/// 账号测试：钉住这条渠道（可选某个账号）发一次真实流式请求，实时显示模型输出。
/// 与列表上的「测试」不同——那个只验凭证连通，这里看模型到底能不能正常说话。
export function ChannelTryDrawer({ channel, onClose }: { channel: ChannelRow; onClose: () => void }) {
  const { t } = useTranslation()
  // 渠道名就是订阅账号（邮箱，或等于某把 key 的账号标识）时遮住中间，其余原样
  const shownName = maskChannelName(channel.name, channel.keys.map((k) => k.account_label))
  const [model, setModel] = useState(channel.models[0] ?? '')
  const [keyId, setKeyId] = useState('')
  const [prompt, setPrompt] = useState(() => t('admin:channelTryPromptDefault'))
  const [phase, setPhase] = useState<Phase>('idle')
  const [output, setOutput] = useState('')
  const [reasoning, setReasoning] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [usage, setUsage] = useState<ChatUsage | null>(null)
  const [timing, setTiming] = useState<{ first: number | null; total: number | null }>({ first: null, total: null })
  const stop = useRef<(() => void) | null>(null)
  useEffect(() => () => stop.current?.(), [])

  const start = () => {
    const key = getKey()
    if (key === null || model === '') return
    stop.current?.()
    setPhase('running')
    setOutput('')
    setReasoning('')
    setError(null)
    setUsage(null)
    setTiming({ first: null, total: null })
    const began = performance.now()
    let first: number | null = null
    stop.current = streamSse(`/admin/channels/${channel.id}/try`, {
      model, prompt, ...(keyId !== '' ? { key_id: Number(keyId) } : {}),
    }, key, {
      onDelta: (delta) => {
        if (first === null && (delta.content !== '' || delta.reasoning !== '')) {
          first = Math.round(performance.now() - began)
          setTiming((v) => ({ ...v, first }))
        }
        if (delta.content !== '') setOutput((v) => v + delta.content)
        if (delta.reasoning !== '') setReasoning((v) => v + delta.reasoning)
        if (delta.usage !== null) setUsage(delta.usage)
      },
      onDone: () => {
        setTiming((v) => ({ ...v, total: Math.round(performance.now() - began) }))
        setPhase('done')
      },
      onError: (code, status, param, message) => {
        setTiming((v) => ({ ...v, total: Math.round(performance.now() - began) }))
        setError([describeError(new ApiError(status, code, param)), message].filter(Boolean).join(' · '))
        setPhase('failed')
      },
    })
  }
  const cancel = () => {
    stop.current?.()
    stop.current = null
    setPhase(output === '' ? 'idle' : 'done')
  }

  const status = {
    idle: null,
    running: <Badge variant="info" dot>{t('admin:channelTryRunning')}</Badge>,
    done: <Badge variant="success" dot>{t('admin:channelTryOk')}</Badge>,
    failed: <Badge variant="destructive" dot>{t('admin:channelTryFailed')}</Badge>,
  }[phase]

  return (
    <Drawer
      open
      onClose={onClose}
      size="lg"
      title={t('admin:channelTryTitle', { name: shownName })}
      description={t('admin:channelTryDescription')}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>{t('common:close')}</Button>
          {phase === 'running' ? (
            <Button variant="outline" onClick={cancel}><Square className="h-4 w-4" />{t('admin:channelTryStop')}</Button>
          ) : (
            <Button onClick={start} disabled={model === ''}><Play className="h-4 w-4" />{t('admin:channelTryStart')}</Button>
          )}
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <ProviderChip provider={channel.provider} />
          <span className="text-muted-foreground tabular-nums">#{channel.id}</span>
          <span className="font-medium">{shownName}</span>
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="try-model">{t('admin:channelTryModel')}</Label>
            <Select id="try-model" value={model} onChange={setModel}
              options={channel.models.map((m) => ({ value: m, label: m }))} />
          </div>
          {channel.keys.length > 1 && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="try-key">{t('admin:channelTryAccount')}</Label>
              <Select id="try-key" value={keyId} onChange={setKeyId} options={[
                { value: '', label: t('admin:channelTryAnyAccount') },
                ...channel.keys.map((k) => ({ value: String(k.id), label: k.account_label ? `#${k.id} · ${maskAccount(k.account_label)}` : `#${k.id}` })),
              ]} />
            </div>
          )}
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="try-prompt">{t('admin:channelTryPrompt')}</Label>
          <Textarea id="try-prompt" rows={3} value={prompt} maxLength={4000}
            onChange={(e) => setPrompt(e.target.value)} />
        </div>
        <div className="flex flex-col gap-2">
          <div className="flex min-h-6 flex-wrap items-center gap-2 text-xs text-muted-foreground tabular-nums">
            <span className="text-sm font-medium text-foreground">{t('admin:channelTryOutput')}</span>
            {status}
            {timing.first !== null && <span>{t('admin:channelTryFirst', { ms: timing.first })}</span>}
            {timing.total !== null && <span>{t('admin:channelTryTotal', { ms: timing.total })}</span>}
            {usage !== null && <span>{t('admin:channelTryTokens', { input: usage.prompt_tokens, output: usage.completion_tokens })}</span>}
          </div>
          <div
            role="log"
            aria-live="polite"
            aria-label={t('admin:channelTryOutput')}
            className="min-h-40 max-h-96 overflow-y-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-muted/40 p-3 text-sm leading-6"
          >
            {reasoning !== '' && <div className="mb-2 border-l-2 border-border pl-2 text-xs italic text-muted-foreground">{reasoning}</div>}
            {output}
            {phase === 'running' && <span className="ml-0.5 inline-block h-4 w-1.5 animate-pulse bg-primary/70 align-middle" aria-hidden />}
            {phase === 'idle' && output === '' && <span className="text-muted-foreground">{t('admin:channelTryIdle')}</span>}
            {error !== null && <p className="text-destructive">{error}</p>}
          </div>
          <p className="text-xs text-muted-foreground">{t('admin:channelTryBilling')}</p>
        </div>
      </div>
    </Drawer>
  )
}
