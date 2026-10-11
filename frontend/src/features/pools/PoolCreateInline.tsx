import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { ErrorState } from '@/components/ui/state'
import { apiFetch, ApiError } from '@/lib/api'
import { isMachineCode } from '@/lib/codes'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { poolOptions } from './pool-options'
import type { PoolOptions } from './pool-options'
import type { PoolRow } from './types'

/** A pool can be created without leaving an unsaved channel form. */
export function PoolCreateInline({ pools, onCreated, onCancel }: {
  pools: PoolRow[]
  onCreated: (code: string) => void
  onCancel: () => void
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [code, setCode] = useState('')
  const trimmed = code.trim()
  const exists = pools.some((pool) => pool.pool_code === trimmed)
  const invalid = trimmed !== '' && !isMachineCode(trimmed)
  const create = useMutation({
    mutationFn: async () => {
      // POST also edits existing pools. Refresh the catalog before creating to
      // catch codes that have been added since this form was opened.
      const latest = await queryClient.fetchQuery({ ...poolOptions(), staleTime: 0 })
      if (latest.data.some((pool) => pool.pool_code === trimmed)) {
        throw new ApiError(409, 'conflict', 'pool_code')
      }
      await apiFetch('/admin/pools', { method: 'POST', body: {
        pool_code: trimmed, description: '', routing_strategy: 'priority_weighted', fallback_pool_code: null,
      } })
      return { pool_code: trimmed, description: '', routing_strategy: 'priority_weighted', fallback_pool_code: null,
        builtin: false, channel_count: 0, group_count: 0, key_count: 0, fallback_ref_count: 0 } satisfies PoolRow
    },
    onSuccess: (pool) => {
      queryClient.setQueryData<PoolOptions>(qk.adminPoolOptions, (current) => {
        const data = [...(current?.data ?? []).filter((p) => p.pool_code !== pool.pool_code), pool]
        return { data, total: data.length }
      })
      void queryClient.invalidateQueries({ queryKey: qk.adminPools })
      onCreated(pool.pool_code)
    },
  })
  return <form className="flex min-w-0 flex-col gap-3 rounded-md border border-border bg-muted/20 p-3"
    aria-label={t('admin:poolCreate')} onSubmit={(event) => {
      event.preventDefault()
      if (trimmed && !exists && !invalid && !create.isPending) create.mutate()
    }}>
    <div className="flex flex-col gap-1.5">
      <Label htmlFor="channel-new-pool-code">{t('admin:poolCode')}</Label>
      <Input id="channel-new-pool-code" value={code} maxLength={32} placeholder="stable"
        autoFocus disabled={create.isPending} aria-invalid={exists || invalid}
        onChange={(event) => { setCode(event.target.value); create.reset() }} />
      {exists && <p role="alert" className="text-xs text-destructive">{t('admin:poolCodeExists')}</p>}
      {invalid && <p role="alert" className="text-xs text-destructive">{t('admin:codeFormatInvalid', { max: 32 })}</p>}
    </div>
    <p className="text-xs leading-5 text-muted-foreground">{t('admin:poolCreateInlineHint')}</p>
    {create.isError && <ErrorState message={describeError(create.error)} />}
    <div className="flex flex-wrap gap-2">
      <Button size="sm" type="submit" disabled={!trimmed || exists || invalid} loading={create.isPending}>
        {t('admin:poolCreateAndSelect')}
      </Button>
      <Button size="sm" variant="ghost" disabled={create.isPending} onClick={onCancel}>{t('common:cancel')}</Button>
    </div>
  </form>
}
