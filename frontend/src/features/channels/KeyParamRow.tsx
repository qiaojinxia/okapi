import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelKeyRow } from '@/features/channels/types'
import { parseLimit } from './account-controls/policy'
import { OAuthKeyHealth } from './oauth-key-health'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { OptionalSection } from '@/components/ui/optional-section'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 单把 key 的权重、并发上限与状态。
///
/// 并发上限走三态语义：留空提交 null = 解除上限，与"不改"区分。
///
/// 静态 key 可重新启用；订阅 OAuth key 失效后必须更新授权，恢复 worker 不自动捞它。
export function KeyParamRow({
  channelId,
  provider,
  row,
  refreshEnabled,
  showIdentity = false,
  showCredential = true,
  onDone,
}: {
  channelId: number
  provider: string
  row: ChannelKeyRow
  refreshEnabled: boolean
  showIdentity?: boolean
  /** OAuth credential status; the channel drawer shows it with the credential instead. */
  showCredential?: boolean
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [weight, setWeight] = useState(String(row.weight))
  const [conc, setConc] = useState(row.max_concurrency === null ? '' : String(row.max_concurrency))

  const limit = parseLimit(conc)
  const parsedWeight = parseLimit(weight)
  const valid = limit !== null && (limit === undefined || limit <= 2_147_483_647)
    && parsedWeight != null && parsedWeight <= 2_147_483_647

  const enable = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channelId}/keys/${row.id}`, {
        method: 'PATCH',
        body: { status: 1 },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channelId}/keys/${row.id}`, {
        method: 'PATCH',
        body: {
          weight: parsedWeight,
          max_concurrency: limit ?? null,
        },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <div className={showIdentity ? 'flex flex-wrap items-end gap-2 rounded-md border border-border p-2' : 'flex flex-wrap items-end gap-2'}>
      {showIdentity && <div className="flex h-9 items-center">
        <Badge variant={row.status === 1 ? 'success' : row.status === 6 ? 'destructive' : 'muted'}>
          #{row.id}
        </Badge>
      </div>}
      <div className="min-w-0 basis-full">
        <OptionalSection id={`channel-key-options-${row.id}`} title={t('admin:channelKeyOptions')}
          summary={t('admin:channelKeySummary', { concurrency: conc || t('admin:channelLimitUnlimited'), weight })}>
          <div className="flex flex-wrap items-end gap-2">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor={`kw-${row.id}`}>{t('admin:keyWeight')}</Label>
              <Input
                aria-invalid={parsedWeight == null}
                id={`kw-${row.id}`}
                className="w-20"
                value={weight}
                inputMode="numeric"
                onChange={(e) => setWeight(e.target.value)}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor={`kc-${row.id}`}>{t('admin:keyConcurrency')}</Label>
              <Input
                aria-invalid={limit === null}
                id={`kc-${row.id}`}
                className="w-24"
                value={conc}
                placeholder={t('team:noLimit')}
                inputMode="numeric"
                onChange={(e) => setConc(e.target.value)}
              />
            </div>
            <div className="flex h-9 items-center">
              <Button size="sm" variant="outline" disabled={save.isPending || !valid} onClick={() => save.mutate()}>
                {t('common:save')}
              </Button>
            </div>
          </div>
        </OptionalSection>
      </div>
      {row.status !== 1 && !(row.credential_kind === 1 && row.status === 6) && (
        <div className="flex h-9 items-center">
          <Button
            size="sm"
            variant="outline"
            disabled={enable.isPending}
            onClick={() => enable.mutate()}
          >
            {t('admin:keyReEnable')}
          </Button>
        </div>
      )}
      {row.status === 6 && (
        <span className="flex h-9 items-center text-xs text-destructive">
          {t('admin:keyInvalid')}
        </span>
      )}
      {row.cooldown_until !== null && (
        <span className="flex h-9 items-center text-xs text-destructive">
          {t('admin:keyCooling', { until: row.cooldown_until })}
        </span>
      )}
      {row.failed_count > 0 && (
        <span className="flex h-9 items-center text-xs text-muted-foreground">
          {t('admin:keyFails', { n: row.failed_count })}
        </span>
      )}
      {showCredential && row.credential_kind === 1 && (
        <OAuthKeyHealth channelId={channelId} provider={provider} row={row} refreshEnabled={refreshEnabled} onDone={onDone} />
      )}
    </div>
  )
}
