import {
  Asterisk,
  Cloud,
  KeyRound,
  Mountain,
  Plug,
  Route,
  ShieldCheck,
  Sparkle,
  Sparkles,
  SquareTerminal,
  Triangle,
  type LucideIcon,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

/// 平台色按厂商家族分：同家的 API 与订阅同色，靠图标与类型段区分。品牌名不翻译。
const PLATFORMS: Record<string, { name: string; icon: LucideIcon; tone: string }> = {
  anthropic: { name: 'Anthropic', icon: Asterisk, tone: 'bg-orange-500/12 text-orange-700 dark:text-orange-300' },
  anthropic_max: { name: 'Claude', icon: Asterisk, tone: 'bg-orange-500/12 text-orange-700 dark:text-orange-300' },
  openai: { name: 'OpenAI', icon: Sparkles, tone: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300' },
  codex: { name: 'Codex', icon: SquareTerminal, tone: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300' },
  gemini: { name: 'Gemini', icon: Sparkle, tone: 'bg-blue-500/12 text-blue-700 dark:text-blue-300' },
  vertex: { name: 'Vertex', icon: Triangle, tone: 'bg-indigo-500/12 text-indigo-700 dark:text-indigo-300' },
  azure: { name: 'Azure', icon: Cloud, tone: 'bg-sky-500/12 text-sky-700 dark:text-sky-300' },
  bedrock: { name: 'Bedrock', icon: Mountain, tone: 'bg-amber-500/14 text-amber-700 dark:text-amber-300' },
  openai_compat: { name: 'Compatible', icon: Plug, tone: 'bg-slate-500/12 text-slate-700 dark:text-slate-300' },
  custom_pass: { name: 'Pass-through', icon: Route, tone: 'bg-zinc-500/12 text-zinc-700 dark:text-zinc-300' },
}

const SUBSCRIPTION = new Set(['anthropic_max', 'codex'])

/// 渠道列表的「平台 / 类型」胶囊：一眼分出是哪家、是订阅账号还是 API Key。
export function ProviderChip({ provider, className }: { provider: string; className?: string }) {
  const { t } = useTranslation()
  const platform = PLATFORMS[provider] ?? { name: provider, icon: Plug, tone: 'bg-muted text-muted-foreground' }
  const subscription = SUBSCRIPTION.has(provider)
  const Icon = platform.icon
  const TypeIcon = subscription ? ShieldCheck : KeyRound
  return (
    <span
      className={cn('inline-flex h-6 items-center gap-2 whitespace-nowrap rounded-md px-2 text-xs font-medium', platform.tone, className)}
      title={provider}
    >
      <span className="inline-flex items-center gap-1">
        <Icon className="h-3.5 w-3.5" aria-hidden />
        {platform.name}
      </span>
      <span className="inline-flex items-center gap-1 opacity-85">
        <TypeIcon className="h-3 w-3" aria-hidden />
        {subscription ? t('admin:providerTypeSubscription') : t('admin:providerTypeApiKey')}
      </span>
    </span>
  )
}
