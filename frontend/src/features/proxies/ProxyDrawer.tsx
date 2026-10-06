import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { ProbeSummary } from './ProbeSummary'
import type { ProbeResult, ProxyRow, ReconcileReport } from './types'

const URL_PATTERN = /^(https?|socks5h?):\/\/[^\s/]+\/?$/i

function parseCap(raw: string): number | null | undefined {
  const trimmed = raw.trim()
  if (trimmed === '') return null
  const n = Number(trimmed)
  return Number.isInteger(n) && n > 0 && n <= 2_147_483_647 ? n : undefined
}

/// 代理抽屉：新建与编辑共用。
///
/// 地址含认证信息：编辑时不回显（后端只给掩码），留空 = 保持原地址；填了就整条替换，
/// 熔断与上次测试结果随之清掉（那些事实属于旧地址）。保存前可先「测试」一次：
/// 不落库，直接看出口 IP 与延迟，免得把一个打不通的地址绑到渠道上。
export function ProxyDrawer({
  proxy,
  onClose,
  onDone,
}: {
  proxy: ProxyRow | undefined
  onClose: () => void
  onDone: (report?: ReconcileReport) => void
}) {
  const { t } = useTranslation()
  const isEdit = proxy !== undefined
  const [name, setName] = useState(proxy?.name ?? '')
  const [url, setUrl] = useState('')
  const [cap, setCap] = useState(proxy?.max_keys == null ? '' : String(proxy.max_keys))
  const [concurrencyRaw, setConcurrencyRaw] = useState(
    proxy?.max_concurrency == null ? '' : String(proxy.max_concurrency),
  )
  const concurrency = parseCap(concurrencyRaw)
  const [note, setNote] = useState(proxy?.note ?? '')
  const [enabled, setEnabled] = useState((proxy?.status ?? 1) === 1)
  const [probe, setProbe] = useState<ProbeResult | null>(null)
  const maxKeys = parseCap(cap)
  const urlValid = url.trim() === '' ? isEdit : URL_PATTERN.test(url.trim())

  const test = useMutation({
    mutationFn: () =>
      apiFetch<ProbeResult>('/admin/proxies/test', { method: 'POST', body: { url: url.trim() } }),
    onSuccess: setProbe,
    onError: (err) => toast.error(describeError(err)),
  })

  const save = useMutation({
    mutationFn: () =>
      isEdit
        ? apiFetch<{ assignment?: ReconcileReport }>(`/admin/proxies/${proxy.id}`, {
            method: 'PATCH',
            body: {
              name: name.trim(),
              ...(url.trim() === '' ? {} : { url: url.trim() }),
              max_keys: maxKeys ?? null,
              max_concurrency: concurrency ?? null,
              note: note.trim() === '' ? null : note.trim(),
              status: enabled ? 1 : 2,
            },
          })
        : apiFetch<{ assignment?: ReconcileReport }>('/admin/proxies', {
            method: 'POST',
            body: {
              ...(name.trim() === '' ? {} : { name: name.trim() }),
              url: url.trim(),
              max_keys: maxKeys ?? undefined,
              max_concurrency: concurrency ?? undefined,
              note: note.trim() === '' ? undefined : note.trim(),
              status: enabled ? 1 : 2,
            },
          }),
    onSuccess: (r) => {
      onDone(r.assignment)
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={isEdit ? t('admin:proxyEdit', { name: proxy.name }) : t('admin:proxyCreate')}
      description={t('admin:proxyDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={!urlValid || maxKeys === undefined || concurrency === undefined
              || (isEdit && name.trim() === '') || save.isPending}
            onClick={() => save.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')}>
        <Field label={t('admin:proxyName')} htmlFor="px-name" hint={isEdit ? undefined : t('admin:proxyNameHint')}>
          <Input id="px-name" value={name} maxLength={128} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field
          label={t('admin:proxyUrlLabel')}
          htmlFor="px-url"
          hint={isEdit ? t('admin:proxyUrlKeepHint', { current: proxy.url_masked }) : t('admin:proxyUrlFormatHint')}
          error={!urlValid ? t('errors:bad_request', { param: 'url' }) : undefined}
        >
          <div className="flex gap-2">
            <Input
              id="px-url"
              className="font-mono text-sm"
              type="text"
              autoComplete="off"
              spellCheck={false}
              value={url}
              placeholder={isEdit ? proxy.url_masked : 'socks5h://user:pass@1.2.3.4:1080'}
              aria-invalid={!urlValid}
              onChange={(e) => {
                setUrl(e.target.value)
                setProbe(null)
              }}
            />
            <Button
              variant="outline"
              disabled={url.trim() === '' || !urlValid}
              loading={test.isPending}
              onClick={() => test.mutate()}
            >
              {t('admin:proxyTest')}
            </Button>
          </div>
        </Field>
        {probe && <ProbeSummary result={probe} />}
        <p role="note" className="text-xs text-muted-foreground">{t('admin:proxySocksDnsHint')}</p>
      </FieldGroup>

      <FieldGroup title={t('admin:proxyCapacity')} hint={t('admin:proxyCapacityHint')}>
        <Field
          label={t('admin:proxyMaxKeys')}
          htmlFor="px-cap"
          error={maxKeys === undefined ? t('errors:bad_request', { param: 'max_keys' }) : undefined}
        >
          <Input
            id="px-cap"
            className="w-32"
            inputMode="numeric"
            value={cap}
            placeholder={t('admin:channelLimitUnlimited')}
            aria-invalid={maxKeys === undefined}
            onChange={(e) => setCap(e.target.value)}
          />
        </Field>
        <Field
          label={t('admin:proxyMaxConcurrency')}
          htmlFor="px-concurrency"
          hint={t('admin:proxyMaxConcurrencyHint')}
          error={concurrency === undefined ? t('errors:bad_request', { param: 'max_concurrency' }) : undefined}
        >
          <Input
            id="px-concurrency"
            className="w-32"
            inputMode="numeric"
            value={concurrencyRaw}
            placeholder={t('admin:channelLimitUnlimited')}
            aria-invalid={concurrency === undefined}
            onChange={(e) => setConcurrencyRaw(e.target.value)}
          />
        </Field>
      </FieldGroup>

      <FieldGroup title={t('common:description')}>
        <Field label={t('admin:proxyNote')} htmlFor="px-note">
          <Input id="px-note" value={note} maxLength={255} onChange={(e) => setNote(e.target.value)} />
        </Field>
        <Switch
          label={t('common:enabled')}
          description={t('admin:proxyEnabledHint')}
          checked={enabled}
          onChange={setEnabled}
        />
      </FieldGroup>
    </Drawer>
  )
}
