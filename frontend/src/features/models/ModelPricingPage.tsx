import { Pencil, Plus, Trash2, Upload } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ModelListRow } from '@/features/models/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { ImportDrawer } from '@/features/models/ImportDrawer'
import { Input, Label } from '@/components/ui/input'
import { ModelDrawer } from '@/features/models/ModelDrawer'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

/// 模型定价页。
///
/// 只管一件事：模型的倍率配置。价格分组与计费活动各有独立页面——
/// 它们与模型定价是不同的决策，此前挤在同一屏让人分不清改的是哪一层。
export function ModelPricingPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
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

  const remove = useMutation({
    mutationFn: (name: string) =>
      apiFetch<{ requires_publish: boolean }>(`/admin/models/${encodeURIComponent(name)}`, {
        method: 'DELETE',
      }),
    onSuccess: (r) => {
      setMsg(r.requires_publish ? t('admin:requiresPublish') : t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const publish = useMutation({
    mutationFn: () =>
      apiFetch<{ epoch: number }>('/admin/pricing/publish', { method: 'POST', body: {} }),
    onSuccess: (data) => setMsg(t('admin:publishedEpoch', { epoch: data.epoch })),
    onError: (err) => setMsg(describeError(err)),
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
        action={
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => setImporting(true)}>
              <Upload className="h-4 w-4" />
              {t('admin:importTitle')}
            </Button>
            <Button onClick={() => setDrawer({})}>
              <Plus className="h-4 w-4" />
              {t('admin:upsertModel')}
            </Button>
          </div>
        }
      />

      <Toolbar
        filters={
          <>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="m-search">{t('common:search')}</Label>
              <Input
                id="m-search"
                className="w-56"
                value={search}
                placeholder={t('admin:modelSearchHint')}
                onChange={(e) => setSearch(e.target.value)}
              />
            </div>
            {unpricedCount > 0 && (
              <Button
                size="sm"
                variant={onlyUnpriced ? 'default' : 'outline'}
                onClick={() => setOnlyUnpriced((v) => !v)}
              >
                {t('admin:onlyUnpriced', { n: unpricedCount })}
              </Button>
            )}
          </>
        }
        selection={
          <>
            <span className="text-xs text-muted-foreground">{t('admin:publishHint')}</span>
            <Button size="sm" disabled={publish.isPending} onClick={() => publish.mutate()}>
              {t('admin:publish')}
            </Button>
          </>
        }
      />

      {msg !== null && <p className="text-xs text-muted-foreground">{msg}</p>}

      {models.isError ? (
        <ErrorState message={describeError(models.error)} />
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
              <Th>{t('admin:cacheRatio')}</Th>
              <Th>{t('admin:cacheWriteRatioShort')}</Th>
              <Th>{t('admin:audioRatioShort')}</Th>
              <Th>{t('admin:imageRatio')}</Th>
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
                <Td>{m.model_ratio ?? '—'}</Td>
                <Td>{m.completion_ratio ?? '—'}</Td>
                <Td>{m.cache_ratio ?? '—'}</Td>
                <Td>{m.cache_write_ratio ?? '—'}</Td>
                <Td>
                  {m.audio_ratio === null || m.audio_ratio === '1.000000'
                    ? '—'
                    : `${m.audio_ratio} ×${m.audio_completion_ratio ?? '1'}`}
                </Td>
                <Td>{m.image_ratio ?? '—'}</Td>
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
            setMsg(t('admin:requiresPublish'))
            invalidate()
          }}
        />
      )}
      {importing && <ImportDrawer onClose={() => setImporting(false)} onDone={invalidate} />}
    </div>
  )
}
