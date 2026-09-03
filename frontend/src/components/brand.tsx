import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

/// 品牌标：主色圆角方块 + 三道斜纹（okapi 后腿的条纹）。
/// 纯 SVG 内联、颜色走令牌：暗色下自动跟主色一起变亮，不需要两套图片。
export function BrandMark({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 32 32"
      aria-hidden
      className={cn('h-8 w-8 shrink-0', className)}
      fill="none"
    >
      <defs>
        <linearGradient id="okapi-brand" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" stopColor="var(--color-primary)" />
          <stop offset="100%" stopColor="var(--color-chart-4)" />
        </linearGradient>
      </defs>
      <rect width="32" height="32" rx="9" fill="url(#okapi-brand)" />
      <path
        d="M8 22 L20 8 M12 25 L24 11 M17 26 L26 16"
        stroke="var(--color-primary-foreground)"
        strokeWidth="2.6"
        strokeLinecap="round"
        opacity="0.95"
      />
    </svg>
  )
}

/// 标 + 字：侧栏顶部、登录页、公开页头共用。`compact` 只留标（侧栏折叠成图标栏时）。
export function BrandLockup({
  compact = false,
  size = 'md',
  className,
}: {
  compact?: boolean
  size?: 'md' | 'lg'
  className?: string
}) {
  const { t } = useTranslation()
  return (
    <span className={cn('inline-flex items-center gap-2.5', className)}>
      <BrandMark className={size === 'lg' ? 'h-10 w-10' : 'h-8 w-8'} />
      {!compact && (
        <span
          className={cn(
            'font-semibold tracking-tight',
            size === 'lg' ? 'text-2xl' : 'text-[17px]',
          )}
        >
          {t('common:appName')}
        </span>
      )}
    </span>
  )
}
