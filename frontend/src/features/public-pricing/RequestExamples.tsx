import { Check, Copy, Download, Terminal } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { PricingModel } from './types'
import { buildRequestExample, defaultApiBase, requestTemplates } from './request-examples'
import type { RequestTemplate } from './request-examples'
import { Button } from '@/components/ui/button'
import { CopyButton, useCopy } from '@/components/ui/copy-button'
import { Input, Label, Textarea } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'

export function RequestExamples({ model }: { model: PricingModel }) {
  const { t } = useTranslation()
  const [base, setBase] = useState(() => defaultApiBase(window.location.origin, import.meta.env.VITE_GATEWAY_BASE_URL))
  const [template, setTemplate] = useState<RequestTemplate>(model.capabilities?.embedding === true ? 'embeddings' : 'chat')
  const [prompt, setPrompt] = useState(t('catalog:samplePrompt'))
  const [stream, setStream] = useState(false)
  const [language, setLanguage] = useState<'curl' | 'python' | 'javascript' | 'body'>('curl')
  const { copy, copied } = useCopy()
  const example = buildRequestExample(base, template, model.model, prompt, stream)
  const supportsStream = requestTemplates.find((item) => item.id === template)?.stream
  const downloadShell = () => {
    if (!example) return
    const url = URL.createObjectURL(new Blob([`#!/usr/bin/env sh\n# First set your key: export OKAPI_API_KEY='YOUR_API_KEY'\nset -eu\n\n${example.curl}\n`], { type: 'text/x-shellscript;charset=utf-8' }))
    const link = document.createElement('a')
    link.href = url
    link.download = `okapi-${template}.sh`
    link.click()
    window.setTimeout(() => URL.revokeObjectURL(url), 1000)
  }
  return <section className="flex flex-col gap-4">
    <p className="text-xs leading-5 text-muted-foreground">{t('catalog:exampleHint')}</p>
    <div className="flex flex-col gap-2">
      <Label htmlFor="example-base">{t('catalog:apiBase')}</Label>
      <div className="flex items-center gap-2"><Input id="example-base" value={base} onChange={(e) => setBase(e.target.value)} placeholder="https://api.example.com/v1" aria-invalid={!example} aria-describedby="example-base-hint" spellCheck={false} />
        {example && <CopyButton value={example.base} label={t('catalog:copyBase')} />}</div>
      <p id="example-base-hint" className="text-xs leading-5 text-muted-foreground">{t('catalog:baseHint')}</p>
      {!example && <p role="alert" className="text-xs text-destructive">{t('catalog:invalidBase')}</p>}
    </div>
    <div className="grid grid-cols-1 items-end gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
      <div className="flex flex-col gap-2"><Label htmlFor="example-template">{t('catalog:apiTemplate')}</Label><Select id="example-template" value={template} onChange={(value) => setTemplate(value as RequestTemplate)} options={requestTemplates.map((item) => ({ value: item.id, label: item.name }))} /></div>
      {supportsStream && <label className="flex min-h-9 cursor-pointer items-center gap-2 text-xs"><input type="checkbox" className="h-4 w-4 accent-primary" checked={stream} onChange={(e) => setStream(e.target.checked)} />{t('catalog:streamResponse')}</label>}
    </div>
    {example && <div className="flex min-w-0 items-start gap-2 rounded-lg border border-border bg-muted/30 p-3">
      <span className="pt-1 text-xs font-semibold text-primary">POST</span><code className="min-w-0 flex-1 break-all pt-1 text-xs leading-5">{example.url}</code><CopyButton value={example.url} label={t('catalog:copyEndpoint')} />
    </div>}
    <div className="grid grid-cols-1 items-start gap-2 sm:grid-cols-2">
      <details className="rounded-lg border border-border p-3">
        <summary className="cursor-pointer rounded text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{t('catalog:editRequestInput')}</summary>
        <div className="mt-3 flex flex-col gap-2"><Label htmlFor="example-prompt">{t('catalog:requestInput')}</Label><Textarea id="example-prompt" value={prompt} onChange={(e) => setPrompt(e.target.value)} className="min-h-20" /></div>
      </details>
      <details className="rounded-lg border border-border p-3">
        <summary className="cursor-pointer rounded text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{t('catalog:setupKey')}</summary>
        <p className="mt-3 text-xs leading-5 text-muted-foreground">{t('catalog:keyPlaceholderHint')}</p>
        <div className="mt-3 flex min-w-0 items-center gap-2 rounded-lg bg-muted/60 p-2.5"><code className="min-w-0 flex-1 break-all text-xs">export OKAPI_API_KEY='YOUR_API_KEY'</code><CopyButton value="export OKAPI_API_KEY='YOUR_API_KEY'" label={t('catalog:copyKeySetup')} /></div>
      </details>
    </div>
    <div className="min-w-0 overflow-hidden rounded-xl border border-border">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-muted/40 p-3">
        <Segmented size="sm" ariaLabel={t('catalog:exampleLanguage')} value={language} onChange={setLanguage} options={[
          { value: 'curl', label: 'cURL', icon: Terminal }, { value: 'python', label: 'Python' }, { value: 'javascript', label: 'JavaScript' }, { value: 'body', label: 'JSON' },
        ]} />
        <Button size="sm" variant="outline" disabled={!example} onClick={() => example && void copy(example[language])}>{copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}{t('catalog:copyExample')}</Button>
      </div>
      <pre tabIndex={0} aria-label={t('catalog:generatedExample')} className="max-h-[420px] min-w-0 overflow-auto bg-muted/15 p-4 text-xs leading-6 outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40"><code>{example?.[language] ?? t('catalog:invalidBase')}</code></pre>
    </div>
    <div className="flex flex-wrap items-center justify-between gap-3">
      <p className="text-xs text-muted-foreground">{language === 'javascript' ? t('catalog:nodeRuntime') : language === 'python' ? t('catalog:pythonRuntime') : language === 'body' ? t('catalog:jsonRuntime') : t('catalog:shellRuntime')}</p>
      <Button variant="outline" size="sm" disabled={!example} onClick={downloadShell}><Download className="h-3.5 w-3.5" />{t('catalog:downloadShell')}</Button>
    </div>
    <p className="text-xs leading-5 text-muted-foreground">{t('catalog:exampleGroupHint')}</p>
  </section>
}
