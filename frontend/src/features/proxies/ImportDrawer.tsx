import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { proxyGroupOptions } from './options'
import type { ImportResult } from './types'

const SCHEMES = ['socks5h', 'socks5', 'http', 'https'] as const

function parseCap(raw: string): number | null | undefined {
  const trimmed = raw.trim()
  if (trimmed === '') return null
  const n = Number(trimmed)
  return Number.isInteger(n) && n > 0 && n <= 2_147_483_647 ? n : undefined
}

/// 批量导入代理：每行一个，兼容完整 URL 与代理商常给的三种写法；同一地址（协议 + 主机 + 端口 + 用户名）
/// 已存在的跳过。可选直接加进某个代理组（固定分配组当场给排队的 key 分配）。导入结果逐行列出。
export function ImportDrawer({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { t } = useTranslation()
  const [text, setText] = useState('')
  const [scheme, setScheme] = useState<(typeof SCHEMES)[number]>('socks5h')
  const [prefix, setPrefix] = useState('')
  const [cap, setCap] = useState('')
  const [concurrencyRaw, setConcurrencyRaw] = useState('')
  const [group, setGroup] = useState('')
  const [result, setResult] = useState<ImportResult | null>(null)
  const groups = useQuery(proxyGroupOptions())
  const maxKeys = parseCap(cap)
  const concurrency = parseCap(concurrencyRaw)
  const lines = text.split('\n').filter((l) => l.trim() !== '' && !l.trim().startsWith('#')).length

  const save = useMutation({
    mutationFn: () =>
      apiFetch<ImportResult>('/admin/proxies/import', {
        method: 'POST',
        body: {
          text,
          default_scheme: scheme,
          ...(prefix.trim() === '' ? {} : { name_prefix: prefix.trim() }),
          ...(maxKeys ? { max_keys: maxKeys } : {}),
          ...(concurrency ? { max_concurrency: concurrency } : {}),
          ...(group === '' ? {} : { group_code: group }),
        },
      }),
    onSuccess: (r) => {
      setResult(r)
      onDone()
      if (r.assignment && r.assignment.unassigned > 0) {
        toast.warning(t('admin:egressUnassignedWarn', { n: r.assignment.unassigned }))
      } else {
        toast.success(t('admin:proxyImportDone', { created: r.created.length, skipped: r.skipped.length }))
      }
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:proxyImport')}
      description={t('admin:proxyImportDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:close')}
          </Button>
          <Button
            disabled={lines === 0 || lines > 1000 || maxKeys === undefined || concurrency === undefined || save.isPending}
            onClick={() => save.mutate()}
          >
            {t('admin:proxyImportSubmit', { n: lines })}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('admin:proxyImportLines')} hint={t('admin:proxyImportFormats')}>
        <Textarea
          id="px-import"
          rows={10}
          className="font-mono text-xs"
          spellCheck={false}
          value={text}
          placeholder={'socks5h://user:pass@1.2.3.4:1080\n1.2.3.4:1080:user:pass\nuser:pass@1.2.3.4:1080\n1.2.3.4:1080'}
          onChange={(e) => {
            setText(e.target.value)
            setResult(null)
          }}
        />
        <Field label={t('admin:proxyImportScheme')} htmlFor="px-import-scheme" hint={t('admin:proxyImportSchemeHint')}>
          <Select
            id="px-import-scheme"
            className="w-40"
            value={scheme}
            onChange={(v) => setScheme(v as (typeof SCHEMES)[number])}
            options={SCHEMES.map((s) => ({ value: s, label: s }))}
          />
        </Field>
      </FieldGroup>

      <FieldGroup title={t('admin:proxyImportDefaults')}>
        <div className="grid grid-cols-3 gap-3">
          <Field label={t('admin:proxyImportPrefix')} htmlFor="px-import-prefix" hint={t('admin:proxyImportPrefixHint')}>
            <Input id="px-import-prefix" value={prefix} maxLength={100} onChange={(e) => setPrefix(e.target.value)} />
          </Field>
          <Field
            label={t('admin:proxyMaxKeys')}
            htmlFor="px-import-cap"
            error={maxKeys === undefined ? t('errors:bad_request', { param: 'max_keys' }) : undefined}
          >
            <Input id="px-import-cap" inputMode="numeric" value={cap}
              placeholder={t('admin:channelLimitUnlimited')} onChange={(e) => setCap(e.target.value)} />
          </Field>
          <Field
            label={t('admin:proxyMaxConcurrency')}
            htmlFor="px-import-concurrency"
            error={concurrency === undefined ? t('errors:bad_request', { param: 'max_concurrency' }) : undefined}
          >
            <Input id="px-import-concurrency" inputMode="numeric" value={concurrencyRaw}
              placeholder={t('admin:channelLimitUnlimited')} onChange={(e) => setConcurrencyRaw(e.target.value)} />
          </Field>
        </div>
        <Field label={t('admin:proxyImportGroup')} htmlFor="px-import-group" hint={t('admin:proxyImportGroupHint')}>
          <Select
            id="px-import-group"
            className="w-64"
            value={group}
            placeholder={t('admin:proxyImportNoGroup')}
            onChange={setGroup}
            options={(groups.data ?? []).map((g) => ({ value: g.code, label: `${g.name} (${g.code})` }))}
          />
        </Field>
      </FieldGroup>

      {result && (
        <FieldGroup title={t('admin:proxyImportResult')}>
          <div role="status" className="flex flex-wrap gap-2 text-xs">
            <Badge variant="success">{t('admin:proxyImportCreated', { n: result.created.length })}</Badge>
            {result.skipped.length > 0 && (
              <Badge variant="warning">{t('admin:proxyImportSkipped', { n: result.skipped.length })}</Badge>
            )}
          </div>
          {result.skipped.length > 0 && (
            <ul className="flex flex-col gap-0.5 text-xs text-muted-foreground">
              {result.skipped.map((s) => (
                <li key={s.line}>
                  {t('admin:proxyImportLine', { n: s.line })}：
                  {t(s.reason === 'duplicate' ? 'admin:proxyImportDuplicate' : 'admin:proxyImportInvalid')}
                </li>
              ))}
            </ul>
          )}
        </FieldGroup>
      )}
    </Drawer>
  )
}
