import { useMutation } from '@tanstack/react-query'
import { Textarea } from '@/components/ui/textarea'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

export const Route = createFileRoute('/admin/pricing')({
  component: PricingPage,
})

function PricingPage() {
  const { t } = useTranslation()
  const [msg, setMsg] = useState<string | null>(null)
  const [form, setForm] = useState({
    model_name: '',
    model_ratio: '1',
    completion_ratio: '1',
    cache_ratio: '1',
    cache_write_ratio: '1',
  })
  const [tierJson, setTierJson] = useState('')

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/models', {
        method: 'POST',
        body: {
          ...form,
          tier_ratios: tierJson.trim() ? (JSON.parse(tierJson) as unknown) : undefined,
        },
      }),
    onSuccess: () => setMsg(t('common:success')),
    onError: (err) =>
      setMsg(err instanceof SyntaxError ? t('admin:advancedBadJson') : describeError(err)),
  })
  const publish = useMutation({
    mutationFn: () =>
      apiFetch<{ epoch: number }>('/admin/pricing/publish', { method: 'POST', body: {} }),
    onSuccess: (data) => setMsg(t('admin:publishedEpoch', { epoch: data.epoch })),
    onError: (err) => setMsg(describeError(err)),
  })

  const [importJson, setImportJson] = useState('')
  const [importMsg, setImportMsg] = useState<string | null>(null)
  const importRun = useMutation({
    mutationFn: () => {
      const parsed: unknown = JSON.parse(importJson)
      return apiFetch<{ imported: number; skipped: string[] }>(
        '/admin/pricing/import-newapi',
        { method: 'POST', body: parsed },
      )
    },
    onSuccess: (r) =>
      setImportMsg(
        t('admin:importResult', { imported: r.imported, skipped: r.skipped.length }),
      ),
    onError: (err) => setImportMsg(describeError(err)),
  })

  const fields = [
    ['model_name', t('admin:modelName')],
    ['model_ratio', t('admin:modelRatio')],
    ['completion_ratio', t('admin:completionRatio')],
    ['cache_ratio', t('admin:cacheRatio')],
    ['cache_write_ratio', t('admin:cacheWriteRatio')],
  ] as const

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:pricing')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={field}>{label}</Label>
              <Input
                id={field}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="tier-ratios">{t('admin:tierRatios')}</Label>
          <Input
            id="tier-ratios"
            className="font-mono text-xs"
            value={tierJson}
            onChange={(e) => setTierJson(e.target.value)}
            placeholder='{"flex":"0.5","priority":"2.0"}'
          />
        </div>
        <div className="flex items-center gap-3">
          <Button
            variant="outline"
            disabled={upsert.isPending || !form.model_name}
            onClick={() => upsert.mutate()}
          >
            {t('admin:upsertModel')}
          </Button>
          <Button disabled={publish.isPending} onClick={() => publish.mutate()}>
            {t('admin:publish')}
          </Button>
          {msg && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        <div className="mt-4 flex flex-col gap-2 border-t border-border pt-4">
          <Label htmlFor="import">{t('admin:importTitle')}</Label>
          <Textarea
            id="import"
            rows={6}
            value={importJson}
            placeholder={t('admin:importPlaceholder')}
            onChange={(e) => setImportJson(e.target.value)}
          />
          <div className="flex items-center gap-3">
            <Button
              variant="outline"
              disabled={importRun.isPending || !importJson.trim()}
              onClick={() => importRun.mutate()}
            >
              {t('admin:importRun')}
            </Button>
            {importMsg && <span className="text-xs text-muted-foreground">{importMsg}</span>}
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
