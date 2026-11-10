import { Plug, Route, type LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

/// 平台色按厂商家族分：同家的 API 与订阅同色，后半段文字区分账号形态。品牌名不翻译。
/// `brand` 是 public/vendor-icons 里的单色标志；通用协议用线框图标，不伪造品牌。
type Platform = { name: string; tone: string; brand?: string; icon?: LucideIcon }

const PLATFORMS: Record<string, Platform> = {
  anthropic: { name: 'Anthropic', brand: 'anthropic', tone: 'bg-orange-500/12 text-orange-800 dark:text-orange-300' },
  anthropic_max: { name: 'Claude', brand: 'claude', tone: 'bg-orange-500/12 text-orange-800 dark:text-orange-300' },
  openai: { name: 'OpenAI', brand: 'openai', tone: 'bg-emerald-500/12 text-emerald-800 dark:text-emerald-300' },
  codex: { name: 'Codex', brand: 'openai', tone: 'bg-emerald-500/12 text-emerald-800 dark:text-emerald-300' },
  gemini: { name: 'Gemini', brand: 'gemini', tone: 'bg-blue-500/12 text-blue-800 dark:text-blue-300' },
  vertex: { name: 'Vertex', brand: 'vertexai', tone: 'bg-indigo-500/12 text-indigo-800 dark:text-indigo-300' },
  azure: { name: 'Azure', brand: 'azure', tone: 'bg-sky-500/12 text-sky-800 dark:text-sky-300' },
  bedrock: { name: 'Bedrock', brand: 'bedrock', tone: 'bg-amber-500/14 text-amber-800 dark:text-amber-300' },
  openai_compat: { name: 'Compatible', icon: Plug, tone: 'bg-slate-500/12 text-slate-800 dark:text-slate-300' },
  custom_pass: { name: 'Pass-through', icon: Route, tone: 'bg-zinc-500/12 text-zinc-800 dark:text-zinc-300' },
}

const platformOf = (provider: string): Platform =>
  PLATFORMS[provider] ?? { name: provider, icon: Plug, tone: 'bg-muted text-muted-foreground' }

/// 标志按形状遮罩并填当前文字色：品牌、胶囊和暗色主题始终使用同一组语义色。
function BrandMark({ brand, className }: { brand: string; className?: string }) {
  const mask = `url(/vendor-icons/${brand}.svg) center / contain no-repeat`
  return <span aria-hidden className={cn('inline-block shrink-0 bg-current', className)} style={{ mask, WebkitMask: mask }} />
}

/// 列表行首的品牌锚点。只负责识别，不重复承担平台名称；名称与档位仍由下面的胶囊完整表达。
/// `sm` 给列表行：行是两行文字高（40px），标志比它矮一点（36px）才不顶着上下边。
export function ProviderMark({ provider, size = 'md', className }: { provider: string; size?: 'md' | 'sm'; className?: string }) {
  const platform = platformOf(provider)
  const Icon = platform.icon ?? Plug
  const glyph = size === 'sm' ? 'h-[18px] w-[18px]' : 'h-5 w-5'
  return (
    <span
      aria-hidden
      data-provider={provider}
      className={cn(
        'inline-flex shrink-0 items-center justify-center border border-current/10 shadow-[inset_0_1px_0_oklch(1_0_0/0.28)]',
        size === 'sm' ? 'h-9 w-9 rounded-[10px]' : 'h-10 w-10 rounded-xl',
        platform.tone,
        className,
      )}
    >
      {platform.brand
        ? <BrandMark brand={platform.brand} className={glyph} />
        : <Icon className={glyph} strokeWidth={1.8} />}
    </span>
  )
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
/// `compact` 给列表行的第二行：和 12px 的说明文字同一行高，矮一号、方一点，不再是一枚抢眼的大胶囊。
export function ProviderChip({ provider, plans, compact = false, className }: { provider: string; plans?: string[]; compact?: boolean; className?: string }) {
  const { t } = useTranslation()
  const platform = platformOf(provider)
  const known = SUBSCRIPTION.has(provider) && plans !== undefined && plans.length > 0
  const kind = SUBSCRIPTION.has(provider)
    ? known
      ? (plans ?? []).map(planLabel).join(' · ')
      : t('admin:providerTypeSubscription')
    // 列表行里地方紧：「API Key」缩成「API」，订阅档位照写
    : compact ? 'API' : t('admin:providerTypeApiKey')
  return (
    <span
      data-slot="provider-chip"
      className={cn(
        'inline-flex shrink-0 items-center whitespace-nowrap text-[11px] leading-4',
        compact ? 'h-[18px] rounded-[5px] px-1.5 font-medium' : 'h-[22px] rounded-full px-2.5 font-semibold',
        platform.tone,
        className,
      )}
      title={known ? `${provider}\n${t('admin:channelPlanHint')}` : provider}
    >
      {`${platform.name} ${kind}`}
    </span>
  )
}
