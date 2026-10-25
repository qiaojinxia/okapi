import { useState } from 'react'
import { useDryRun } from '@/features/compat/api'
import type { CompatRule, DryRunBody, DryRunResult } from '@/features/compat/types'

export type ReplayPhase = 'request' | 'response' | 'error' | 'capability'

export const REPLAY_PHASES: ReplayPhase[] = ['request', 'response', 'error', 'capability']

export interface ReplayForm {
  phase: ReplayPhase
  model: string
  dialect: string
  operation: string
  stream: boolean
  host: string
  errorStatus: string
  /// 各阶段的样本文本：请求体 / 响应（或一帧）/ 错误体。
  request: string
  response: string
  error: string
}

export const DEFAULT_FORM: ReplayForm = {
  phase: 'request',
  model: 'gpt-4o',
  dialect: 'openai.chat',
  operation: 'chat',
  stream: false,
  host: '',
  errorStatus: '400',
  request: '{\n  "model": "gpt-4o",\n  "messages": [{ "role": "user", "content": "hi" }],\n  "user": "u-1"\n}',
  response:
    '{\n  "id": "chatcmpl-1",\n  "object": "chat.completion",\n  "choices": [\n    { "index": 0, "message": { "role": "assistant", "content": "hello" }, "finish_reason": "stop" }\n  ],\n  "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }\n}',
  error: '{\n  "error": { "message": "unsupported parameter: user", "type": "invalid_request_error" }\n}',
}

/// 样本文本 → JSON；不是合法 JSON 返回 undefined（错误体允许是纯文本，那是调用方的事）。
function parseSample(text: string): unknown {
  try {
    return JSON.parse(text) as unknown
  } catch {
    return undefined
  }
}

/// 试跑的目标：渠道（用它已发布的厂商 / 护栏）或一个厂商（档案叠加的内置厂商）；护栏什么都不带。
export interface ReplayTarget {
  channelId?: number
  vendor?: string
}

export interface Replay {
  form: ReplayForm
  patch: (change: Partial<ReplayForm>) => void
  /// 样本不合法时的原因（`request` / `response` / `status`），能跑时为 null。
  problem: string | null
  run: () => void
  running: boolean
  result: DryRunResult | undefined
  failure: unknown
}

/// 试跑状态：表单 + 最近一次结果。蓝图页签用结果给节点打「命中 / 跳过」标记，所以放在外层。
/// 试跑用的是编辑器里的规则（叠在所选厂商之上当渠道层），不是线上那一版。
export function useReplay(target: ReplayTarget, rules: CompatRule[] | null): Replay {
  const [form, setForm] = useState<ReplayForm>(DEFAULT_FORM)
  const dryRun = useDryRun()

  const status = Number(form.errorStatus)
  const problem = (() => {
    if (rules === null) return 'rules'
    if (form.phase === 'request' && parseSample(form.request) === undefined) return 'request'
    if (form.phase === 'response' && parseSample(form.response) === undefined) return 'response'
    if (form.phase === 'error' && !(Number.isInteger(status) && status >= 100 && status <= 599)) return 'status'
    if (form.phase === 'error' && form.request.trim() !== '' && parseSample(form.request) === undefined) return 'request'
    return null
  })()

  const run = () => {
    if (problem !== null || rules === null) return
    const body: DryRunBody = {
      ...(target.channelId === undefined ? {} : { channel_id: target.channelId }),
      ...(target.vendor === undefined ? {} : { vendor: target.vendor }),
      rules,
      phase: form.phase,
      context: {
        model: form.model.trim(),
        dialect: form.dialect,
        operation: form.operation,
        stream: form.stream,
        ...(form.host.trim() === '' ? {} : { host: form.host.trim() }),
      },
    }
    if (form.phase === 'request') body.request = parseSample(form.request)
    if (form.phase === 'response') body.response = parseSample(form.response)
    if (form.phase === 'error') {
      body.error = { status, body: parseSample(form.error) ?? form.error }
      if (form.request.trim() !== '') body.request = parseSample(form.request)
    }
    dryRun.mutate(body)
  }

  return {
    form,
    patch: (change) => setForm((current) => ({ ...current, ...change })),
    problem,
    run,
    running: dryRun.isPending,
    result: dryRun.data,
    failure: dryRun.error,
  }
}
