import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

/// 平台色按厂商家族分：同家的 API 与订阅同色，后半段文字区分账号形态。品牌名不翻译。
const PLATFORMS: Record<string, { name: string; tone: string }> = {
  anthropic: { name: 'Anthropic', tone: 'bg-orange-500/12 text-orange-800 dark:text-orange-300' },
  anthropic_max: { name: 'Claude', tone: 'bg-orange-500/12 text-orange-800 dark:text-orange-300' },
  openai: { name: 'OpenAI', tone: 'bg-emerald-500/12 text-emerald-800 dark:text-emerald-300' },
  codex: { name: 'Codex', tone: 'bg-emerald-500/12 text-emerald-800 dark:text-emerald-300' },
  gemini: { name: 'Gemini', tone: 'bg-blue-500/12 text-blue-800 dark:text-blue-300' },
  vertex: { name: 'Vertex', tone: 'bg-indigo-500/12 text-indigo-800 dark:text-indigo-300' },
  azure: { name: 'Azure', tone: 'bg-sky-500/12 text-sky-800 dark:text-sky-300' },
  bedrock: { name: 'Bedrock', tone: 'bg-amber-500/14 text-amber-800 dark:text-amber-300' },
  openai_compat: { name: 'Compatible', tone: 'bg-slate-500/12 text-slate-800 dark:text-slate-300' },
  custom_pass: { name: 'Pass-through', tone: 'bg-zinc-500/12 text-zinc-800 dark:text-zinc-300' },
}

const SUBSCRIPTION = new Set(['anthropic_max', 'codex'])

const PLAN_LABELS: Record<string, string> = {
  pro: 'Pro',
  max: 'Max',
  max_5x: 'Max 5x',
  max_20x: 'Max 20x',
  team: 'Team',
  enterprise: 'Enterprise',
  // ChatGPT（Codex）档位，取自用量接口的 plan_type
  free: 'Free',
  go: 'Go',
  plus: 'Plus',
  prolite: 'Pro Lite',
  promax: 'Pro Max',
  business: 'Business',
  edu: 'Edu',
}

export function planLabel(plan: string): string {
  return PLAN_LABELS[plan] ?? plan
}

/// 渠道列表的「平台 / 类型」胶囊：一眼分出是哪家、订阅账号是什么档位还是 API Key。
///
/// 订阅账号写「平台 + 档位」（Claude Pro、Codex Plus）；档位还没探到就写「订阅」，
/// API 渠道写「API Key」。多把 key 档位不同时都列出（Claude Pro · Max 20x）。
export function ProviderChip({ provider, plans, className }: { provider: string; plans?: string[]; className?: string }) {
  const { t } = useTranslation()
  const platform = PLATFORMS[provider] ?? { name: provider, tone: 'bg-muted text-muted-foreground' }
  const known = SUBSCRIPTION.has(provider) && plans !== undefined && plans.length > 0
  const kind = SUBSCRIPTION.has(provider)
    ? known
      ? (plans ?? []).map(planLabel).join(' · ')
      : t('admin:providerTypeSubscription')
    : t('admin:providerTypeApiKey')
  return (
    <span
      className={cn('inline-flex h-5 items-center whitespace-nowrap rounded-md px-2.5 text-xs font-medium', platform.tone, className)}
      title={known ? `${provider}\n${t('admin:channelPlanHint')}` : provider}
    >
      {`${platform.name} ${kind}`}
    </span>
  )
}
