import { AlertTriangle, Coins, Pencil, Plus, Rocket, Trash2, Upload } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ModelListRow } from '@/features/models/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { IconButton } from '@/components/ui/icon-button'
import { ImportDrawer } from '@/features/models/ImportDrawer'
import { ModelDrawer } from '@/features/models/ModelDrawer'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { SearchInput } from '@/components/ui/search-input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Link } from '@tanstack/react-router'
import type { ChannelRow } from '@/features/channels/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

/// 模型定价页。
///
/// 只管一件事：模型的倍率配置。价格分组与计费活动各有独立页面——
/// 它们与模型定价是不同的决策，此前挤在同一屏让人分不清改的是哪一层。
export function ModelPricingPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [onlyUnpriced, setOnlyUnpriced] = useState(false)
  const [drawer, setDrawer] = useState<{ model?: ModelListRow } | null>(null)
  const [importing, setImporting] = useState(false)
  const { confirm, dialog } = useConfirm()

  const models = useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: ModelListRow[] }>('/admin/models'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminModels })
  // 每个模型有几条启用渠道在服务：定了价却没渠道，请求同样会被拒——
  // 与"未定价"是同一类"配了一半"的漏项，此前只有路由诊断器能查出来
  const channels = useQuery({
    queryKey: qk.adminChannels,
    queryFn: () => apiFetch<{ data: ChannelRow[] }>('/admin/channels'),
  })
  const channelCount = new Map<string, number>()
  for (const c of channels.data?.data ?? []) {
    if (c.status !== 1) continue
    for (const m of c.models) channelCount.set(m, (channelCount.get(m) ?? 0) + 1)
  }
  // 倍率 → $/1M（基准 $2/1M，DESIGN §3.2）：站长配倍率、对上游报价单时想的是美元
  const perMillion = (ratio: string | null, factor = 1) =>
    ratio === null ? null : Number(ratio) * factor * 2

  const remove = useMutation({
    mutationFn: (name: string) =>
      apiFetch<{ requires_publish: boolean }>(`/admin/models/${encodeURIComponent(name)}`, {
        method: 'DELETE',
      }),
    onSuccess: (r) => {
      if (r.requires_publish) toast.warning(t('admin:requiresPublish'))
      else toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const publish = useMutation({
    mutationFn: () =>
      apiFetch<{ epoch: number }>('/admin/pricing/publish', { method: 'POST', body: {} }),
    onSuccess: (data) => toast.success(t('admin:publishedEpoch', { epoch: data.epoch })),
    onError: (err) => toast.error(describeError(err)),
  })

  const all = models.data?.data ?? []
  const rows = all.filter((m) => {
    const kw = search.trim().toLowerCase()
    const hitKw = kw === '' || m.model_name.toLowerCase().includes(kw)
    return hitKw && (!onlyUnpriced || m.pricing_mode === null)
  })
  const unpricedCount = all.filter((m) => m.pricing_mode === null).length

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:modelListTitle')}
        description={t('admin:modelsDesc')}
        icon={Coins}
        action={
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => setImporting(true)}>
              <Upload className="h-4 w-4" />
              {t('admin:importTitle')}
            </Button>
            <Button onClick={() => setDrawer({})}>
              <Plus className="h-4 w-4" />
              {t('admin:createModel')}
            </Button>
          </div>
        }
      />

      <Toolbar
        filters={
          <>
            <SearchInput
              id="m-search"
              className="w-64"
              value={search}
              placeholder={t('admin:modelSearchHint')}
              onChange={setSearch}
            />
            {unpricedCount > 0 && (
              <Button
                size="sm"
                variant={onlyUnpriced ? 'destructive' : 'outline'}
                aria-pressed={onlyUnpriced}
                onClick={() => setOnlyUnpriced((v) => !v)}
              >
                <AlertTriangle className="h-3.5 w-3.5" />
                {t('admin:onlyUnpriced', { n: unpricedCount })}
              </Button>
            )}
            <span className="text-xs text-muted-foreground tabular-nums">
              {t('common:resultCount', { n: rows.length })}
            </span>
          </>
        }
        selection={
          <>
            <span className="text-xs text-muted-foreground">{t('admin:publishHint')}</span>
            <Button size="sm" loading={publish.isPending} onClick={() => publish.mutate()}>
              {!publish.isPending && <Rocket className="h-3.5 w-3.5" />}
              {t('admin:publish')}
            </Button>
          </>
        }
      />

      {models.isError ? (
        <ErrorState message={describeError(models.error)} onRetry={() => void models.refetch()} />
      ) : models.isPending ? (
        <TableSkeleton rows={6} cols={12} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:modelsEmptyHint')} />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>{t('admin:modelName')}</Th>
              <Th>{t('admin:vendor')}</Th>
              <Th>{t('admin:pricingMode')}</Th>
              <Th>{t('admin:modelRatio')}</Th>
              <Th>{t('admin:completionRatio')}</Th>
              <Th>{t('admin:perMillionCol')}</Th>
              <Th>{t('admin:cacheRatio')}</Th>
              <Th>{t('admin:cacheWriteRatioShort')}</Th>
              <Th>{t('admin:audioRatioShort')}</Th>
              <Th>{t('admin:imageRatio')}</Th>
              <Th>{t('admin:modelChannelsCol')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((m) => (
              <Tr key={m.model_name}>
                <Td className="font-mono text-xs">{m.model_name}</Td>
                <Td className="text-xs text-muted-foreground">{m.vendor ?? '—'}</Td>
                <Td>
                  {m.pricing_mode === null ? (
                    <Badge variant="destructive">{t('admin:unpriced')}</Badge>
                  ) : (
                    <Badge variant="muted">{m.pricing_mode}</Badge>
                  )}
                </Td>
                <Td>{formatRatio(m.model_ratio)}</Td>
                <Td>{formatRatio(m.completion_ratio)}</Td>
                <Td className="whitespace-nowrap text-xs text-muted-foreground">
                  {m.model_ratio === null
                    ? '—'
                    : `$${perMillion(m.model_ratio)?.toFixed(2)} / $${perMillion(
                        m.model_ratio,
                        Number(m.completion_ratio ?? '1'),
                      )?.toFixed(2)}`}
                </Td>
                <Td>{formatRatio(m.cache_ratio)}</Td>
                <Td>{formatRatio(m.cache_write_ratio)}</Td>
                <Td>
                  {m.audio_ratio === null || Number(m.audio_ratio) === 1
                    ? '—'
                    : `${formatRatio(m.audio_ratio)} ×${formatRatio(m.audio_completion_ratio ?? '1')}`}
                </Td>
                <Td>{formatRatio(m.image_ratio)}</Td>
                <Td>
                  {(channelCount.get(m.model_name) ?? 0) === 0 ? (
                    <Badge variant={m.pricing_mode === null ? 'muted' : 'destructive'}>
                      {t('admin:modelNoChannel')}
                    </Badge>
                  ) : (
                    <Link
                      to="/admin/channels"
                      className="text-xs underline decoration-dotted hover:text-foreground"
                    >
                      {t('admin:modelChannelCount', { n: channelCount.get(m.model_name) })}
                    </Link>
                  )}
                </Td>
                <Td>
                  <div className="flex items-center gap-0.5">
                    <IconButton
                      icon={Pencil}
                      label={t('common:edit')}
                      onClick={() => setDrawer({ model: m })}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: m.model_name }),
                          description: t('common:confirmModelDelete'),
                          requireText: m.model_name,
                          onConfirm: () => remove.mutate(m.model_name),
                        })
                      }
                    />
                  </div>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {dialog}
      {drawer !== null && (
        <ModelDrawer
          model={drawer.model}
          onClose={() => setDrawer(null)}
          onDone={() => {
            toast.warning(t('admin:requiresPublish'))
            invalidate()
          }}
        />
      )}
      {importing && <ImportDrawer onClose={() => setImporting(false)} onDone={invalidate} />}
    </div>
  )
}
