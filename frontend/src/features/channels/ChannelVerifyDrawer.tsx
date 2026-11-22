import {
  AlertTriangle,
  Binary,
  CheckCircle2,
  Cpu,
  Fingerprint,
  Info,
  Loader2,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
  ShieldX,
  Square,
} from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Drawer } from '@/components/ui/drawer'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { toast } from '@/components/ui/toast'
import { ProviderChip, ProviderMark } from '@/features/channels/ProviderChip'
import type { ChannelRow } from '@/features/channels/types'
import {
  type ChallengeSampleSummary,
  type CheckStatus,
  type ClaimStatus,
  type FingerprintReport,
  type OverallVerdict,
  type ProtocolCheckResult,
  type TokenizerPosterior,
  type TokenizerReport,
  type VerifyCheckSelection,
  type VerifyEvent,
  type VerifyReport,
  fetchVerifyBank,
  refreshVerifyBank,
  streamChannelVerify,
} from '@/features/channels/channel-verify-api'
import { ApiError, getKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { maskAccount, maskChannelName } from '@/lib/mask'
import { cn } from '@/lib/utils'

type Phase = 'idle' | 'running' | 'done' | 'failed'

function pct(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return '—'
  return `${(v * 100).toFixed(1)}%`
}

function statusVariant(status: CheckStatus): 'success' | 'warning' | 'destructive' | 'muted' {
  switch (status) {
    case 'pass':
      return 'success'
    case 'warn':
      return 'warning'
    case 'fail':
    case 'error':
      return 'destructive'
    case 'skipped':
      return 'muted'
  }
}

function claimVariant(status: ClaimStatus): 'success' | 'warning' | 'destructive' {
  switch (status) {
    case 'consistent':
      return 'success'
    case 'uncertain':
      return 'warning'
    case 'inconsistent':
      return 'destructive'
  }
}

function verdictStyle(verdict: OverallVerdict): {
  box: string
  badge: 'success' | 'warning' | 'destructive'
  Icon: typeof ShieldCheck
} {
  switch (verdict) {
    case 'verified':
      return {
        box: 'border-success/35 bg-success/[0.07] text-foreground',
        badge: 'success',
        Icon: ShieldCheck,
      }
    case 'warning':
      return {
        box: 'border-warning/40 bg-warning/[0.08] text-foreground',
        badge: 'warning',
        Icon: ShieldAlert,
      }
    case 'mismatch':
    case 'error':
      return {
        box: 'border-destructive/40 bg-destructive/[0.07] text-foreground',
        badge: 'destructive',
        Icon: ShieldX,
      }
  }
}

/// 渠道模型验真抽屉：整合 L1 协议/能力探针、贝叶斯开集 Tokenizer 指纹与 L2 ModelTrace 行为指纹归因。
export function ChannelVerifyDrawer({ channel, onClose }: { channel: ChannelRow; onClose: () => void }) {
  const { t, i18n } = useTranslation()
  const isZh = i18n.language.toLowerCase().startsWith('zh')
  const queryClient = useQueryClient()
  const shownName = maskChannelName(channel.name, channel.keys.map((k) => k.account_label))

  const [model, setModel] = useState(channel.models[0] ?? '')
  const [keyId, setKeyId] = useState('')
  const [checks, setChecks] = useState<VerifyCheckSelection>({
    protocol: true,
    tokenizer: true,
    fingerprint: true,
  })
  const [phase, setPhase] = useState<Phase>('idle')
  const [stageNote, setStageNote] = useState<string | null>(null)
  const [streamError, setStreamError] = useState<string | null>(null)
  const [stageErrors, setStageErrors] = useState<Array<{ stage: string; message: string }>>([])
  const [upstreamModel, setUpstreamModel] = useState<string | null>(null)
  const [mimicActive, setMimicActive] = useState(false)
  const [protocol, setProtocol] = useState<ProtocolCheckResult | null>(null)
  const [tokenizerLive, setTokenizerLive] = useState<{
    probesRun: number
    maxProbes: number
    posterior: TokenizerPosterior
  } | null>(null)
  const [tokenizer, setTokenizer] = useState<TokenizerReport | null>(null)
  const [samples, setSamples] = useState<ChallengeSampleSummary[]>([])
  const [fingerprint, setFingerprint] = useState<FingerprintReport | null>(null)
  const [report, setReport] = useState<VerifyReport | null>(null)

  const stopRef = useRef<(() => void) | null>(null)
  useEffect(() => () => stopRef.current?.(), [])

  const bank = useQuery({
    queryKey: ['admin', 'verify-bank'],
    queryFn: fetchVerifyBank,
    staleTime: 60_000,
  })

  const refreshBank = useMutation({
    mutationFn: refreshVerifyBank,
    onSuccess: (next) => {
      queryClient.setQueryData(['admin', 'verify-bank'], next)
      toast.success(t('admin:channelVerifyBankRefreshed'))
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const anyCheckSelected = checks.protocol || checks.tokenizer || checks.fingerprint

  const handleEvent = (ev: VerifyEvent) => {
    switch (ev.type) {
      case 'init':
        setUpstreamModel(ev.upstream_model)
        setMimicActive(ev.mimic_profile_active)
        queryClient.setQueryData(['admin', 'verify-bank'], ev.bank)
        break
      case 'stage': {
        const label = t(`admin:channelVerifyStage_${ev.stage}`, { defaultValue: ev.stage })
        if (ev.completed !== null && ev.completed !== undefined && ev.total) {
          setStageNote(`${label} (${ev.completed}/${ev.total})`)
        } else if (ev.status === 'loading_bank') {
          setStageNote(t('admin:channelVerifyLoadingBank'))
        } else {
          setStageNote(label)
        }
        break
      }
      case 'protocol_result':
        setProtocol(ev.result)
        break
      case 'tokenizer_progress':
        setTokenizerLive({
          probesRun: ev.probes_run,
          maxProbes: ev.max_probes,
          posterior: ev.posterior,
        })
        setStageNote(`${t('admin:channelVerifyStage_tokenizer')} (${ev.probes_run}/${ev.max_probes})`)
        break
      case 'tokenizer_result':
        setTokenizer(ev.result)
        setTokenizerLive(null)
        break
      case 'fingerprint_sample':
        setSamples((prev) => {
          const next = [...prev.filter((s) => s.index !== ev.sample.index), ev.sample]
          return next.sort((a, b) => a.index - b.index)
        })
        break
      case 'fingerprint_result':
        setFingerprint(ev.result)
        setSamples(ev.result.samples)
        void queryClient.invalidateQueries({ queryKey: ['admin', 'verify-bank'] })
        break
      case 'error':
        setStageErrors((prev) => [...prev, { stage: ev.stage, message: ev.message }])
        break
      case 'done':
        setReport(ev.report)
        setPhase('done')
        setStageNote(null)
        break
    }
  }

  const start = () => {
    const key = getKey()
    if (key === null || model === '' || !anyCheckSelected) return
    stopRef.current?.()
    setPhase('running')
    setStageNote(t('admin:channelVerifyStarting'))
    setStreamError(null)
    setStageErrors([])
    setUpstreamModel(null)
    setMimicActive(false)
    setProtocol(null)
    setTokenizerLive(null)
    setTokenizer(null)
    setSamples([])
    setFingerprint(null)
    setReport(null)

    stopRef.current = streamChannelVerify(
      channel.id,
      {
        model,
        checks,
        ...(keyId !== '' ? { key_id: Number(keyId) } : {}),
      },
      key,
      {
        onEvent: handleEvent,
        onEnd: () => {
          setPhase((curr) => (curr === 'running' ? 'done' : curr))
          setStageNote(null)
        },
        onError: (code, status, param, message) => {
          setStreamError(
            [describeError(new ApiError(status, code, param)), message].filter(Boolean).join(' · '),
          )
          setPhase('failed')
          setStageNote(null)
        },
      },
    )
  }

  const cancel = () => {
    stopRef.current?.()
    stopRef.current = null
    setPhase((curr) => (curr === 'running' ? (report ? 'done' : 'idle') : curr))
    setStageNote(null)
  }

  const activePosterior = tokenizer?.posterior ?? tokenizerLive?.posterior ?? null

  return (
    <Drawer
      open
      onClose={onClose}
      size="xl"
      icon={<ProviderMark provider={channel.provider} />}
      title={t('admin:channelVerifyTitle', { name: shownName })}
      description={t('admin:channelVerifyDescription')}
      footer={
        <div className="flex w-full flex-wrap items-center justify-between gap-2">
          <span className="text-[11px] text-muted-foreground">
            {t('admin:channelVerifyAttribution')}
          </span>
          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={onClose}>
              {t('common:close')}
            </Button>
            {phase === 'running' ? (
              <Button variant="outline" onClick={cancel}>
                <Square className="h-4 w-4" />
                {t('admin:channelVerifyStop')}
              </Button>
            ) : (
              <Button onClick={start} disabled={model === '' || !anyCheckSelected}>
                <ShieldCheck className="h-4 w-4" />
                {t('admin:channelVerifyStart')}
              </Button>
            )}
          </div>
        </div>
      }
    >
      <div className="form-sections">
        {/* 顶部渠道身份与参考库状态条 */}
        <div className="flex flex-wrap items-center justify-between gap-2.5 rounded-xl border border-border/80 bg-muted/25 px-3.5 py-2.5 text-sm">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <ProviderChip provider={channel.provider} compact />
            <span className="font-mono text-xs text-muted-foreground tabular-nums">#{channel.id}</span>
            <span className="font-semibold">{shownName}</span>
            {upstreamModel !== null && upstreamModel !== model && (
              <Badge variant="muted" className="font-mono text-[11px]">
                → {upstreamModel}
              </Badge>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            {bank.data !== undefined && (
              <span data-slot="verify-bank-meta" className="inline-flex items-center gap-1.5 tabular-nums">
                <Badge variant={bank.data.loaded || bank.data.cache_exists ? 'success' : 'muted'} dot>
                  {bank.data.built_at
                    ? t('admin:channelVerifyBankBuilt', { date: bank.data.built_at.slice(0, 10) })
                    : bank.data.cache_exists
                      ? t('admin:channelVerifyBankCached')
                      : t('admin:channelVerifyBankOnDemand')}
                </Badge>
                <span className="hidden sm:inline">
                  {t('admin:channelVerifyTokenizerBankMeta', {
                    classes: bank.data.tokenizer_classes,
                    probes: bank.data.tokenizer_probes,
                  })}
                </span>
              </span>
            )}
            <Button
              size="sm"
              variant="outline"
              disabled={refreshBank.isPending || phase === 'running'}
              onClick={() => refreshBank.mutate()}
              className="h-7 px-2 text-xs"
            >
              <RefreshCw className={cn('h-3 w-3', refreshBank.isPending && 'animate-spin')} />
              {t('admin:channelVerifyBankRefresh')}
            </Button>
          </div>
        </div>

        {/* 验真参数与项目勾选 */}
        <div className="flex flex-col gap-3.5 rounded-[0.875rem] border border-border/80 bg-muted/25 p-4">
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="verify-model">{t('admin:channelTryModel')}</Label>
              <Select
                id="verify-model"
                value={model}
                onChange={setModel}
                disabled={phase === 'running'}
                options={channel.models.map((m) => ({ value: m, label: m }))}
              />
            </div>
            {channel.keys.length > 1 && (
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="verify-key">{t('admin:channelTryAccount')}</Label>
                <Select
                  id="verify-key"
                  value={keyId}
                  onChange={setKeyId}
                  disabled={phase === 'running'}
                  options={[
                    { value: '', label: t('admin:channelTryAnyAccount') },
                    ...channel.keys.map((k) => ({
                      value: String(k.id),
                      label: k.account_label ? `#${k.id} · ${maskAccount(k.account_label)}` : `#${k.id}`,
                    })),
                  ]}
                />
              </div>
            )}
          </div>

          <div className="grid gap-2.5 sm:grid-cols-3">
            <label
              className={cn(
                'flex flex-col gap-1 rounded-xl border p-3 transition-colors',
                checks.protocol
                  ? 'border-primary/40 bg-primary/[0.04]'
                  : 'border-border/75 bg-card/70',
              )}
            >
              <Checkbox
                checked={checks.protocol}
                disabled={phase === 'running'}
                onChange={(v) => setChecks((c) => ({ ...c, protocol: v }))}
                label={t('admin:channelVerifyCheckProtocol')}
                description={t('admin:channelVerifyCheckProtocolDesc')}
              />
            </label>
            <label
              className={cn(
                'flex flex-col gap-1 rounded-xl border p-3 transition-colors',
                checks.tokenizer
                  ? 'border-primary/40 bg-primary/[0.04]'
                  : 'border-border/75 bg-card/70',
              )}
            >
              <Checkbox
                checked={checks.tokenizer}
                disabled={phase === 'running'}
                onChange={(v) => setChecks((c) => ({ ...c, tokenizer: v }))}
                label={t('admin:channelVerifyCheckTokenizer')}
                description={t('admin:channelVerifyCheckTokenizerDesc')}
              />
            </label>
            <label
              className={cn(
                'flex flex-col gap-1 rounded-xl border p-3 transition-colors',
                checks.fingerprint
                  ? 'border-primary/40 bg-primary/[0.04]'
                  : 'border-border/75 bg-card/70',
              )}
            >
              <Checkbox
                checked={checks.fingerprint}
                disabled={phase === 'running'}
                onChange={(v) => setChecks((c) => ({ ...c, fingerprint: v }))}
                label={t('admin:channelVerifyCheckFingerprint')}
                description={t('admin:channelVerifyCheckFingerprintDesc')}
              />
            </label>
          </div>
          <p className="text-xs text-muted-foreground">{t('admin:channelVerifyBilling')}</p>
        </div>

        {/* 运行状态条 / 错误提示 / 最终综合结论横幅 */}
        {phase === 'running' && (
          <div
            role="status"
            aria-live="polite"
            className="flex items-center gap-2.5 rounded-xl border border-info/35 bg-info/[0.07] px-4 py-3 text-sm"
          >
            <Loader2 className="h-4 w-4 shrink-0 animate-spin text-info" />
            <span className="font-medium text-foreground">
              {stageNote ?? t('admin:channelVerifyRunning')}
            </span>
          </div>
        )}

        {streamError !== null && (
          <div
            role="alert"
            className="flex items-start gap-2.5 rounded-xl border border-destructive/40 bg-destructive/[0.07] px-4 py-3 text-sm text-destructive"
          >
            <ShieldX className="mt-0.5 h-4 w-4 shrink-0" />
            <span>{streamError}</span>
          </div>
        )}

        {report !== null && <VerdictBanner report={report} />}

        {mimicActive && report === null && (
          <div className="flex items-start gap-2 rounded-xl border border-info/30 bg-info/[0.06] px-3.5 py-2.5 text-xs text-muted-foreground">
            <Info className="mt-0.5 h-3.5 w-3.5 shrink-0 text-info" />
            <span>{t('admin:channelVerifyMimicNotice')}</span>
          </div>
        )}

        {stageErrors.length > 0 && report === null && (
          <div className="flex flex-col gap-1.5 rounded-xl border border-warning/40 bg-warning/[0.07] px-3.5 py-2.5 text-xs">
            {stageErrors.map((e, i) => (
              <div key={`${e.stage}-${i}`} className="flex items-start gap-2 text-warning">
                <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>
                  <strong>{t(`admin:channelVerifyStage_${e.stage}`, { defaultValue: e.stage })}:</strong>{' '}
                  {e.message}
                </span>
              </div>
            ))}
          </div>
        )}

        {/* 空闲占位 */}
        {phase === 'idle' && protocol === null && activePosterior === null && fingerprint === null && (
          <div className="flex flex-col items-center justify-center gap-1.5 rounded-xl border border-dashed border-border/80 bg-muted/15 px-6 py-10 text-center">
            <ShieldCheck className="h-7 w-7 text-muted-foreground/60" />
            <p className="text-sm font-medium text-foreground">{t('admin:channelVerifyIdleTitle')}</p>
            <p className="max-w-md text-xs text-muted-foreground">{t('admin:channelVerifyIdleDesc')}</p>
          </div>
        )}

        {/* 卡片 1：L1 协议与能力探针 */}
        {protocol !== null && <ProtocolCard protocol={protocol} />}

        {/* 卡片 2：贝叶斯开集 Tokenizer 指纹 */}
        {activePosterior !== null && (
          <TokenizerCard
            posterior={activePosterior}
            report={tokenizer}
            liveProgress={tokenizerLive ? { run: tokenizerLive.probesRun, max: tokenizerLive.maxProbes } : null}
            isZh={isZh}
          />
        )}

        {/* 卡片 3：ModelTrace L2 行为指纹归因 */}
        {(samples.length > 0 || fingerprint !== null) && (
          <FingerprintCard samples={samples} report={fingerprint} />
        )}
      </div>
    </Drawer>
  )
}

function VerdictBanner({ report }: { report: VerifyReport }) {
  const { t } = useTranslation()
  const style = verdictStyle(report.verdict)
  const Icon = style.Icon
  return (
    <section
      data-slot="verify-verdict"
      data-verdict={report.verdict}
      className={cn('flex flex-col gap-2.5 rounded-xl border p-4', style.box)}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2.5">
          <Icon className="h-5 w-5 shrink-0" />
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-sm font-semibold">
              {t(`admin:channelVerifyVerdict_${report.verdict}`)}
            </h3>
            <Badge variant={style.badge} dot>
              {t(`admin:channelVerifyVerdictTag_${report.verdict}`)}
            </Badge>
          </div>
        </div>
        <span className="font-mono text-xs text-muted-foreground tabular-nums">
          {t('admin:channelTryTotal', { ms: report.elapsed_ms })}
        </span>
      </div>
      {report.warnings.length > 0 && (
        <ul className="flex flex-col gap-1.5 border-t border-border/50 pt-2.5 text-xs">
          {report.warnings.map((w, idx) => (
            <li key={`${w.code}-${idx}`} className="flex items-start gap-2">
              {w.severity === 'error' ? (
                <ShieldX className="mt-0.5 h-3.5 w-3.5 shrink-0 text-destructive" />
              ) : w.severity === 'warn' ? (
                <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warning" />
              ) : (
                <Info className="mt-0.5 h-3.5 w-3.5 shrink-0 text-info" />
              )}
              <span className="leading-5 text-foreground/90">
                {t(`admin:channelVerifyWarn_${w.code}`, { defaultValue: w.message })}
                {w.code !== 'mimic_profile_active' && w.code !== 'declared_model_not_in_bank' && (
                  <span className="ml-1.5 text-muted-foreground">({w.message})</span>
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function ProtocolCard({ protocol }: { protocol: ProtocolCheckResult }) {
  const { t } = useTranslation()
  return (
    <section
      data-slot="verify-protocol-card"
      className="flex flex-col gap-3 rounded-[0.875rem] border border-border/80 bg-card p-4 shadow-xs"
    >
      <div className="flex items-center gap-2">
        <Cpu className="h-4 w-4 text-primary" />
        <h3 className="text-[13.5px] font-semibold">{t('admin:channelVerifyCheckProtocol')}</h3>
      </div>

      <div className="grid gap-2.5 sm:grid-cols-3">
        {/* 返回模型名 */}
        <div className="flex flex-col gap-1.5 rounded-xl border border-border/70 bg-muted/20 p-3">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-muted-foreground">
              {t('admin:channelVerifyModelName')}
            </span>
            <Badge variant={statusVariant(protocol.model_name.status)} dot>
              {t(`admin:channelVerifyStatus_${protocol.model_name.status}`)}
            </Badge>
          </div>
          <div className="font-mono text-xs font-medium break-all text-foreground">
            {protocol.model_name.response_model ?? '—'}
          </div>
          <div className="text-[11px] text-muted-foreground">
            {protocol.model_name.declared_family && protocol.model_name.response_family
              ? `${protocol.model_name.declared_family} ↔ ${protocol.model_name.response_family}`
              : protocol.model_name.detail ?? protocol.model_name.upstream_model}
          </div>
        </div>

        {/* Tool Calling Nonce */}
        <div className="flex flex-col gap-1.5 rounded-xl border border-border/70 bg-muted/20 p-3">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-muted-foreground">
              {t('admin:channelVerifyToolCall')}
            </span>
            <Badge variant={statusVariant(protocol.tool_call.status)} dot>
              {t(`admin:channelVerifyStatus_${protocol.tool_call.status}`)}
            </Badge>
          </div>
          <div className="font-mono text-xs text-foreground tabular-nums">
            {protocol.tool_call.nonce}
          </div>
          <div className="text-[11px] text-muted-foreground tabular-nums">
            {protocol.tool_call.detail
              ? protocol.tool_call.detail
              : t('admin:channelVerifyToolCallOk', { ms: protocol.tool_call.latency_ms })}
          </div>
        </div>

        {/* Anthropic Thinking Signature */}
        <div className="flex flex-col gap-1.5 rounded-xl border border-border/70 bg-muted/20 p-3">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-muted-foreground">
              {t('admin:channelVerifySignature')}
            </span>
            <Badge variant={statusVariant(protocol.signature.status)} dot>
              {t(`admin:channelVerifyStatus_${protocol.signature.status}`)}
            </Badge>
          </div>
          <div className="font-mono text-xs text-foreground tabular-nums">
            {protocol.signature.has_signature
              ? t('admin:channelVerifySignatureBytes', {
                  bytes: protocol.signature.signature_len ?? 0,
                  mode: protocol.signature.thinking_mode ?? 'thinking',
                })
              : protocol.signature.applicable
                ? t('admin:channelVerifySignatureAbsent')
                : t('admin:channelVerifyNotApplicable')}
          </div>
          <div className="text-[11px] text-muted-foreground tabular-nums">
            {protocol.signature.detail
              ? protocol.signature.detail
              : protocol.signature.latency_ms > 0
                ? `${protocol.signature.latency_ms} ms`
                : '—'}
          </div>
        </div>
      </div>
    </section>
  )
}

function TokenizerCard({
  posterior,
  report,
  liveProgress,
  isZh,
}: {
  posterior: TokenizerPosterior
  report: TokenizerReport | null
  liveProgress: { run: number; max: number } | null
  isZh: boolean
}) {
  const { t } = useTranslation()
  const topClasses = posterior.classes.slice(0, 4)

  return (
    <section
      data-slot="verify-tokenizer-card"
      className="flex flex-col gap-3 rounded-[0.875rem] border border-border/80 bg-card p-4 shadow-xs"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <Binary className="h-4 w-4 text-primary" />
          <h3 className="text-[13.5px] font-semibold">{t('admin:channelVerifyCheckTokenizer')}</h3>
          {liveProgress !== null && (
            <Badge variant="info" dot>
              {liveProgress.run}/{liveProgress.max}
            </Badge>
          )}
          {report?.verdict.claim && (
            <Badge variant={claimVariant(report.verdict.claim.status)} dot>
              {t(`admin:channelVerifyClaim_${report.verdict.claim.status}`)}
            </Badge>
          )}
        </div>
        {report !== null && (
          <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground tabular-nums">
            <Badge variant="muted">
              {t('admin:channelVerifyTokenizerProbes', { n: report.probes_run })}
            </Badge>
            <Badge variant="muted">
              {t('admin:channelVerifyBaseline', { tokens: report.baseline_tokens })}
              {report.baseline_drift !== null && report.baseline_drift !== undefined && report.baseline_drift > 0
                ? ` (±${report.baseline_drift})`
                : ''}
            </Badge>
          </div>
        )}
      </div>

      {report !== null && (
        <div className="flex flex-wrap items-center gap-2 rounded-lg bg-muted/35 px-3 py-2 text-xs">
          <span className="font-semibold text-foreground">
            {report.verdict.kind === 'unknown'
              ? t('admin:channelVerifyTokenizerProprietary')
              : `${report.verdict.top.lab_name} · ${isZh ? report.verdict.top.series_zh : report.verdict.top.series}`}
          </span>
          <Badge variant="outline" className="font-mono">
            {t(`admin:channelVerifyTokenizerKind_${report.verdict.kind}`)} · {pct(report.verdict.confidence)}
          </Badge>
          {report.verdict.claim && (
            <span className="text-muted-foreground">
              {t('admin:channelVerifyClaimVendor', {
                vendor: report.verdict.claim.vendor,
                prob: pct(report.verdict.claim.probability),
              })}
            </span>
          )}
        </div>
      )}

      <div className="flex flex-col gap-2">
        {topClasses.map((cls) => {
          const share = Math.min(1, Math.max(0, cls.exact + cls.related))
          return (
            <div key={cls.id} className="flex flex-col gap-1">
              <div className="flex items-center justify-between gap-2 text-xs">
                <span className="truncate font-medium text-foreground">
                  {cls.lab_name} · {isZh ? cls.series_zh : cls.series}
                </span>
                <span className="shrink-0 font-mono text-muted-foreground tabular-nums">
                  {pct(share)} ({cls.matched}/{cls.compared})
                </span>
              </div>
              <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full rounded-full bg-primary transition-[width] duration-300"
                  style={{ width: `${Math.max(share * 100, share > 0.005 ? 2 : 0)}%` }}
                />
              </div>
            </div>
          )
        })}

        <div className="flex flex-col gap-1 pt-0.5">
          <div className="flex items-center justify-between gap-2 text-xs">
            <span className="font-medium text-muted-foreground">
              {t('admin:channelVerifyTokenizerUnknownBar')}
            </span>
            <span className="font-mono text-muted-foreground tabular-nums">{pct(posterior.unknown)}</span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
            <div
              className="h-full rounded-full bg-info/80 transition-[width] duration-300"
              style={{ width: `${Math.max(posterior.unknown * 100, posterior.unknown > 0.005 ? 2 : 0)}%` }}
            />
          </div>
        </div>
      </div>
    </section>
  )
}

function FingerprintCard({
  samples,
  report,
}: {
  samples: ChallengeSampleSummary[]
  report: FingerprintReport | null
}) {
  const { t } = useTranslation()

  const decisionBadgeVariant = (d: FingerprintReport['decision']) => {
    switch (d) {
      case 'scored':
        return report?.family_matches === false ? 'destructive' : 'success'
      case 'partial':
        return 'warning'
      case 'unscorable':
        return 'warning'
      case 'not_model':
        return 'destructive'
    }
  }

  return (
    <section
      data-slot="verify-fingerprint-card"
      className="flex flex-col gap-3.5 rounded-[0.875rem] border border-border/80 bg-card p-4 shadow-xs"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <Fingerprint className="h-4 w-4 text-primary" />
          <h3 className="text-[13.5px] font-semibold">{t('admin:channelVerifyCheckFingerprint')}</h3>
          {report !== null && (
            <Badge variant={decisionBadgeVariant(report.decision)} dot>
              {t(`admin:channelVerifyDecision_${report.decision}`)}
            </Badge>
          )}
        </div>
        {report !== null && (
          <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground tabular-nums">
            <Badge variant="muted">
              {t('admin:channelVerifyBankModels', { n: report.bank_model_count })}
            </Badge>
            <Badge variant="muted">{report.bank_built_at.slice(0, 10)}</Badge>
          </div>
        )}
      </div>

      {/* 3 轮挑战采样状态条 */}
      <div className="grid gap-2 sm:grid-cols-3">
        {[0, 1, 2].map((idx) => {
          const sample = samples.find((s) => s.index === idx)
          return (
            <div
              key={idx}
              className="flex items-center justify-between gap-2 rounded-lg border border-border/70 bg-muted/20 px-3 py-2 text-xs tabular-nums"
            >
              <span className="font-medium text-muted-foreground">
                {t('admin:channelVerifyChallenge', { n: idx + 1 })}
              </span>
              {sample === undefined ? (
                <span className="inline-flex items-center gap-1 text-muted-foreground">
                  <Loader2 className="h-3 w-3 animate-spin" />
                  {t('admin:channelVerifySampling')}
                </span>
              ) : sample.accepted ? (
                <span className="inline-flex items-center gap-1 font-mono text-success">
                  <CheckCircle2 className="h-3.5 w-3.5" />
                  {sample.parsed_numbers}/{sample.expected_count} · {(sample.latency_ms / 1000).toFixed(1)}s
                </span>
              ) : (
                <span className="truncate font-mono text-destructive" title={sample.error ?? undefined}>
                  {sample.error ? t('admin:channelTryFailed') : `${sample.parsed_numbers}/${sample.expected_count}`}
                </span>
              )}
            </div>
          )
        })}
      </div>

      {report !== null && (
        <>
          {/* 顶部归因摘要 */}
          <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-muted/35 px-3 py-2.5 text-xs">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-muted-foreground">{t('admin:channelVerifyPredictedModel')}:</span>
              <span className="font-semibold text-foreground">{report.prediction_name}</span>
              <Badge variant={report.family_matches === false ? 'destructive' : 'info'}>
                {report.family_prediction_name}
                {report.top_family_probability !== null ? ` · ${pct(report.top_family_probability)}` : ''}
              </Badge>
              {report.model_like === false && (
                <Badge variant="destructive">{t('admin:channelVerifyNotModelBadge')}</Badge>
              )}
            </div>
            <span className="text-muted-foreground">{report.evidence_label}</span>
          </div>

          {/* 候选模型 Top-5 + 家族概率 */}
          <div className="grid gap-4 md:grid-cols-2">
            <div className="flex flex-col gap-2">
              <div className="text-xs font-semibold text-muted-foreground">
                {t('admin:channelVerifyTopModels')}
              </div>
              {report.top_matches.map((cand, idx) => {
                const bar = cand.probability ?? Math.max(0, Math.min(1, cand.score))
                return (
                  <div key={cand.model} className="flex flex-col gap-1">
                    <div className="flex items-center justify-between gap-2 text-xs">
                      <span className="truncate font-medium text-foreground">
                        {idx + 1}. {cand.display_name}
                      </span>
                      <span className="shrink-0 font-mono text-muted-foreground tabular-nums">
                        {cand.probability !== null ? pct(cand.probability) : cand.score.toFixed(3)}
                      </span>
                    </div>
                    <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                      <div
                        className={cn(
                          'h-full rounded-full transition-[width] duration-300',
                          idx === 0 ? 'bg-primary' : 'bg-primary/50',
                        )}
                        style={{ width: `${Math.max(bar * 100, bar > 0.005 ? 2 : 0)}%` }}
                      />
                    </div>
                  </div>
                )
              })}
            </div>

            <div className="flex flex-col gap-2">
              <div className="text-xs font-semibold text-muted-foreground">
                {t('admin:channelVerifyTopFamilies')}
              </div>
              {report.family_probabilities.length === 0 ? (
                <p className="text-xs text-muted-foreground">{report.evidence_reason}</p>
              ) : (
                report.family_probabilities.map((fam) => {
                  const isDeclared = report.declared_family === fam.family
                  return (
                    <div key={fam.family} className="flex flex-col gap-1">
                      <div className="flex items-center justify-between gap-2 text-xs">
                        <span className="inline-flex items-center gap-1.5 font-medium text-foreground">
                          {fam.family_name}
                          {isDeclared && (
                            <Badge variant="outline" className="min-h-4 px-1.5 py-0 text-[10px]">
                              {t('admin:channelVerifyDeclaredTag')}
                            </Badge>
                          )}
                        </span>
                        <span className="font-mono text-muted-foreground tabular-nums">
                          {pct(fam.probability)}
                        </span>
                      </div>
                      <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                        <div
                          className={cn(
                            'h-full rounded-full transition-[width] duration-300',
                            isDeclared ? 'bg-success' : 'bg-info/75',
                          )}
                          style={{
                            width: `${Math.max(fam.probability * 100, fam.probability > 0.005 ? 2 : 0)}%`,
                          }}
                        />
                      </div>
                    </div>
                  )
                })
              )}
            </div>
          </div>
        </>
      )}
    </section>
  )
}
