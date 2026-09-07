import { useQuery } from '@tanstack/react-query'
import { ExternalLink, FileCode2, Terminal } from 'lucide-react'
import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { CLIENTS, buildSnippets, resolveConnectConfig } from './connect-snippets'
import type { ClientId, ConnectConfig, Snippet } from './connect-snippets'
import { buildImportLinks } from './import-links'
import type { ImportTarget } from './import-links'
import { CopyButton, CopyText } from '@/components/ui/copy-button'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { isAvailable } from '@/features/public-pricing/catalog-data'
import { defaultApiBase } from '@/features/public-pricing/request-examples'
import type { PricingGroup, PricingModel } from '@/features/public-pricing/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

// 产品名不翻译；"通用客户端"一项在渲染时取文案键。
const CLIENT_NAMES: Record<Exclude<ClientId, 'apps'>, string> = {
  curl: 'cURL', python: 'Python', node: 'Node.js', claude: 'Claude Code', codex: 'Codex CLI',
}

/// 接入配置面板：Base URL + 模型 ID + 按客户端生成的配置。
///
/// 模型候选只列本分组已接入的（模型广场同一口径），但允许手输——站长可能刚加了模型
/// 而目录缓存还没刷新。密钥只在从"密钥已创建"回执打开时填入明文，否则一律占位符：
/// 浏览器里那把登录 key 是门户会话凭证，从不被读出来拼进配置。
export function ConnectPanel({ apiKey, group }: { apiKey?: string; group: string }) {
  const { t } = useTranslation()
  const ids = { base: useId(), model: useId(), list: useId() }
  const [base, setBase] = useState(() => defaultApiBase(window.location.origin, import.meta.env.VITE_GATEWAY_BASE_URL))
  const [model, setModel] = useState<string | null>(null)
  const [client, setClient] = useState<ClientId>('curl')
  const pricing = useQuery({
    queryKey: qk.publicPricing,
    queryFn: () => apiFetch<{ models: PricingModel[]; groups: PricingGroup[] }>('/api/pricing'),
    staleTime: 60_000,
  })
  const available = (pricing.data?.models ?? [])
    .filter((m) => isAvailable(m, group))
    .map((m) => m.model)
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
  const modelValue = model ?? available[0] ?? ''
  const cfg = resolveConnectConfig(base, modelValue, apiKey)
  const hint = client === 'claude' ? t('portal:guideClaudeHint')
    : client === 'codex' ? t('portal:guideCodexHint')
    : client === 'apps' ? t('portal:guideAppsHint')
    : client === 'curl' ? t('catalog:shellRuntime')
    : t('portal:guideSdkHint')

  return (
    <div className="flex flex-col gap-4">
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={t('catalog:apiBase')} htmlFor={ids.base} hint={t('catalog:baseHint')} error={cfg ? null : t('catalog:invalidBase')}>
          <div className="flex items-center gap-2">
            <Input id={ids.base} value={base} onChange={(e) => setBase(e.target.value)} spellCheck={false} aria-invalid={!cfg} />
            {cfg && <CopyButton value={cfg.base} label={t('catalog:copyBase')} />}
          </div>
        </Field>
        <Field label={t('portal:guideModel')} htmlFor={ids.model} hint={t('portal:guideModelHint')}>
          <div className="flex items-center gap-2">
            <Input id={ids.model} list={ids.list} value={modelValue} onChange={(e) => setModel(e.target.value)} spellCheck={false} placeholder={t('portal:guideModelPlaceholder')} />
            {modelValue !== '' && <CopyButton value={modelValue} label={t('catalog:copyModel')} />}
          </div>
          <datalist id={ids.list}>
            {available.map((m) => <option key={m} value={m} />)}
          </datalist>
        </Field>
      </div>

      <Segmented
        size="sm"
        ariaLabel={t('portal:guideClient')}
        value={client}
        onChange={setClient}
        options={CLIENTS.map((id) => ({ value: id, label: id === 'apps' ? t('portal:guideClientApps') : CLIENT_NAMES[id] }))}
      />
      <p className="text-xs leading-5 text-muted-foreground">{hint}</p>

      {cfg && (client === 'apps'
        ? <AppFields cfg={cfg} />
        : buildSnippets(client, cfg, t('catalog:samplePrompt')).map((snippet, i) => <SnippetBlock key={`${client}-${i}`} snippet={snippet} />))}

      {/* 一键导入只在明文在场时出现：链接里必然带 key，占位符没有意义（§11.31 同一规则） */}
      {cfg && apiKey !== undefined && <ImportLinks cfg={cfg} />}

      <p className="text-xs leading-5 text-muted-foreground">
        {apiKey === undefined ? t('portal:guideKeyPlaceholder') : t('portal:guideKeyFilled')}
      </p>
    </div>
  )
}

const IMPORT_LABEL: Record<ImportTarget, string> = {
  'ccswitch-claude': 'portal:guideImportCcClaude',
  'ccswitch-codex': 'portal:guideImportCcCodex',
  nextchat: 'portal:guideImportNextChat',
  cherry: 'portal:guideImportCherry',
}

/// 聊天客户端一键导入（§11.39）：cc-switch / NextChat / Cherry Studio。
export function ImportLinks({ cfg }: { cfg: ConnectConfig }) {
  const { t } = useTranslation()
  const links = buildImportLinks(cfg, window.location.hostname)
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border p-3" data-testid="import-links">
      <span className="text-xs font-medium">{t('portal:guideImportTo')}</span>
      <div className="flex flex-wrap gap-2">
        {links.map((link) => (
          <a
            key={link.target}
            href={link.href}
            target={link.scheme ? undefined : '_blank'}
            rel={link.scheme ? undefined : 'noreferrer noopener'}
            className="inline-flex h-8 items-center gap-1.5 rounded-md border border-border bg-card px-3 text-xs font-medium hover:bg-accent/60"
          >
            <ExternalLink aria-hidden className="h-3.5 w-3.5 text-muted-foreground" />
            {t(IMPORT_LABEL[link.target])}
          </a>
        ))}
      </div>
      <p className="text-xs leading-5 text-muted-foreground">{t('portal:guideImportHint')}</p>
    </div>
  )
}

function SnippetBlock({ snippet }: { snippet: Snippet }) {
  const { t } = useTranslation()
  const Icon = snippet.target === 'terminal' ? Terminal : FileCode2
  return (
    <div className="min-w-0 overflow-hidden rounded-lg border border-border">
      <div className="flex items-center justify-between gap-2 border-b border-border bg-muted/40 px-3 py-1.5">
        <span className="flex min-w-0 items-center gap-1.5 text-xs font-medium">
          <Icon aria-hidden className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate font-mono">{snippet.path ?? t('portal:guideTerminal')}</span>
        </span>
        <CopyButton value={snippet.code} label={t('catalog:copyExample')} size="xs" />
      </div>
      <pre tabIndex={0} className="max-h-72 min-w-0 overflow-auto bg-muted/15 p-3 text-xs leading-5 outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40">
        <code>{snippet.code}</code>
      </pre>
    </div>
  )
}

/// 图形客户端（Cursor / Cherry Studio / ChatBox…）没有配置文件可贴，给它们要填的四个字段。
function AppFields({ cfg }: { cfg: ConnectConfig }) {
  const { t } = useTranslation()
  const rows: Array<[string, string, boolean]> = [
    [t('portal:guideFieldType'), t('portal:guideFieldTypeValue'), false],
    [t('portal:guideFieldBase'), cfg.base, true],
    ['API Key', cfg.key, true],
    [t('portal:guideFieldModel'), cfg.model, true],
  ]
  return (
    <dl className="divide-y divide-border rounded-lg border border-border text-sm">
      {rows.map(([label, value, copyable]) => (
        <div key={label} className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 px-3 py-2">
          <dt className="text-xs text-muted-foreground">{label}</dt>
          <dd className="min-w-0 max-w-full">
            {copyable ? <CopyText value={value} /> : <span className="text-xs">{value}</span>}
          </dd>
        </div>
      ))}
    </dl>
  )
}
