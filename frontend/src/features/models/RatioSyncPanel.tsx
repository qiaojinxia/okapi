import { useMutation } from '@tanstack/react-query'
import { Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { EmptyState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatRatio } from '@/lib/money'

/// 后端 `/admin/pricing/sync/fetch` 的差异表：模型 → 轴 → { 本地值, 各源值 }。
/// 源值为 `"same"` 表示与本地一致。
interface AxisDiff {
  current: string | null
  upstreams: Record<string, string>
}
type Differences = Record<string, Record<string, AxisDiff>>

interface SourceStatus {
  name: string
  status: 'ok' | 'error'
  error?: string
  models: number
}

interface FetchResult {
  differences: Differences
  sources: SourceStatus[]
}

interface SourceDraft {
  id: number
  name: string
  url: string
}

/// 一条选中的改动：模型 × 轴 → 采用哪个源的值。
type Selection = Record<string, { source: string; value: string }>

const AXIS_LABEL: Record<string, string> = {
  model_ratio: 'admin:syncAxisModel',
  completion_ratio: 'admin:syncAxisCompletion',
  cache_ratio: 'admin:syncAxisCache',
  cache_write_ratio: 'admin:syncAxisCacheWrite',
  audio_ratio: 'admin:syncAxisAudio',
  audio_completion_ratio: 'admin:syncAxisAudioCompletion',
  image_ratio: 'admin:syncAxisImage',
  per_call_price: 'admin:syncAxisPerCall',
}

const selectionKey = (model: string, axis: string) => `${model}\u0000${axis}`

/// 上游倍率在线同步（IMPLEMENTATION §11.36）：源列表 → 拉取差异 → 点选源值 → 应用。
///
/// 默认全不选：拉取只是"看见差异"，改价是另一个显式动作。
/// 同一模型多个轴各自独立选择——完全可能只想跟上游的输入价、保留自己的补全价。
export function RatioSyncPanel({ onApplied }: { onApplied: () => void }) {
  const { t } = useTranslation()
  const [sources, setSources] = useState<SourceDraft[]>([{ id: 0, name: '', url: '' }])
  const [nextId, setNextId] = useState(1)
  const [result, setResult] = useState<FetchResult | null>(null)
  const [selected, setSelected] = useState<Selection>({})

  const validSources = sources
    .map((s) => ({ name: s.name.trim(), url: s.url.trim() }))
    .filter((s) => s.name !== '' && s.url !== '')

  const fetchDiff = useMutation({
    mutationFn: () =>
      apiFetch<FetchResult>('/admin/pricing/sync/fetch', {
        method: 'POST',
        body: { sources: validSources },
      }),
    onSuccess: (r) => {
      setResult(r)
      setSelected({})
      const failed = r.sources.filter((s) => s.status === 'error')
      if (failed.length > 0) {
        toast.warning(t('admin:syncSourcesFailed', { n: failed.length, names: failed.map((s) => s.name).join(', ') }))
      }
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const apply = useMutation({
    mutationFn: () =>
      apiFetch<{ applied: number }>('/admin/pricing/sync/apply', {
        method: 'POST',
        body: {
          changes: Object.entries(selected).map(([key, pick]) => {
            const [model, axis] = key.split('\u0000')
            return { model, axis, value: pick.value }
          }),
        },
      }),
    onSuccess: (r) => {
      toast.success(t('admin:syncApplied', { n: r.applied }))
      setSelected({})
      setResult(null)
      onApplied()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const rows = result
    ? Object.entries(result.differences).flatMap(([model, axes]) =>
        Object.entries(axes).map(([axis, diff]) => ({ model, axis, diff })),
      )
    : []
  const sourceNames = result?.sources.filter((s) => s.status === 'ok').map((s) => s.name) ?? []
  const selectedCount = Object.keys(selected).length

  const toggle = (model: string, axis: string, source: string, value: string) => {
    const key = selectionKey(model, axis)
    setSelected((prev) => {
      const next = { ...prev }
      if (prev[key]?.source === source) delete next[key]
      else next[key] = { source, value }
      return next
    })
  }

  return (
    <div className="flex flex-col gap-4">
      <p className="text-xs leading-5 text-muted-foreground">{t('admin:syncDesc')}</p>

      <div className="flex flex-col gap-2">
        {sources.map((s, i) => (
          <div key={s.id} className="grid grid-cols-[minmax(0,8rem)_minmax(0,1fr)_auto] items-end gap-2">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor={`sync-name-${s.id}`}>{t('admin:syncSourceName')}</Label>
              <Input
                id={`sync-name-${s.id}`}
                value={s.name}
                placeholder={t('admin:syncSourceNamePlaceholder')}
                onChange={(e) =>
                  setSources((prev) => prev.map((x) => (x.id === s.id ? { ...x, name: e.target.value } : x)))
                }
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor={`sync-url-${s.id}`}>{t('admin:syncSourceUrl')}</Label>
              <Input
                id={`sync-url-${s.id}`}
                className="font-mono text-xs"
                inputMode="url"
                value={s.url}
                placeholder="https://example.com/api/pricing"
                onChange={(e) =>
                  setSources((prev) => prev.map((x) => (x.id === s.id ? { ...x, url: e.target.value } : x)))
                }
              />
            </div>
            <Button
              variant="ghost"
              size="icon"
              disabled={sources.length === 1}
              aria-label={t('admin:syncRemoveSource', { n: i + 1 })}
              onClick={() => setSources((prev) => prev.filter((x) => x.id !== s.id))}
            >
              <Trash2 aria-hidden className="h-4 w-4" />
            </Button>
          </div>
        ))}
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={sources.length >= 8}
            onClick={() => {
              setSources((prev) => [...prev, { id: nextId, name: '', url: '' }])
              setNextId((n) => n + 1)
            }}
          >
            <Plus aria-hidden className="h-4 w-4" />
            {t('admin:syncAddSource')}
          </Button>
          <Button
            size="sm"
            disabled={validSources.length === 0 || fetchDiff.isPending}
            loading={fetchDiff.isPending}
            onClick={() => fetchDiff.mutate()}
          >
            {t('admin:syncFetch')}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">{t('admin:syncSourceHint')}</p>
      </div>

      {result && (
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap gap-1.5">
            {result.sources.map((s) => (
              <Badge
                key={s.name}
                variant={s.status === 'ok' ? 'success' : 'destructive'}
                title={s.error ?? ''}
              >
                {s.status === 'ok'
                  ? t('admin:syncSourceOk', { name: s.name, n: s.models })
                  : t('admin:syncSourceError', { name: s.name, error: s.error ?? '' })}
              </Badge>
            ))}
          </div>

          {rows.length === 0 ? (
            <EmptyState hint={t('admin:syncNoDiff')} />
          ) : (
            <>
              <p className="text-xs text-muted-foreground">{t('admin:syncPickHint')}</p>
              <Table stickyHeader wrapperClassName="max-h-96">
                <THead>
                  <Tr>
                    <Th>{t('pricing:model')}</Th>
                    <Th>{t('admin:syncAxis')}</Th>
                    <Th numeric>{t('admin:syncLocal')}</Th>
                    {sourceNames.map((name) => (
                      <Th key={name} numeric>
                        {name}
                      </Th>
                    ))}
                  </Tr>
                </THead>
                <TBody>
                  {rows.map(({ model, axis, diff }) => {
                    const key = selectionKey(model, axis)
                    return (
                      <Tr key={key}>
                        <Td className="font-mono text-xs">{model}</Td>
                        <Td className="text-xs">{t(AXIS_LABEL[axis] ?? 'admin:syncAxis')}</Td>
                        <Td numeric className="tabular-nums">
                          {diff.current === null ? (
                            <span className="text-muted-foreground">{t('admin:syncMissingLocal')}</span>
                          ) : (
                            formatRatio(diff.current)
                          )}
                        </Td>
                        {sourceNames.map((name) => {
                          const value = diff.upstreams[name]
                          if (value === undefined) {
                            return (
                              <Td key={name} numeric className="text-muted-foreground">
                                —
                              </Td>
                            )
                          }
                          if (value === 'same') {
                            return (
                              <Td key={name} numeric className="text-xs text-muted-foreground">
                                {t('admin:syncSame')}
                              </Td>
                            )
                          }
                          const on = selected[key]?.source === name
                          return (
                            <Td key={name} numeric>
                              {/* 点某个源的值 = 采用它；再点取消。默认全不选 */}
                              <Button
                                size="sm"
                                variant={on ? 'default' : 'outline'}
                                className="tabular-nums"
                                aria-pressed={on}
                                onClick={() => toggle(model, axis, name, value)}
                              >
                                {formatRatio(value)}
                              </Button>
                            </Td>
                          )
                        })}
                      </Tr>
                    )
                  })}
                </TBody>
              </Table>
              <div className="flex items-center justify-between gap-2">
                <span className="text-xs text-muted-foreground">
                  {t('admin:syncSelected', { n: selectedCount })}
                </span>
                <Button
                  disabled={selectedCount === 0 || apply.isPending}
                  loading={apply.isPending}
                  onClick={() => apply.mutate()}
                >
                  {t('admin:syncApply', { n: selectedCount })}
                </Button>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  )
}
