import { Plus, Trash2 } from 'lucide-react'
import { useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { containsSecret, decimalText, isRecord, scaledInteger, settingMeta } from './setting-catalog'
import type { SettingRow } from './setting-catalog'
import { EPAY_FIELDS, initialFields, OAUTH_FIELDS, OAUTH_OPTIONAL_FIELDS, parseFields, STRIPE_FIELDS } from './setting-fields'
import type { FormError, SettingField } from './setting-fields'

export function SettingEditorDrawer({ row, pending, onCancel, onSave }: {
  row: SettingRow
  pending: boolean
  onCancel: () => void
  onSave: (value: unknown) => void
}) {
  const { t } = useTranslation()
  const formId = useId()
  const meta = settingMeta(row.key)
  const requestedMode = row.is_secret ? 'secret' : meta.editor !== 'auto' ? meta.editor
    : typeof row.value === 'boolean' ? 'bool' : typeof row.value === 'number' ? 'number'
    : typeof row.value === 'string' ? 'string' : 'json'
  // 旧值结构不匹配时保留原始编辑能力，不能把无法识别的数据初始化成空对象后覆盖。
  const unsupportedObject = ['epay', 'stripe', 'ssrf', 'limits'].includes(requestedMode) && row.value !== null && !isRecord(row.value)
  const unsupportedArray = requestedMode === 'oauth' && row.value !== null && (!Array.isArray(row.value) || !row.value.every(isRecord))
  const mode = unsupportedObject || unsupportedArray ? 'json' : requestedMode
  const fields = mode === 'epay' ? EPAY_FIELDS : mode === 'stripe' ? STRIPE_FIELDS : []
  const [draft, setDraft] = useState(() => initialFields(row.value, fields))
  const [text, setText] = useState(mode === 'percent' ? decimalText(row.value, 100) : row.is_secret ? '' : String(row.value ?? ''))
  const [bool, setBool] = useState(row.value === true)
  const [json, setJson] = useState(() => JSON.stringify(row.value, null, 2) ?? 'null')
  const [showJson, setShowJson] = useState(!containsSecret(row.value))
  const [dirty, setDirty] = useState(false)
  const nextId = useRef(0)
  const [limits, setLimits] = useState(() => Object.entries(isRecord(row.value) ? row.value : {}).map(([model, rpm]) => ({ id: nextId.current++, model, rpm: String(rpm) })))
  const [providers, setProviders] = useState(() => (Array.isArray(row.value) ? row.value : []).map((value) => ({ id: nextId.current++, value: initialFields(value, [...OAUTH_FIELDS, ...OAUTH_OPTIONAL_FIELDS]) })))
  const patch = (key: string, value: unknown) => { setDirty(true); setDraft((previous) => ({ ...previous, [key]: value })) }
  const label = meta.label ? t(meta.label) : row.key

  // 所有转换在提交前验证；对象的未识别字段随原值保留，避免表单覆盖扩展配置。
  const parsed = (() : { value: unknown; error?: FormError } => {
    if (mode === 'epay' || mode === 'stripe') return parseFields(draft, fields)
    if (mode === 'ssrf') return { value: { ...draft, allow_http: draft.allow_http === true, allow_private: draft.allow_private === true } }
    if (mode === 'percent') {
      const value = scaledInteger(text, 2)
      return { value, error: value === null || value > 10000 ? { key: 'admin:settingInvalidPercent' } : undefined }
    }
    if (mode === 'number') {
      const value = Number(text)
      return { value, error: text.trim() === '' || !Number.isFinite(value) ? { key: 'admin:settingInvalidNumber' } : undefined }
    }
    if (mode === 'bool') return { value: bool }
    if (mode === 'string' || mode === 'secret') return { value: text, error: mode === 'secret' && text.trim() === '' ? { key: 'admin:settingSecretRequired' } : undefined }
    if (mode === 'limits') {
      const names = limits.map((item) => item.model.trim())
      if (names.some((name) => !name) || new Set(names).size !== names.length) return { value: null, error: { key: 'admin:settingModelUnique' } }
      const values = limits.map((item) => scaledInteger(item.rpm, 0))
      if (values.some((value) => value === null)) return { value: null, error: { key: 'admin:settingRpmInvalid' } }
      return { value: Object.fromEntries(names.map((name, i) => [name, values[i]])) }
    }
    if (mode === 'oauth') {
      const values: Record<string, unknown>[] = []
      for (const provider of providers) {
        const result = parseFields(provider.value, [...OAUTH_FIELDS, ...OAUTH_OPTIONAL_FIELDS])
        if (result.error) return result
        const code = String(result.value.code)
        if (!/^[a-zA-Z0-9_-]+$/.test(code) || values.some((item) => item.code === code)) return { value: null, error: { key: 'admin:settingProviderUnique' } }
        if (!['github', 'discord', 'linuxdo'].includes(code) && ['authorize_url', 'token_url', 'userinfo_url'].some((key) => !result.value[key])) return { value: null, error: { key: 'admin:settingProviderUrlsRequired' } }
        values.push(result.value)
      }
      return { value: values }
    }
    try { return { value: JSON.parse(json) as unknown } }
    catch { return { value: null, error: { key: 'admin:advancedBadJson' } } }
  })()
  const error = parsed.error ? t(parsed.error.key, { field: parsed.error.field ? t(parsed.error.field) : '' }) : null
  const close = () => { if (!pending) onCancel() }

  return (
    <Drawer open title={label} description={t(meta.description)} onClose={close} footer={<>
      <Button variant="ghost" className="min-h-11 md:min-h-9" disabled={pending} onClick={close}>{t('common:cancel')}</Button>
      <Button type="submit" className="min-h-11 md:min-h-9" form={formId} loading={pending} disabled={!dirty || !!error}>{t('common:save')}</Button>
    </>}>
      <form id={formId} onSubmit={(e) => { e.preventDefault(); if (dirty && !pending && !error) onSave(parsed.value) }} className="flex min-w-0 flex-col gap-5">
        <fieldset disabled={pending} className="flex min-w-0 flex-col gap-5">
          {(mode === 'epay' || mode === 'stripe') && <ObjectFields fields={fields} draft={draft} onChange={patch} prefix={formId} />}
          {mode === 'ssrf' && <>
            <Switch label={t('admin:settingAllowHttp')} description={t('admin:settingAllowHttpHint')} checked={draft.allow_http === true} onChange={(v) => patch('allow_http', v)} />
            <Switch label={t('admin:settingAllowPrivate')} description={t('admin:settingAllowPrivateHint')} checked={draft.allow_private === true} onChange={(v) => patch('allow_private', v)} />
          </>}
          {(mode === 'percent' || mode === 'number' || mode === 'string' || mode === 'secret') && <Field label={mode === 'percent' ? t('admin:settingRebatePercent') : label} htmlFor={`${formId}-value`} hint={mode === 'percent' ? t('admin:settingRebateHint') : mode === 'secret' ? t('admin:settingSecretHint') : undefined}>
            <div className="flex items-center gap-2">
              <Input id={`${formId}-value`} className="h-11 md:h-9" inputMode={mode === 'percent' || mode === 'number' ? 'decimal' : undefined} type={mode === 'secret' ? 'password' : 'text'} autoComplete="off" value={text} onChange={(e) => { setText(e.target.value); setDirty(true) }} />
              {mode === 'percent' && <span>%</span>}
            </div>
          </Field>}
          {mode === 'bool' && <Switch label={label} checked={bool} onChange={(v) => { setBool(v); setDirty(true) }} />}
          {mode === 'limits' && <FieldGroup title={t('admin:settingModelLimits')} hint={t('admin:settingRpmHint')}>
            {limits.map((item) => <div key={item.id} className="grid grid-cols-[minmax(0,1fr)_5.5rem_auto] items-end gap-2">
              <Field label={t('admin:settingModelName')} htmlFor={`${formId}-model-${item.id}`}><Input id={`${formId}-model-${item.id}`} value={item.model} onChange={(e) => { setDirty(true); setLimits(limits.map((row) => row.id === item.id ? { ...row, model: e.target.value } : row)) }} /></Field>
              <Field label="RPM" htmlFor={`${formId}-rpm-${item.id}`}><Input id={`${formId}-rpm-${item.id}`} inputMode="numeric" value={item.rpm} onChange={(e) => { setDirty(true); setLimits(limits.map((row) => row.id === item.id ? { ...row, rpm: e.target.value } : row)) }} /></Field>
              <Button variant="ghost" size="icon" aria-label={t('admin:settingRemoveRule', { n: item.id + 1 })} onClick={() => { setDirty(true); setLimits(limits.filter((row) => row.id !== item.id)) }}><Trash2 aria-hidden className="h-4 w-4" /></Button>
            </div>)}
            <Button variant="outline" className="self-start" onClick={() => { setDirty(true); setLimits([...limits, { id: nextId.current++, model: '', rpm: '' }]) }}><Plus aria-hidden className="h-4 w-4" />{t('admin:settingAddRule')}</Button>
          </FieldGroup>}
          {mode === 'oauth' && <>
            {providers.map((provider, index) => <div key={provider.id} className="flex flex-col gap-4 rounded-lg border border-border p-4">
              <div className="flex items-center justify-between gap-2"><h3 className="text-sm font-medium">{t('admin:settingProviderNumber', { n: index + 1 })}</h3><Button variant="ghost" size="icon" aria-label={t('admin:settingRemoveProvider', { n: index + 1 })} onClick={() => { setDirty(true); setProviders(providers.filter((item) => item.id !== provider.id)) }}><Trash2 aria-hidden className="h-4 w-4" /></Button></div>
              <ObjectFields fields={OAUTH_FIELDS} draft={provider.value} prefix={`${formId}-${provider.id}`} onChange={(key, value) => { setDirty(true); setProviders(providers.map((item) => item.id === provider.id ? { ...item, value: { ...item.value, [key]: value } } : item)) }} />
              <details className="min-w-0" open={!['github', 'discord', 'linuxdo'].includes(String(provider.value.code)) || undefined}>
                <summary className="cursor-pointer py-2 text-xs text-primary">{t('admin:settingProviderAdvanced')}</summary>
                <p className="mb-3 text-xs leading-5 text-muted-foreground">{t('admin:settingProviderPresetHint')}</p>
                <ObjectFields fields={OAUTH_OPTIONAL_FIELDS} draft={provider.value} prefix={`${formId}-${provider.id}`} onChange={(key, value) => { setDirty(true); setProviders(providers.map((item) => item.id === provider.id ? { ...item, value: { ...item.value, [key]: value } } : item)) }} />
              </details>
            </div>)}
            <Button variant="outline" className="self-start" onClick={() => { setDirty(true); setProviders([...providers, { id: nextId.current++, value: initialFields({}, [...OAUTH_FIELDS, ...OAUTH_OPTIONAL_FIELDS]) }]) }}><Plus aria-hidden className="h-4 w-4" />{t('admin:settingAddProvider')}</Button>
          </>}
          {mode === 'json' && <Field label={t('admin:settingRawJson')} htmlFor={`${formId}-json`} hint={t('admin:settingRawJsonHint')}>
            {showJson ? <Textarea id={`${formId}-json`} rows={12} spellCheck={false} className="font-mono text-xs" value={json} onChange={(e) => { setDirty(true); setJson(e.target.value) }} />
              : <Button variant="outline" onClick={() => setShowJson(true)}>{t('admin:settingRevealJson')}</Button>}
          </Field>}
        </fieldset>
        {dirty && error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <code className="text-[11px] break-all text-muted-foreground">{row.key}</code>
      </form>
    </Drawer>
  )
}

function ObjectFields({ fields, draft, onChange, prefix }: {
  fields: SettingField[]
  draft: Record<string, unknown>
  onChange: (key: string, value: string) => void
  prefix: string
}) {
  const { t } = useTranslation()
  return <div className="flex min-w-0 flex-col gap-4">{fields.map((field) => <Field key={field.key} label={t(field.label)} htmlFor={`${prefix}-${field.key}`} required={!field.optional} hint={field.kind === 'rate' ? t('admin:settingRateHint') : field.optional ? t('admin:settingOptionalHint') : undefined}>
    <Input id={`${prefix}-${field.key}`} className="h-11 md:h-9" type={field.kind === 'secret' ? 'password' : 'text'} autoComplete="off" inputMode={field.kind === 'rate' ? 'decimal' : field.kind === 'url' ? 'url' : undefined} value={String(draft[field.key] ?? '')} placeholder={field.placeholder} onChange={(e) => onChange(field.key, e.target.value)} />
  </Field>)}</div>
}
