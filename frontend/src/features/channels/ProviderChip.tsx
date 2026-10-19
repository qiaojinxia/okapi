import { KeyRound, Plug, Route, ShieldCheck, type LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

/// 平台色按厂商家族分：同家的 API 与订阅同色，类型段区分账号形态。品牌名不翻译。
/// `brand` 是 `public/vendor-icons` 里的单色标志；没有品牌的通用协议用线框图标。
const PLATFORMS: Record<string, { name: string; brand?: string; icon?: LucideIcon; tone: string }> = {
  anthropic: { name: 'Anthropic', brand: 'anthropic', tone: 'bg-orange-500/12 text-orange-700 dark:text-orange-300' },
  anthropic_max: { name: 'Claude', brand: 'claude', tone: 'bg-orange-500/12 text-orange-700 dark:text-orange-300' },
  openai: { name: 'OpenAI', brand: 'openai', tone: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300' },
  codex: { name: 'Codex', brand: 'openai', tone: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300' },
  gemini: { name: 'Gemini', brand: 'gemini', tone: 'bg-blue-500/12 text-blue-700 dark:text-blue-300' },
  vertex: { name: 'Vertex', brand: 'vertexai', tone: 'bg-indigo-500/12 text-indigo-700 dark:text-indigo-300' },
  azure: { name: 'Azure', brand: 'azure', tone: 'bg-sky-500/12 text-sky-700 dark:text-sky-300' },
  bedrock: { name: 'Bedrock', brand: 'bedrock', tone: 'bg-amber-500/14 text-amber-700 dark:text-amber-300' },
  openai_compat: { name: 'Compatible', icon: Plug, tone: 'bg-slate-500/12 text-slate-700 dark:text-slate-300' },
  custom_pass: { name: 'Pass-through', icon: Route, tone: 'bg-zinc-500/12 text-zinc-700 dark:text-zinc-300' },
}

/// 品牌标志按形状遮罩、填当前文字色：跟着胶囊的平台色走，暗色主题不用另配一套。
function BrandMark({ brand, className }: { brand: string; className?: string }) {
  const mask = `url(/vendor-icons/${brand}.svg) center / contain no-repeat`
  return <span aria-hidden className={cn('inline-block shrink-0 bg-current', className)} style={{ mask, WebkitMask: mask }} />
}

const SUBSCRIPTION = new Set(['anthropic_max', 'codex'])

/// 渠道列表的「平台 / 类型」胶囊：一眼分出是哪家、是订阅账号还是 API Key。
export function ProviderChip({ provider, className }: { provider: string; className?: string }) {
  const { t } = useTranslation()
  const platform = PLATFORMS[provider] ?? { name: provider, icon: Plug, tone: 'bg-muted text-muted-foreground' }
  const subscription = SUBSCRIPTION.has(provider)
  const Icon = platform.icon ?? Plug
  const TypeIcon = subscription ? ShieldCheck : KeyRound
  return (
    <span
      className={cn('inline-flex h-6 items-center gap-2 whitespace-nowrap rounded-md px-2 text-xs font-medium', platform.tone, className)}
      title={provider}
    >
      <span className="inline-flex items-center gap-1">
        {platform.brand ? <BrandMark brand={platform.brand} className="h-3.5 w-3.5" /> : <Icon className="h-3.5 w-3.5" aria-hidden />}
        {platform.name}
      </span>
      <span className="inline-flex items-center gap-1 opacity-85">
        <TypeIcon className="h-3 w-3" aria-hidden />
        {subscription ? t('admin:providerTypeSubscription') : t('admin:providerTypeApiKey')}
      </span>
    </span>
  )
}
