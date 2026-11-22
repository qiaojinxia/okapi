import { parseSseChunk } from '@/features/playground/chat-stream'
import { apiFetch } from '@/lib/api'

export type CheckStatus = 'pass' | 'warn' | 'fail' | 'skipped' | 'error'
export type OverallVerdict = 'verified' | 'warning' | 'mismatch' | 'error'
export type TokenizerVerdictKind = 'exact' | 'related' | 'unknown'
export type ClaimStatus = 'consistent' | 'inconsistent' | 'uncertain'
export type FingerprintDecision = 'scored' | 'partial' | 'unscorable' | 'not_model'
export type ProbabilityStatus = 'unavailable' | 'reference_calibrated'

export interface BankStatus {
  loaded: boolean
  cache_exists: boolean
  built_at: string | null
  model_count: number | null
  tokenizer_classes: number
  tokenizer_probes: number
  sources: string[]
}

export interface VerifyCheckSelection {
  protocol: boolean
  tokenizer: boolean
  fingerprint: boolean
}

export interface ModelNameCheck {
  declared_model: string
  upstream_model: string
  response_model?: string | null
  declared_family?: string | null
  response_family?: string | null
  status: CheckStatus
  detail?: string | null
}

export interface ToolCallCheck {
  status: CheckStatus
  nonce: string
  tool_called: boolean
  nonce_matched: boolean
  arguments_valid_json: boolean
  latency_ms: number
  detail?: string | null
}

export interface SignatureCheck {
  status: CheckStatus
  applicable: boolean
  has_thinking_block: boolean
  has_signature: boolean
  signature_len?: number | null
  thinking_mode?: string | null
  latency_ms: number
  detail?: string | null
}

export interface ProtocolCheckResult {
  model_name: ModelNameCheck
  tool_call: ToolCallCheck
  signature: SignatureCheck
}

export interface ClassScore {
  id: string
  lab: string
  lab_name: string
  series: string
  series_zh: string
  exact: number
  related: number
  matched: number
  compared: number
  overhead: number
}

export interface LabScore {
  lab: string
  lab_name: string
  total: number
}

export interface TokenizerPosterior {
  classes: ClassScore[]
  labs: LabScore[]
  unknown: number
  alien: number
  answered: number
}

export interface ClaimCheck {
  expected: string[]
  vendor: string
  first_party: boolean
  probability: number
  status: ClaimStatus
}

export interface TokenizerVerdict {
  kind: TokenizerVerdictKind
  top: ClassScore
  confidence: number
  claim: ClaimCheck | null
}

export interface TokenizerReport {
  posterior: TokenizerPosterior
  verdict: TokenizerVerdict
  probes_run: number
  baseline_tokens: number
  baseline_drift?: number | null
  latency_ms: number
}

export interface ChallengeSampleSummary {
  index: number
  expected_count: number
  parsed_numbers: number
  minimum_numbers: number
  accepted: boolean
  latency_ms: number
  response_model?: string | null
  error?: string | null
}

export interface FingerprintCandidate {
  model: string
  display_name: string
  family: string
  family_name: string
  score: number
  probability: number | null
}

export interface FamilyProbability {
  family: string
  family_name: string
  probability: number
}

export interface FingerprintReport {
  bank_built_at: string
  bank_model_count: number
  decision: FingerprintDecision
  prediction: string
  prediction_name: string
  family_prediction: string
  family_prediction_name: string
  declared_family: string | null
  family_matches: boolean | null
  declared_in_bank: boolean
  top_probability: number | null
  top_family_probability: number | null
  probability_status: ProbabilityStatus
  used_outputs: number
  model_like: boolean | null
  evidence_label: string
  evidence_reason: string
  top_matches: FingerprintCandidate[]
  family_probabilities: FamilyProbability[]
  samples: ChallengeSampleSummary[]
}

export interface VerifyWarning {
  code: string
  severity: string
  message: string
}

export interface VerifyReport {
  channel_id: number
  provider: string
  model: string
  upstream_model: string
  key_id?: number | null
  mimic_profile_active: boolean
  verdict: OverallVerdict
  warnings: VerifyWarning[]
  protocol?: ProtocolCheckResult | null
  tokenizer?: TokenizerReport | null
  fingerprint?: FingerprintReport | null
  elapsed_ms: number
}

export type VerifyEvent =
  | {
      type: 'init'
      channel_id: number
      provider: string
      model: string
      upstream_model: string
      key_id?: number | null
      mimic_profile_active: boolean
      checks: VerifyCheckSelection
      bank: BankStatus
    }
  | {
      type: 'stage'
      stage: string
      status: string
      detail?: string | null
      completed?: number | null
      total?: number | null
    }
  | { type: 'protocol_result'; result: ProtocolCheckResult }
  | { type: 'tokenizer_progress'; probes_run: number; max_probes: number; posterior: TokenizerPosterior }
  | { type: 'tokenizer_result'; result: TokenizerReport }
  | { type: 'fingerprint_sample'; sample: ChallengeSampleSummary }
  | { type: 'fingerprint_result'; result: FingerprintReport }
  | { type: 'done'; report: VerifyReport }
  | { type: 'error'; stage: string; message: string }

export interface VerifyRequest {
  model: string
  key_id?: number
  checks: VerifyCheckSelection
  max_tokenizer_probes?: number
}

export function fetchVerifyBank(): Promise<BankStatus> {
  return apiFetch<BankStatus>('/admin/channels/verify/bank')
}

export function refreshVerifyBank(): Promise<BankStatus> {
  return apiFetch<BankStatus>('/admin/channels/verify/bank/refresh', { method: 'POST' })
}

interface ErrorEnvelope {
  error?: { code?: string; type?: string; param?: string; message?: string }
}

/// 发起 SSE 模型验真流。返回中断函数。
export function streamChannelVerify(
  channelId: number,
  body: VerifyRequest,
  key: string,
  callbacks: {
    onEvent: (event: VerifyEvent) => void
    onEnd: () => void
    onError: (code: string, status: number, param?: string, message?: string) => void
  },
): () => void {
  const controller = new AbortController()
  void (async () => {
    let resp: Response
    try {
      resp = await fetch(`/admin/channels/${channelId}/verify`, {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${key}`,
          'Content-Type': 'application/json',
          Accept: 'text/event-stream',
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      })
    } catch {
      if (!controller.signal.aborted) callbacks.onError('network_error', 0)
      return
    }
    if (!resp.ok) {
      let code = `http_${resp.status}`
      let param: string | undefined
      let message: string | undefined
      try {
        const envelope = (await resp.json()) as ErrorEnvelope
        code = envelope.error?.code ?? envelope.error?.type ?? code
        param = envelope.error?.param
        if (typeof envelope.error?.message === 'string' && envelope.error.message.trim() !== '') {
          message = envelope.error.message.trim().slice(0, 1000)
        }
      } catch {
        // non-JSON error
      }
      callbacks.onError(code, resp.status, param, message)
      return
    }
    const reader = resp.body?.getReader()
    if (!reader) {
      callbacks.onError('stream_unavailable', 0)
      return
    }
    const decoder = new TextDecoder()
    let carry = ''
    try {
      for (;;) {
        const { value, done } = await reader.read()
        if (done) break
        const parsed = parseSseChunk(carry, decoder.decode(value, { stream: true }))
        carry = parsed.carry
        for (const raw of parsed.events) {
          try {
            callbacks.onEvent(JSON.parse(raw) as VerifyEvent)
          } catch {
            // ignore malformed frames
          }
        }
      }
      if (carry.trim() !== '') {
        const tail = parseSseChunk(carry, '\n\n')
        for (const raw of tail.events) {
          try {
            callbacks.onEvent(JSON.parse(raw) as VerifyEvent)
          } catch {
            // ignore trailing non-JSON
          }
        }
      }
      callbacks.onEnd()
    } catch {
      if (!controller.signal.aborted) callbacks.onError('stream_error', 0)
    }
  })()
  return () => controller.abort()
}
